//! T2：订阅归因语义层
//!
//! 把 `~/.claude/settings.json` 的 env 快照转成「订阅档案时间线」，并回答
//! 「某条 usage 在归因上属于哪个档案」。契约见
//! docs/data-source/claude-settings-json.md（D20/D23）。
//!
//! 核心原则：归因不可信就 Unknown（`None`），宁缺勿错。
//! 本模块是纯逻辑：不碰文件系统、不碰数据库，持久化由 store 层负责。

use sha2::{Digest, Sha256};

/// 切换宽限期：新档案区间开头这段时间内到达的 usage 归 Unknown。
/// 理由：切换瞬间仍在运行的会话可能继续走旧中转（契约「时间线语义」）。
/// 默认 300s，与激活会话阈值同源；OQ4 允许按体感调整。
pub const GRACE_MS: i64 = 300_000;

/// 档案持久 ID（对应 profiles.id）
pub type ProfileId = i64;

/// 订阅档案：身份 = (endpoint, credential_fingerprint) 组合。
/// `credential_fingerprint` 为 SHA-256 前 8 位 hex，鉴权原文永不入库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: ProfileId,
    pub endpoint: String,
    pub credential_fingerprint: Option<String>,
    pub display_name: String,
}

/// 时间线区间，半开 `[start_ms, end_ms)`；`end_ms = None` 表示仍在使用。
/// `id` 为数据库行 id，尚未持久化的新区间为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interval {
    pub id: Option<i64>,
    pub profile_id: ProfileId,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

/// 内存中的档案时间线。store 层从数据库加载后交给 [`apply_snapshot`] 决策，
/// 再把 [`ApplyOutcome`] 落库。
#[derive(Debug, Clone)]
pub struct Timeline {
    profiles: Vec<Profile>,
    intervals: Vec<Interval>,
    next_profile_id: ProfileId,
}

impl Default for Timeline {
    fn default() -> Self {
        Timeline {
            profiles: Vec::new(),
            intervals: Vec::new(),
            next_profile_id: 1,
        }
    }
}

impl Timeline {
    /// 从已持久化数据重建。区间按开始时间排序；新档案 ID 从现有最大值递增。
    pub fn new(profiles: Vec<Profile>, mut intervals: Vec<Interval>) -> Self {
        intervals.sort_by_key(|iv| (iv.start_ms, iv.id.unwrap_or(i64::MAX)));
        let next_profile_id = profiles.iter().map(|p| p.id).max().unwrap_or(0) + 1;
        Timeline {
            profiles,
            intervals,
            next_profile_id,
        }
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn intervals(&self) -> &[Interval] {
        &self.intervals
    }

    /// 当前打开的区间（至多一个）
    pub fn open_interval(&self) -> Option<&Interval> {
        self.intervals.iter().rev().find(|iv| iv.end_ms.is_none())
    }

    fn profile(&self, id: ProfileId) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// 按 (endpoint, fingerprint) 查找档案，不存在则分配新持久 ID。
    /// 返回档案副本与「是否新建」。
    fn ensure_profile(&mut self, endpoint: &str, fingerprint: Option<&str>) -> (Profile, bool) {
        if let Some(existing) = self
            .profiles
            .iter()
            .find(|p| p.endpoint == endpoint && p.credential_fingerprint.as_deref() == fingerprint)
        {
            return (existing.clone(), false);
        }
        let profile = Profile {
            id: self.next_profile_id,
            endpoint: endpoint.to_string(),
            credential_fingerprint: fingerprint.map(str::to_string),
            display_name: default_display_name(endpoint),
        };
        self.next_profile_id += 1;
        self.profiles.push(profile.clone());
        (profile, true)
    }

    /// 关闭当前打开区间（end_ms = at_ms），返回被关闭的区间副本
    fn close_open(&mut self, at_ms: i64) -> Option<Interval> {
        let idx = self.intervals.iter().rposition(|iv| iv.end_ms.is_none())?;
        let interval = &mut self.intervals[idx];
        // 时钟回拨等异常下避免产生 end < start 的非法区间
        interval.end_ms = Some(at_ms.max(interval.start_ms));
        Some(interval.clone())
    }

    fn open_interval_at(&mut self, profile_id: ProfileId, at_ms: i64) -> Interval {
        let interval = Interval {
            id: None,
            profile_id,
            start_ms: at_ms,
            end_ms: None,
        };
        self.intervals.push(interval.clone());
        interval
    }
}

/// settings.json 快照解析结果（D20 诚实降级的第一道闸）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsSnapshot {
    /// 坏 JSON（可能读到半截）：保持上一个已知态，不产生时间线事件
    Unreadable,
    /// 有效 JSON 但无可用 BASE_URL：该时刻无归因信息（Unknown）
    NoAttribution,
    /// 可归因配置
    Profile {
        endpoint: String,
        /// 鉴权键存在时的 SHA-256 前 8 位 hex；不存在为 None
        fingerprint: Option<String>,
    },
}

