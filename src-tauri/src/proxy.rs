//! v0.2 本地代理模块（Local Reverse Proxy）
//!
//! 核心职责：
//! 1. 在本地启动轻量 HTTP 反向代理（默认 127.0.0.1:4040）；
//! 2. 接收来自 Claude Code 的请求，实时流式转发给真实上游（Anthropic 官方或中转）；
//! 3. 增量嗅探 SSE 响应提取 `message.id`，并通过 mpsc 通道通知数据线程执行精确归因与晚到对账；
//! 4. 提供 Claude Code settings.json 的安全注入、自动备份与还原机制。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::core::attr;
use crate::watch::Message;

pub const DEFAULT_PORT: u16 = 4040;
pub const HEALTH_PATH: &str = "/health";

/// 增量 SSE 响应嗅探器：保护跨 TCP chunk 分包切割的 message.id
#[derive(Debug, Default)]
pub struct SseSniffer {
    buffer: Vec<u8>,
    pub captured_id: Option<String>,
    pub done: bool,
}

impl SseSniffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 持续喂入数据块，若嗅探到新的 message.id 则返回
    pub fn feed(&mut self, chunk: &[u8]) -> Option<String> {
        if self.done {
            return None;
        }

        // 避免异常长数据块导致内存溢出
        if self.buffer.len() > 65536 {
            self.buffer.drain(..self.buffer.len().saturating_sub(16384));
        }
        self.buffer.extend_from_slice(chunk);

        // 按行解析
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line_bytes = self.buffer.drain(..=pos).collect::<Vec<u8>>();
            let line = String::from_utf8_lossy(&line_bytes);
            let trimmed = line.trim();

            if let Some(payload) = trimmed.strip_prefix("data:") {
                let payload = payload.trim();
                if payload == "[DONE]" {
                    self.done = true;
                    return None;
                }
                if let Some(id) = Self::extract_id(payload) {
                    self.captured_id = Some(id.clone());
                    self.done = true;
                    return Some(id);
                }
            }
        }

        // 兼容非 SSE 格式（直接返回 JSON 的短请求）
        if let Some(id) = Self::extract_id(&String::from_utf8_lossy(&self.buffer)) {
            self.captured_id = Some(id.clone());
            self.done = true;
            return Some(id);
        }

        None
    }

    fn extract_id(text: &str) -> Option<String> {
        if !text.contains("\"id\"") {
            return None;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
            // 模式 1: message_start 包含的 message.id
            if let Some(id) = val
                .get("message")
                .and_then(|m| m.get("id"))
                .and_then(|i| i.as_str())
            {
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
            // 模式 2: 顶层 id
            if let Some(id) = val.get("id").and_then(|i| i.as_str()) {
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
        None
    }
}

/// 本地持久化配置 (~/.token-watcher/proxy_config.json)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyConfig {
    pub port: Option<u16>,
    /// 注入前记录的原始用户上游地址（防自环与中转走样）
    pub saved_upstream: Option<String>,
}

/// 注入 settings.json 后的恢复标记 (~/.token-watcher/proxy_injected.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectedMarker {
    pub original_base_url: Option<String>,
    pub injected_at: i64,
}

pub struct ProxyState {
    pub bound_port: u16,
    pub config: Mutex<ProxyConfig>,
    pub data_tx: mpsc::Sender<Message>,
    pub http_client: reqwest::Client,
    pub claude_settings_path: PathBuf,
}

impl ProxyState {
    pub fn resolve_upstream(&self, profile_prefix: Option<&str>) -> String {
        // 1. 如果路径有显式档案前缀 /p/<id>，未来可按 profile 查询，当前默认转给 saved/settings
        if let Some(_p) = profile_prefix {
            // 保留前缀扩展位
        }

        // 2. 优先使用注入前备份的 saved_upstream
        if let Ok(cfg) = self.config.lock() {
            if let Some(saved) = &cfg.saved_upstream {
                let trimmed = saved.trim();
                if !trimmed.is_empty() && !is_self_forward_loop(trimmed, self.bound_port) {
                    return trimmed.to_string();
                }
            }
        }

        // 3. 动态读取 settings.json 中的 ANTHROPIC_BASE_URL
        if let Ok(content) = std::fs::read_to_string(&self.claude_settings_path) {
            if let Ok(root) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(url) = root
                    .get("env")
                    .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                    .and_then(|v| v.as_str())
                {
                    let trimmed = url.trim();
                    if !trimmed.is_empty() && !is_self_forward_loop(trimmed, self.bound_port) {
                        return trimmed.to_string();
                    }
                }
            }
        }

        // 4. 默认回退官方
        "https://api.anthropic.com".to_string()
    }
}

fn is_loopback_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("[::1]")
}

