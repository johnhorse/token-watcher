//! t10_proxy: v0.2 本地代理模式端到端与晚到对账集成测试

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use axum::response::sse::{Event as AxumEvent, Sse};
use axum::routing::post;
use axum::Router;
use futures_util::stream;
use token_watcher_lib::core::parse::UsageRecord;
use token_watcher_lib::core::store::Store;
use token_watcher_lib::proxy;
use token_watcher_lib::watch::Message;

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
        let path = std::env::temp_dir().join(format!("tw-proxy-itest-{nanos}-{id}"));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn sample_usage_record(record_id: &str, ts_ms: i64) -> UsageRecord {
    UsageRecord {
        record_id: record_id.to_string(),
        session_id: "test-sess-1".to_string(),
        timestamp_ms: ts_ms,
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 200,
        cache_creation_tokens: 0,
        model: "claude-3-7-sonnet".to_string(),
        entrypoint: "cli".to_string(),
        is_sidechain: false,
        cwd: Some("/test".to_string()),
        cc_version: Some("2.1.303".to_string()),
    }
}

#[tokio::test]
async fn test_proxy_and_late_reconciliation_flow() {
    let temp = TempDir::new();
    let db_path = temp.path().join("proxy_test.sqlite3");
    let mut store = Store::open(&db_path).unwrap();

    // 1. 建立两个不同档案
    let p1_id = store
        .ensure_profile("https://api.deepseek.com", Some("dp-fp-1"))
        .unwrap();
    let p2_id = store
        .ensure_profile("https://api.anthropic.com", Some("oauth-sub"))
        .unwrap();
    assert_ne!(p1_id, p2_id);

    // 2. 场景 A: 先有代理归因记录，后有 JSONL 落库
    let msg_a = "msg_01_proxy_first";
    store
        .record_proxy_attribution(msg_a, p1_id, Some("https://api.deepseek.com"))
        .unwrap();

    let rec_a = sample_usage_record(msg_a, 1700000000000);
    store.upsert_records(&[rec_a]).unwrap();

    // 验证 msg_a 直接被归因至 p1_id，且 attribution_source 为 'proxy'
    let (pid, source) = store.record_attribution_details(msg_a).unwrap().unwrap();
    assert_eq!(pid, Some(p1_id));
    assert_eq!(source, "proxy");

    // 3. 场景 B: 晚到对账 —— 先落库（此时为 timeline/Unknown），后到达代理归因消息
    let msg_b = "msg_02_late_proxy";
    let rec_b = sample_usage_record(msg_b, 1700000001000);
    // 此时无代理记录，落库默认为 timeline (profile_id = None)
    store.upsert_records(&[rec_b]).unwrap();

    let (pid_before, source_before) = store.record_attribution_details(msg_b).unwrap().unwrap();
    assert_eq!(pid_before, None);
    assert_eq!(source_before, "timeline");

    // 晚到：收到代理归因消息
    store
        .record_proxy_attribution(msg_b, p2_id, Some("https://api.anthropic.com"))
        .unwrap();
    let changed = store.reconcile_usage_attribution(msg_b, p2_id).unwrap();
    assert_eq!(changed, 1, "晚到对账成功更新了 1 行");

    let (pid_after, source_after) = store.record_attribution_details(msg_b).unwrap().unwrap();
    assert_eq!(pid_after, Some(p2_id));
    assert_eq!(source_after, "proxy");

    // 4. 场景 C: 手动修正优先级高于代理归因（P0-3 加固）
    let msg_c = "msg_03_manual_priority";
    let rec_c = sample_usage_record(msg_c, 1700000002000);
    store.upsert_records(&[rec_c]).unwrap();

    // 用户在 UI 手动将 msg_c 改为 p1_id
    store.mark_record_manual(msg_c, Some(p1_id)).unwrap();

    // 代理归因晚到试图将其改为 p2_id
    store
        .record_proxy_attribution(msg_c, p2_id, Some("https://api.anthropic.com"))
        .unwrap();
    let changed_manual = store.reconcile_usage_attribution(msg_c, p2_id).unwrap();
    assert_eq!(changed_manual, 0, "手动修正记录不应被代理覆盖");

    let (pid_manual, source_manual) = store.record_attribution_details(msg_c).unwrap().unwrap();
    assert_eq!(pid_manual, Some(p1_id), "保持用户手动指定的值");
    assert_eq!(source_manual, "manual");
}

#[tokio::test]
async fn test_mock_upstream_forwarding_and_sniffing() {
    // 1. 启动一个 Mock Anthropic 上游服务
    let mock_app = Router::new().route(
        "/v1/messages",
        post(|| async {
            let events = vec![
                Ok::<_, std::convert::Infallible>(
                    AxumEvent::default()
                        .event("message_start")
                        .data(r#"{"type":"message_start","message":{"id":"msg_itest_mock_888","type":"message","model":"mock"}}"#),
                ),
                Ok(AxumEvent::default().event("content_block_delta").data(r#"{"delta":{"text":"hello"}}"#)),
                Ok(AxumEvent::default().event("message_stop").data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );

    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = upstream_listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(upstream_listener, mock_app).await.unwrap();
    });

    let temp = TempDir::new();
    let settings_file = temp.path().join("settings.json");
    // settings.json 指向我们的 mock 上游
    let settings_json = format!(
        r#"{{"env":{{"ANTHROPIC_BASE_URL":"http://127.0.0.1:{upstream_port}","ANTHROPIC_AUTH_TOKEN":"mock-secret"}}}}"#
    );
    fs::write(&settings_file, settings_json).unwrap();

    let (tx, rx) = mpsc::channel();

    // 2. 启动代理服务器（preferred_port 使用 0 让系统动态找空闲端口段）
    let (proxy_state, _join) = proxy::start_proxy_server(tx, settings_file, 4140)
        .await
        .unwrap();

    let proxy_port = proxy_state.bound_port;

    // 3. 客户端向本地代理发请求
    let client = reqwest::Client::new();
    let res = client
        .post(format!("http://127.0.0.1:{proxy_port}/v1/messages"))
        .header("x-api-key", "mock-secret")
        .body(r#"{"model":"claude-3-5","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let body_text = res.text().await.unwrap();
    assert!(body_text.contains("msg_itest_mock_888"));

    // 4. 验证代理通过 mpsc 通道发送了 Attribution 消息
    let received_msg = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    match received_msg {
        Message::ProxyAttribution {
            message_id,
            profile_id,
            endpoint,
            fingerprint,
        } => {
            assert_eq!(message_id, "msg_itest_mock_888");
            assert_eq!(profile_id, None);
            assert_eq!(endpoint, format!("http://127.0.0.1:{upstream_port}"));
            assert_eq!(
                fingerprint,
                Some(token_watcher_lib::core::attr::fingerprint("mock-secret"))
            );
        }
        other => panic!(
            "预期收到 ProxyAttribution，实收: {:?}",
            std::mem::discriminant(&other)
        ),
    }
}