/// 解析 settings.json 文本。鉴权键优先 `ANTHROPIC_AUTH_TOKEN`，
/// 空串视为未设置并回退 `ANTHROPIC_API_KEY`（R2-3）。
pub fn parse_settings(json_str: &str) -> SettingsSnapshot {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return SettingsSnapshot::Unreadable;
    };
    let Some(env) = root.get("env").and_then(|v| v.as_object()) else {
        return SettingsSnapshot::NoAttribution;
    };
    let Some(base_url) = env.get("ANTHROPIC_BASE_URL").and_then(|v| v.as_str()) else {
        return SettingsSnapshot::NoAttribution;
    };
    let Some(endpoint) = normalize_endpoint(base_url) else {
        return SettingsSnapshot::NoAttribution;
    };
    if let Some((_scheme, rest)) = endpoint.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if let Some((host, _port)) = parse_host_port(authority) {
            if host == "127.0.0.1" || host == "localhost" || host == "::1" {
                return SettingsSnapshot::NoAttribution;
            }
        }
    }
    let token = ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"]
        .iter()
        .find_map(|key| {
            env.get(*key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        });
    SettingsSnapshot::Profile {
        endpoint,
        fingerprint: token.map(fingerprint),
    }
}

/// 解析 Codex 的 config.toml，生成可归因 Profile（若配置了 custom.base_url 则为该 endpoint；若为 openai 则为 api.openai.com）
pub fn parse_codex_config(toml_str: &str) -> SettingsSnapshot {
    let mut model_provider = String::new();
    let mut current_section = String::new();
    let mut custom_base_url: Option<String> = None;
    let mut custom_bearer_token: Option<String> = None;

    for line in toml_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = trimmed[1..trimmed.len() - 1].trim().to_string();
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            let val = v.trim().trim_matches('"').trim_matches('\'');
            if current_section.is_empty() {
                if key == "model_provider" {
                    model_provider = val.to_string();
                }
            } else if current_section == "model_providers.custom" {
                if key == "base_url" {
                    custom_base_url = Some(val.to_string());
                } else if key == "experimental_bearer_token" {
                    custom_bearer_token = Some(val.to_string());
                }
            }
        }
    }

    if model_provider == "custom" || (model_provider.is_empty() && custom_base_url.is_some()) {
        if let Some(base_url) = custom_base_url {
            if let Some(endpoint) = normalize_endpoint(&base_url) {
                return SettingsSnapshot::Profile {
                    endpoint,
                    fingerprint: custom_bearer_token.as_deref().map(fingerprint),
                };
            }
        }
    }

    SettingsSnapshot::Profile {
        endpoint: "https://api.openai.com".to_string(),
        fingerprint: None,
    }
}

/// 时间线决策：把一次 settings 快照应用到时间线。
///
/// - 坏 JSON：不产生任何变化；
/// - 组合未变：不产生任何变化；
/// - 组合变化：关闭旧区间、必要时创建档案、开启新区间；
/// - `NoAttribution`：关闭旧区间，不开启新区间（Unknown 时段）。
pub fn apply_snapshot(
    timeline: &mut Timeline,
    snapshot: &SettingsSnapshot,
    at_ms: i64,
) -> ApplyOutcome {
    let target = match snapshot {
        SettingsSnapshot::Unreadable => return ApplyOutcome::Unreadable,
        SettingsSnapshot::NoAttribution => None,
        SettingsSnapshot::Profile {
            endpoint,
            fingerprint,
        } => Some((endpoint.as_str(), fingerprint.as_deref())),
    };

    let current = timeline.open_interval().and_then(|iv| {
        timeline
            .profile(iv.profile_id)
            .map(|p| (p.endpoint.clone(), p.credential_fingerprint.clone()))
    });
    let same = match (&current, &target) {
        (None, None) => true,
        (Some((ce, cf)), Some((te, tf))) => ce == te && cf.as_deref() == *tf,
        _ => false,
    };
    if same {
        return ApplyOutcome::NoChange;
    }

    let closed_interval = timeline.close_open(at_ms);
    let (created_profile, opened_interval) = match target {
        None => (None, None),
        Some((endpoint, fingerprint)) => {
            let (profile, created) = timeline.ensure_profile(endpoint, fingerprint);
            let interval = timeline.open_interval_at(profile.id, at_ms);
            (created.then_some(profile), Some(interval))
        }
    };
    ApplyOutcome::Changed {
        created_profile,
        closed_interval,
        opened_interval,
    }
}