fn is_self_forward_loop(upstream: &str, bound_port: u16) -> bool {
    if !is_loopback_url(upstream) {
        return false;
    }
    // 检查端口是否等于自身
    let port_str = format!(":{bound_port}");
    upstream.contains(&port_str)
}

/// 检查 Host 头防 DNS Rebinding
fn is_valid_host(host: &str) -> bool {
    let bare = host
        .split(':')
        .next()
        .unwrap_or("")
        .trim_matches(&['[', ']'][..]);
    bare == "127.0.0.1" || bare == "localhost" || bare == "::1" || bare.is_empty()
}

pub fn create_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route(HEALTH_PATH, get(health_handler))
        .fallback(any(proxy_handler))
        .with_state(state)
}

async fn health_handler(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    let payload = serde_json::json!({
        "status": "ok",
        "version": "0.2.1",
        "port": state.bound_port,
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        payload.to_string(),
    )
}

async fn proxy_handler(
    State(state): State<Arc<ProxyState>>,
    req: Request,
) -> Result<Response, (StatusCode, String)> {
    // 1. Host 校验防 DNS Rebinding
    let host_val = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !is_valid_host(host_val) {
        return Err((
            StatusCode::FORBIDDEN,
            "DNS Rebinding Protection: Host header invalid".to_string(),
        ));
    }

    let uri = req.uri();
    let original_path = uri.path();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();

    // 2. 解析档案前缀：/p/<id>/v1/messages -> profile_id, /v1/messages
    let (profile_id_opt, forward_path) = parse_profile_route(original_path);

    // 3. 确定真实上游
    let upstream_base = state.resolve_upstream(profile_id_opt.as_deref());

    // 4. 自环检查
    if is_self_forward_loop(&upstream_base, state.bound_port) {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "Proxy Error: Loopback self-forward detected to port {}",
                state.bound_port
            ),
        ));
    }

    // 5. 提取认证凭据与指纹
    let auth_header = req
        .headers()
        .get("x-api-key")
        .or_else(|| req.headers().get(header::AUTHORIZATION))
        .and_then(|v| v.to_str().ok());

    let (fingerprint_opt, endpoint) = if let Some(secret) = auth_header {
        let clean_secret = secret.strip_prefix("Bearer ").unwrap_or(secret).trim();
        let fp = attr::fingerprint(clean_secret);
        let ep = attr::normalize_endpoint(&upstream_base).unwrap_or_else(|| upstream_base.clone());
        (Some(fp), ep)
    } else {
        let ep = attr::normalize_endpoint(&upstream_base).unwrap_or_else(|| upstream_base.clone());
        (None, ep)
    };

    // 6. 构造转发请求
    let target_url = format!(
        "{}{}{}",
        upstream_base.trim_end_matches('/'),
        forward_path,
        query
    );

    let method = req.method().clone();
    let mut outbound = state.http_client.request(method, &target_url);

    // 逐跳头清理（RFC 7230）
    let hop_by_hop = [
        "connection",
        "keep-alive",
        "proxy-connection",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
        "host",
    ];

    for (name, val) in req.headers() {
        let name_str = name.as_str().to_ascii_lowercase();
        if !hop_by_hop.contains(&name_str.as_str()) {
            outbound = outbound.header(name, val);
        }
    }

    // 设置 upstream 的 Host 头与禁用压缩
    if let Ok(parsed_url) = reqwest::Url::parse(&target_url) {
        if let Some(host) = parsed_url.host_str() {
            outbound = outbound.header(header::HOST, host);
        }
    }
    outbound = outbound.header(header::ACCEPT_ENCODING, "identity");

    // 流式透传请求体
    let body_stream = req.into_body().into_data_stream();
    outbound = outbound.body(reqwest::Body::wrap_stream(body_stream));

    // 7. 发送请求
    let upstream_res = match outbound.send().await {
        Ok(res) => res,
        Err(e) => {
            eprintln!("[token-watcher proxy] 请求上游失败: {e}");
            return Err((
                StatusCode::BAD_GATEWAY,
                format!("Failed to reach upstream {target_url}: {e}"),
            ));
        }
    };

    let status = StatusCode::from_u16(upstream_res.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut response_builder = Response::builder().status(status);

    for (k, v) in upstream_res.headers() {
        let k_str = k.as_str().to_ascii_lowercase();
        if !hop_by_hop.contains(&k_str.as_str()) {
            response_builder = response_builder.header(k, v);
        }
    }

    // 8. 流式透传响应并增量嗅探 message.id
    let mut sniffer = SseSniffer::new();
    let data_tx = state.data_tx.clone();
    let parsed_profile_id = profile_id_opt.and_then(|s| s.parse::<i64>().ok());

    let stream = upstream_res
        .bytes_stream()
        .map(move |chunk_res| match chunk_res {
            Ok(bytes) => {
                if !sniffer.done {
                    if let Some(msg_id) = sniffer.feed(&bytes) {
                        let _ = data_tx.send(Message::ProxyAttribution {
                            message_id: msg_id,
                            profile_id: parsed_profile_id,
                            endpoint: endpoint.clone(),
                            fingerprint: fingerprint_opt.clone(),
                        });
                    }
                }
                Ok::<_, std::io::Error>(bytes)
            }
            Err(e) => Err(std::io::Error::other(e)),
        });

    let body = Body::from_stream(stream);
    response_builder
        .body(body)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn parse_profile_route(path: &str) -> (Option<String>, String) {
    if let Some(rest) = path.strip_prefix("/p/") {
        if let Some((profile_id, forward)) = rest.split_once('/') {
            return (Some(profile_id.to_string()), format!("/{forward}"));
        }
    }
    (None, path.to_string())
}

// ---------------------------------------------------------------------------
// settings.json 注入与还原辅助
// ---------------------------------------------------------------------------

pub fn get_proxy_config_path() -> Option<PathBuf> {
    home::home_dir().map(|h| h.join(".token-watcher").join("proxy_config.json"))
}

pub fn get_injected_marker_path() -> Option<PathBuf> {
    home::home_dir().map(|h| h.join(".token-watcher").join("proxy_injected.json"))
}

pub fn load_proxy_config() -> ProxyConfig {
    let Some(path) = get_proxy_config_path() else {
        return ProxyConfig::default();
    };
    if let Ok(content) = std::fs::read_to_string(path) {
        if let Ok(cfg) = serde_json::from_str::<ProxyConfig>(&content) {
            return cfg;
        }
    }
    ProxyConfig::default()
}

pub fn save_proxy_config(cfg: &ProxyConfig) -> Result<(), std::io::Error> {
    let Some(path) = get_proxy_config_path() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_string_pretty(cfg)?;
    std::fs::write(path, data)
}

/// 将 Claude Code 的 settings.json 配置为走本地代理
pub fn inject_settings_proxy(port: u16, settings_path: &Path) -> Result<(), String> {
    let content = if settings_path.exists() {
        std::fs::read_to_string(settings_path).map_err(|e| e.to_string())?
    } else {
        "{}".to_string()
    };

    let mut root: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).unwrap_or_default();

    let env_val = root.entry("env").or_insert_with(|| serde_json::json!({}));
    let env_obj = env_val
        .as_object_mut()
        .ok_or_else(|| "settings.json 中 env 字段不是对象".to_string())?;

    let original_url = env_obj
        .get("ANTHROPIC_BASE_URL")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // 如果原来就已经是 loopback，不视作有效上游
    let saved_upstream = original_url.filter(|u| !is_loopback_url(u));

    // 记录标记
    if let Some(marker_path) = get_injected_marker_path() {
        if let Some(dir) = marker_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let marker = InjectedMarker {
            original_base_url: saved_upstream.clone(),
            injected_at: now,
        };
        let _ = std::fs::write(
            marker_path,
            serde_json::to_string(&marker).unwrap_or_default(),
        );
    }

    // 持久化到 proxy_config.json
    let mut cfg = load_proxy_config();
    if saved_upstream.is_some() {
        cfg.saved_upstream = saved_upstream;
    }
    cfg.port = Some(port);
    let _ = save_proxy_config(&cfg);

    // 更新 settings.json
    env_obj.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        serde_json::Value::String(format!("http://127.0.0.1:{port}")),
    );

    let updated = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;

    // 原子写入：先写临时文件再 rename
    let tmp_path = settings_path.with_extension("tmp");
    if let Some(dir) = settings_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&tmp_path, updated).map_err(|e| e.to_string())?;
    std::fs::rename(tmp_path, settings_path).map_err(|e| e.to_string())?;

    Ok(())
}

/// 还原 settings.json 为注入前的原始配置
pub fn restore_settings_proxy(settings_path: &Path) -> Result<bool, String> {
    let Some(marker_path) = get_injected_marker_path() else {
        return Ok(false);
    };
    if !marker_path.exists() {
        return Ok(false);
    }

    let marker: Option<InjectedMarker> = std::fs::read_to_string(&marker_path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok());

    if settings_path.exists() {
        if let Ok(content) = std::fs::read_to_string(settings_path) {
            if let Ok(mut root) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&content)
            {
                if let Some(env_obj) = root.get_mut("env").and_then(|v| v.as_object_mut()) {
                    match marker.and_then(|m| m.original_base_url) {
                        Some(orig) => {
                            env_obj.insert(
                                "ANTHROPIC_BASE_URL".to_string(),
                                serde_json::Value::String(orig),
                            );
                        }
                        None => {
                            env_obj.remove("ANTHROPIC_BASE_URL");
                        }
                    }
                    if let Ok(updated) = serde_json::to_string_pretty(&root) {
                        let tmp = settings_path.with_extension("restore.tmp");
                        if std::fs::write(&tmp, updated).is_ok() {
                            let _ = std::fs::rename(tmp, settings_path);
                        }
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_file(marker_path);

    // 清空 saved_upstream
    let mut cfg = load_proxy_config();
    cfg.saved_upstream = None;
    let _ = save_proxy_config(&cfg);

    Ok(true)
}

/// 启动代理服务器监听
pub async fn start_proxy_server(
    data_tx: mpsc::Sender<Message>,
    claude_settings_path: PathBuf,
    preferred_port: u16,
) -> Result<(Arc<ProxyState>, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
    // 寻找可用端口（从 preferred_port 开始递增重试）
    let mut bound_listener = None;
    let mut bound_port = preferred_port;

    for p in preferred_port..preferred_port + 20 {
        let addr = SocketAddr::from(([127, 0, 0, 1], p));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                bound_listener = Some(listener);
                bound_port = p;
                break;
            }
            Err(_) => continue,
        }
    }

    let listener = bound_listener.ok_or_else(|| {
        format!(
            "无法在 127.0.0.1:{}-{} 找到空闲端口",
            preferred_port,
            preferred_port + 19
        )
    })?;

    let http_client = reqwest::Client::builder()
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(10))
        .build()?;

    let config = load_proxy_config();

    let state = Arc::new(ProxyState {
        bound_port,
        config: Mutex::new(config),
        data_tx,
        http_client,
        claude_settings_path,
    });

    let app = create_router(state.clone());

    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[token-watcher proxy] 服务器异常退出: {e}");
        }
    });

    Ok((state, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("tw-proxy-test-{nanos}-{id}"));
            std::fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn sse_sniffer_extracts_id_across_split_chunks() {
        let mut sniffer = SseSniffer::new();
        // 模拟 TCP 拆包：数据在 "msg_01" 中间被切开
        let chunk1 =
            b"event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_";
        let chunk2 = b"01TESTSPLIT123\",\"type\":\"message\"}}\r\n\r\n";

        assert_eq!(sniffer.feed(chunk1), None);
        assert_eq!(sniffer.feed(chunk2), Some("msg_01TESTSPLIT123".to_string()));
        assert_eq!(sniffer.captured_id, Some("msg_01TESTSPLIT123".to_string()));
        assert!(sniffer.done);
    }

    #[test]
    fn sse_sniffer_handles_relay_chatcmpl_ids() {
        let mut sniffer = SseSniffer::new();
        let payload =
            b"data: {\"id\":\"chatcmpl-998877\",\"object\":\"chat.completion.chunk\"}\n\n";
        assert_eq!(sniffer.feed(payload), Some("chatcmpl-998877".to_string()));
    }

    #[test]
    fn sse_sniffer_extracts_from_plain_json() {
        let mut sniffer = SseSniffer::new();
        let json = b"{\"id\":\"msg_direct_json_999\",\"usage\":{\"input_tokens\":10}}";
        assert_eq!(sniffer.feed(json), Some("msg_direct_json_999".to_string()));
    }

    #[test]
    fn parse_profile_route_works() {
        assert_eq!(
            parse_profile_route("/p/deepseek/v1/messages"),
            (Some("deepseek".to_string()), "/v1/messages".to_string())
        );
        assert_eq!(
            parse_profile_route("/v1/messages"),
            (None, "/v1/messages".to_string())
        );
    }

    #[test]
    fn loopback_detection_and_self_forward_protection() {
        assert!(is_loopback_url("http://127.0.0.1:4040"));
        assert!(is_loopback_url("http://localhost:8080"));
        assert!(!is_loopback_url("https://api.anthropic.com"));
        assert!(!is_loopback_url("https://api.deepseek.com"));

        assert!(is_self_forward_loop("http://127.0.0.1:4040", 4040));
        assert!(!is_self_forward_loop("http://127.0.0.1:5050", 4040));
    }

    #[test]
    fn settings_injection_and_restoration_cycle() {
        let tmp_dir = TempDir::new();
        let settings_file = tmp_dir.path().join("settings.json");

        // 初始写入真实中转配置
        let initial = r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.deepseek.com","ANTHROPIC_AUTH_TOKEN":"test-token"}}"#;
        std::fs::write(&settings_file, initial).unwrap();

        // 注入代理 4040
        inject_settings_proxy(4040, &settings_file).unwrap();

        let injected_content = std::fs::read_to_string(&settings_file).unwrap();
        assert!(injected_content.contains("http://127.0.0.1:4040"));

        // 还原配置
        let restored = restore_settings_proxy(&settings_file).unwrap();
        assert!(restored);

        let restored_content = std::fs::read_to_string(&settings_file).unwrap();
        assert!(restored_content.contains("https://api.deepseek.com"));
    }
}