/// 一次快照应用的结果，store 层据此落库
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// 坏 JSON：保持上一个已知态
    Unreadable,
    /// 快照与当前归属一致，无时间线变化
    NoChange,
    /// 时间线发生变化（字段按需持久化）
    Changed {
        /// 新建档案（需要 INSERT profiles）
        created_profile: Option<Profile>,
        /// 被关闭的旧区间（需要 UPDATE end_ms）
        closed_interval: Option<Interval>,
        /// 新开的区间（需要 INSERT profile_timeline）
        opened_interval: Option<Interval>,
    },
}

/// 归因查询：`ts_ms` 时刻的 usage 属于哪个档案。
///
/// 区间开头 [`GRACE_MS`] 内、区间之外、时间线为空 → `None`（Unknown）。
/// 区间为半开 `[start, end)`，宽限期只影响区间起点。
pub fn attribute_at(timeline: &Timeline, ts_ms: i64) -> Option<ProfileId> {
    timeline.intervals.iter().rev().find_map(|iv| {
        if ts_ms < iv.start_ms.saturating_add(GRACE_MS) {
            return None;
        }
        if iv.end_ms.is_some_and(|end| ts_ms >= end) {
            return None;
        }
        Some(iv.profile_id)
    })
}

/// 规范化 endpoint：`scheme://host[:port]`。
/// 默认端口（http 80 / https 443）省略，host 小写，path/query/fragment 丢弃。
/// 解析失败返回 `None`（该快照按 Unknown 处理）。
pub fn normalize_endpoint(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.trim().to_ascii_lowercase();
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let (host, port) = parse_host_port(authority)?;

    let default_port = match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    let port = port.filter(|p| Some(*p) != default_port);

    let host = if host.contains(':') {
        format!("[{host}]") // IPv6 字面量
    } else {
        host
    };
    Some(match port {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    })
}

/// 解析 authority 为 (小写 host, 端口)。支持 `[IPv6]:port`，忽略 userinfo。
fn parse_host_port(authority: &str) -> Option<(String, Option<u16>)> {
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    if host_port.is_empty() {
        return None;
    }
    let (host, port_str, bracketed) = if let Some(rest) = host_port.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &rest[..end];
        if host.is_empty() {
            return None;
        }
        let after = &rest[end + 1..];
        let port = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(':')?)
        };
        (host.to_ascii_lowercase(), port, true)
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port)) => (host.to_ascii_lowercase(), Some(port), false),
            None => (host_port.to_ascii_lowercase(), None, false),
        }
    };
    if host.is_empty()
        || (!bracketed && host.contains(':'))
        || host.chars().any(char::is_whitespace)
    {
        return None;
    }
    let port = match port_str {
        None => None,
        Some(p) => Some(p.parse::<u16>().ok().filter(|&p| p > 0)?),
    };
    Some((host, port))
}

/// 鉴权键指纹：SHA-256 前 8 位 hex（4 字节）。鉴权原文永不持久化。
pub fn fingerprint(secret: &str) -> String {
    if secret.starts_with("sk-ant-oat") {
        return "oauth-sub".to_string();
    }
    let digest = Sha256::digest(secret.as_bytes());
    let mut out = String::with_capacity(8);
    for byte in &digest[..4] {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 常见订阅品牌的默认显示名。匹配键 = host 去掉 `api.` 前缀后的首段（精确相等，
/// 不做子串匹配），未命中回退「首段首字母大写」规则。用户可改名（T6）。
const BRAND_DISPLAY_NAMES: &[(&str, &str)] = &[
    ("deepseek", "DeepSeek"),
    ("moonshot", "Moonshot"),
    ("kimi", "Kimi"),
    ("openrouter", "OpenRouter"),
    ("bigmodel", "BigModel"),
    ("siliconflow", "SiliconFlow"),
    ("dashscope", "DashScope"),
];

/// 默认显示名：`api.` 前缀去掉、首段首字母大写；
/// `api.anthropic.com` 特判「Anthropic 官方」；环回地址显示「本地代理 (port)」；
/// 常见品牌走 [`BRAND_DISPLAY_NAMES`] 保留官方大小写。用户之后可改（T6）。
pub fn default_display_name(endpoint: &str) -> String {
    let Some((_scheme, rest)) = endpoint.split_once("://") else {
        return endpoint.to_string();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let Some((host, port)) = parse_host_port(authority) else {
        return endpoint.to_string();
    };
    if host == "127.0.0.1" || host == "localhost" {
        return match port {
            Some(p) => format!("本地代理 ({p})"),
            None => "本地代理".to_string(),
        };
    }
    if host == "api.anthropic.com" {
        return "Anthropic 官方".to_string();
    }
    if host == "api.openai.com" {
        return "OpenAI 官方".to_string();
    }
    let bare = host.strip_prefix("api.").unwrap_or(&host);
    let first = bare.split('.').next().unwrap_or(bare);
    if let Some((_, brand)) = BRAND_DISPLAY_NAMES.iter().find(|(key, _)| *key == first) {
        return brand.to_string();
    }
    let mut chars = first.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: ProfileId, endpoint: &str, fp: Option<&str>) -> Profile {
        Profile {
            id,
            endpoint: endpoint.to_string(),
            credential_fingerprint: fp.map(str::to_string),
            display_name: default_display_name(endpoint),
        }
    }

    #[test]
    fn normalize_endpoint_handles_common_shapes() {
        assert_eq!(
            normalize_endpoint("https://api.deepseek.com/v1/messages"),
            Some("https://api.deepseek.com".to_string())
        );
        assert_eq!(
            normalize_endpoint("HTTPS://API.Example.COM:443/"),
            Some("https://api.example.com".to_string())
        );
        assert_eq!(
            normalize_endpoint("http://127.0.0.1:8080?x=1"),
            Some("http://127.0.0.1:8080".to_string())
        );
        assert_eq!(
            normalize_endpoint("http://[::1]:8080/v1"),
            Some("http://[::1]:8080".to_string())
        );
        assert_eq!(normalize_endpoint("api.example.com"), None);
        assert_eq!(normalize_endpoint("https://"), None);
        assert_eq!(normalize_endpoint("https://host:notaport"), None);
        assert_eq!(normalize_endpoint("https://host:0"), None);
    }

    #[test]
    fn default_display_names() {
        assert_eq!(default_display_name("https://api.deepseek.com"), "DeepSeek");
        assert_eq!(
            default_display_name("https://api.anthropic.com"),
            "Anthropic 官方"
        );
        assert_eq!(
            default_display_name("http://127.0.0.1:8080"),
            "本地代理 (8080)"
        );
        assert_eq!(default_display_name("http://localhost"), "本地代理");
        assert_eq!(default_display_name("https://example.com"), "Example");

        // 品牌表：精确匹配去 api. 后的首段，保留官方大小写
        assert_eq!(default_display_name("https://api.moonshot.cn"), "Moonshot");
        assert_eq!(default_display_name("https://api.kimi.com"), "Kimi");
        assert_eq!(
            default_display_name("https://api.openrouter.ai"),
            "OpenRouter"
        );
        assert_eq!(default_display_name("https://api.bigmodel.cn"), "BigModel");
        assert_eq!(
            default_display_name("https://api.siliconflow.cn"),
            "SiliconFlow"
        );
        assert_eq!(
            default_display_name("https://api.dashscope.aliyuncs.com"),
            "DashScope"
        );
        // 不做子串匹配：mydeepseek 不等于 deepseek
        assert_eq!(
            default_display_name("https://api.mydeepseek.com"),
            "Mydeepseek"
        );
    }

    #[test]
    fn fingerprint_is_short_hex_of_sha256() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223...
        assert_eq!(fingerprint("abc"), "ba7816bf");
        assert_eq!(fingerprint("abc").len(), 8);
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
    }

    #[test]
    fn parse_settings_prefers_auth_token_and_tolerates_shapes() {
        let with_both = r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.deepseek.com",
            "ANTHROPIC_AUTH_TOKEN":"token-a","ANTHROPIC_API_KEY":"key-b"}}"#;
        assert_eq!(
            parse_settings(with_both),
            SettingsSnapshot::Profile {
                endpoint: "https://api.deepseek.com".to_string(),
                fingerprint: Some(fingerprint("token-a")),
            }
        );

        let empty_auth_falls_back = r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.dev",
            "ANTHROPIC_AUTH_TOKEN":"","ANTHROPIC_API_KEY":"key-b"}}"#;
        assert_eq!(
            parse_settings(empty_auth_falls_back),
            SettingsSnapshot::Profile {
                endpoint: "https://x.dev".to_string(),
                fingerprint: Some(fingerprint("key-b")),
            }
        );

        assert_eq!(parse_settings("{broken"), SettingsSnapshot::Unreadable);
        assert_eq!(parse_settings("{}"), SettingsSnapshot::NoAttribution);
        assert_eq!(
            parse_settings(r#"{"env":{}}"#),
            SettingsSnapshot::NoAttribution
        );
        assert_eq!(
            parse_settings(r#"{"env":{"ANTHROPIC_BASE_URL":"not a url"}}"#),
            SettingsSnapshot::NoAttribution
        );
        assert_eq!(
            parse_settings(r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.dev"}}"#),
            SettingsSnapshot::Profile {
                endpoint: "https://x.dev".to_string(),
                fingerprint: None,
            }
        );
    }

    #[test]
    fn unreadable_snapshot_keeps_timeline() {
        let mut timeline = Timeline::new(
            vec![profile(1, "https://a.dev", Some("aaaaaaaa"))],
            vec![Interval {
                id: Some(10),
                profile_id: 1,
                start_ms: 1_000,
                end_ms: None,
            }],
        );
        let before = timeline.clone();

        assert_eq!(
            apply_snapshot(&mut timeline, &SettingsSnapshot::Unreadable, 5_000),
            ApplyOutcome::Unreadable
        );
        assert_eq!(timeline.intervals(), before.intervals());
    }

    #[test]
    fn grace_applies_to_interval_start() {
        let mut timeline = Timeline::default();
        let snap = SettingsSnapshot::Profile {
            endpoint: "https://a.dev".to_string(),
            fingerprint: None,
        };
        apply_snapshot(&mut timeline, &snap, 1_000_000);

        assert_eq!(attribute_at(&timeline, 1_000_000), None, "宽限期内 Unknown");
        assert_eq!(
            attribute_at(&timeline, 1_000_000 + GRACE_MS - 1),
            None,
            "宽限期最后一毫秒仍 Unknown"
        );
        assert_eq!(
            attribute_at(&timeline, 1_000_000 + GRACE_MS),
            Some(1),
            "宽限期结束即归新档案"
        );
    }

    #[test]
    fn test_parse_codex_config_custom_and_official() {
        let custom_toml = r#"
model = "gpt-6.1-sol"
model_provider = "custom"

[model_providers.custom]
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
experimental_bearer_token = "secret-token-123"
"#;
        let snap = parse_codex_config(custom_toml);
        match snap {
            SettingsSnapshot::Profile {
                endpoint,
                fingerprint: fp,
            } => {
                assert_eq!(endpoint, "http://127.0.0.1:15721");
                assert!(fp.is_some());
                assert_eq!(default_display_name(&endpoint), "本地代理 (15721)");
            }
            other => panic!("预期 Profile，实际: {:?}", other),
        }

        let official_toml = r#"
model = "gpt-6.1-sol"
model_provider = "openai"
"#;
        let snap_off = parse_codex_config(official_toml);
        match snap_off {
            SettingsSnapshot::Profile {
                endpoint,
                fingerprint: fp,
            } => {
                assert_eq!(endpoint, "https://api.openai.com");
                assert!(fp.is_none());
                assert_eq!(default_display_name(&endpoint), "OpenAI 官方");
            }
            other => panic!("预期 Profile，实际: {:?}", other),
        }
    }
}
