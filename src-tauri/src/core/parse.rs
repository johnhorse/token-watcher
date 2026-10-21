//! T1：JSONL 行级解析
//!
//! 把 Claude Code 的 assistant 用量行映射为 [`UsageRecord`]，返回行级三态
//! （完整行 / 忽略 / 坏行）。字段映射与语义以
//! docs/data-source/claude-code-jsonl.md 为准（D22 定案）：
//! - 只处理 `type == "assistant"` 且带 `message.usage` 的行，其余忽略且不计坏行；
//! - 逻辑身份 = `message.id` →（缺失回退）`requestRef` →（再回退）`uuid`
//!   （一次回复按 content block 写多行、各复制整轮 usage，必须按调用去重）；
//! - 四项 usage 判定（契约「四项判定」）：至少一项存在且为非负整数 → 入库，
//!   缺失项补 0；四项均缺失，或任一存在的字段非数字/为负 → 坏行；
//! - uuid/session/timestamp 无效 → 坏行；
//! - 模型维度取 `message.model`（不是 `requestedModel`）；
//! - 未知字段一律忽略，schema 漂移不得 panic。

use serde_json::Value;
use sha2::{Digest, Sha256};
use super::CoreError;

/// 一条 assistant 用量记录（已做字段映射与校验）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub record_id: String,
    pub session_id: String,
    /// Unix epoch 毫秒（UTC）；按本地时区的「今日」过滤在 SQL 侧完成
    pub timestamp_ms: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub model: String,
    pub entrypoint: String,
    pub is_sidechain: bool,
    pub cwd: Option<String>,
    pub cc_version: Option<String>,
}

impl UsageRecord {
    pub fn total_tokens(&self) -> i64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }
}

/// 行级三态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedLine {
    /// 完整行：成功映射出一条用量记录
    Record(UsageRecord),
    /// 非用量行（其他 type、assistant 但无 usage、空行）——不报错、不计数
    Ignored,
    /// 坏行：JSON 非法或用量字段无效——跳过并计数
    Bad,
}

/// 解析一行（不含行尾 `\n`；容忍 `\r` 与首尾空白）
pub fn parse_line(line: &[u8]) -> ParsedLine {
    let line = trim_bytes(line);
    if line.is_empty() {
        return ParsedLine::Ignored;
    }
    let value: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return ParsedLine::Bad,
    };
    let Some(obj) = value.as_object() else {
        return ParsedLine::Bad;
    };
    if obj.get("type").and_then(Value::as_str) != Some("assistant") {
        return ParsedLine::Ignored;
    }
    let Some(message) = obj.get("message").and_then(Value::as_object) else {
        return ParsedLine::Ignored;
    };
    let Some(usage) = message.get("usage") else {
        return ParsedLine::Ignored;
    };
    // 带 usage 却不是一个对象：schema 异常，按坏行处理
    let Some(usage) = usage.as_object() else {
        return ParsedLine::Bad;
    };

    let Some(record_uuid) = obj.get("uuid").and_then(Value::as_str) else {
        return ParsedLine::Bad;
    };
    if record_uuid.is_empty() {
        return ParsedLine::Bad;
    }
    // D22 修订（2026-10-19 口径 bug）：一次 assistant 回复按 content block
    // 写多行、每行复制整轮 usage——行级 uuid 会按块数放大计量（实测虚高
    // ~2.4 倍）。逻辑身份 = message.id（一次 API 调用），缺失回退
    // requestRef（重试各计的保守回退），再回退 uuid（老 schema 行）。
    let record_id = obj
        .get("message")
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.get("requestRef").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .unwrap_or(record_uuid)
        .to_string();
    let session_id = obj
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| obj.get("sessionId").and_then(Value::as_str));
    let Some(session_id) = session_id else {
        return ParsedLine::Bad;
    };
    let Some(timestamp_ms) = obj
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp_ms)
    else {
        return ParsedLine::Bad;
    };
    let Some((input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens)) =
        usage_token_values(usage)
    else {
        return ParsedLine::Bad;
    };

    ParsedLine::Record(UsageRecord {
        record_id: record_id.to_string(),
        session_id: session_id.to_string(),
        timestamp_ms,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        model: message
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        entrypoint: obj
            .get("entrypoint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        is_sidechain: obj
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        cwd: obj.get("cwd").and_then(Value::as_str).map(str::to_string),
        cc_version: obj
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn token_value(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(Value::as_u64)
        .and_then(|n| i64::try_from(n).ok())
}

/// 契约「四项判定」：四项中至少一项存在且为非负整数 → 返回四元组，缺失项补 0；
/// 四项均缺失，或任一存在的字段非数字/为负 → None（坏行）。
/// 键存在即视为「存在」——显式 null 也按无效值处理。
fn usage_token_values(usage: &serde_json::Map<String, Value>) -> Option<(i64, i64, i64, i64)> {
    const KEYS: [&str; 4] = [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ];
    let mut values = [0i64; 4];
    let mut present = 0usize;
    for (slot, key) in KEYS.iter().enumerate() {
        if let Some(value) = usage.get(*key) {
            values[slot] = token_value(Some(value))?;
            present += 1;
        }
    }
    if present == 0 {
        None
    } else {
        Some((values[0], values[1], values[2], values[3]))
    }
}

fn trim_bytes(line: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = line.len();
    while start < end && line[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && line[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &line[start..end]
}

/// 解析 ISO-8601 时间戳为 Unix epoch 毫秒。
///
/// 支持 `YYYY-MM-DDTHH:MM:SS[.fff…]Z` 与 `±HH:MM` 偏移（契约里是 `...Z`，
/// 偏移分支只是防御性容忍）。非法/不可能的日期返回 None → 调用方按坏行处理。
pub fn parse_timestamp_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let year = digits(b, 0, 4)?;
    if b.get(4) != Some(&b'-') {
        return None;
    }
    let month = digits(b, 5, 2)?;
    if b.get(7) != Some(&b'-') {
        return None;
    }
    let day = digits(b, 8, 2)?;
    match b.get(10) {
        Some(b'T') | Some(b' ') => {}
        _ => return None,
    }
    let hour = digits(b, 11, 2)?;
    if b.get(13) != Some(&b':') {
        return None;
    }
    let minute = digits(b, 14, 2)?;
    if b.get(16) != Some(&b':') {
        return None;
    }
    let second = digits(b, 17, 2)?;

    let mut idx = 19;
    let mut millis = 0i64;
    if b.get(idx) == Some(&b'.') {
        idx += 1;
        let start = idx;
        while idx < b.len() && b[idx].is_ascii_digit() {
            idx += 1;
        }
        let count = idx - start;
        if count == 0 {
            return None;
        }
        // 取前三位作为毫秒，多余精度截断
        for k in 0..3 {
            millis = millis * 10
                + if k < count {
                    (b[start + k] - b'0') as i64
                } else {
                    0
                };
        }
    }
    let offset_minutes = match b.get(idx) {
        Some(&b'Z') | Some(&b'z') => {
            idx += 1;
            0i64
        }
        Some(&sign @ (b'+' | b'-')) => {
            idx += 1;
            let offset_hour = digits(b, idx, 2)?;
            idx += 2;
            if b.get(idx) != Some(&b':') {
                return None;
            }
            idx += 1;
            let offset_minute = digits(b, idx, 2)?;
            idx += 2;
            let offset = offset_hour * 60 + offset_minute;
            if sign == b'+' {
                offset
            } else {
                -offset
            }
        }
        _ => return None,
    };
    if idx != b.len() {
        return None;
    }
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some((seconds - offset_minutes * 60) * 1_000 + millis)
}

fn digits(b: &[u8], start: usize, len: usize) -> Option<i64> {
    let slice = b.get(start..start + len)?;
    let mut value = 0i64;
    for &c in slice {
        if !c.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (c - b'0') as i64;
    }
    Some(value)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// 公历日期 → Unix epoch 天数（Howard Hinnant 的 days_from_civil）
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
/// Codex CLI 会话文件的上下文状态（跟踪当前 session、模型、工作目录与版本）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexFileState {
    pub session_id: Option<String>,
    pub current_model: Option<String>,
    pub cwd: Option<String>,
    pub cli_version: Option<String>,
    pub originator: Option<String>,
}

impl CodexFileState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// 解析 Codex CLI rollout JSONL 行
pub fn parse_codex_line(line: &[u8], state: &mut CodexFileState) -> ParsedLine {
    let line = trim_bytes(line);
    if line.is_empty() {
        return ParsedLine::Ignored;
    }
    let value: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return ParsedLine::Bad,
    };
    let Some(obj) = value.as_object() else {
        return ParsedLine::Bad;
    };
    let line_type = match obj.get("type").and_then(Value::as_str) {
        Some(t) => t,
        None => return ParsedLine::Bad,
    };

    match line_type {
        "session_meta" => {
            if let Some(payload) = obj.get("payload").and_then(Value::as_object) {
                if let Some(sid) = payload
                    .get("session_id")
                    .or_else(|| payload.get("id"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    state.session_id = Some(sid.to_string());
                }
                if let Some(cwd) = payload
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    state.cwd = Some(cwd.to_string());
                }
                if let Some(ver) = payload
                    .get("cli_version")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    state.cli_version = Some(ver.to_string());
                }
                if let Some(orig) = payload
                    .get("originator")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    state.originator = Some(orig.to_string());
                }
            }
            ParsedLine::Ignored
        }
        "turn_context" => {
            if let Some(payload) = obj.get("payload").and_then(Value::as_object) {
                if let Some(model) = payload
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    state.current_model = Some(model.to_string());
                }
                if let Some(cwd) = payload
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    state.cwd = Some(cwd.to_string());
                }
            }
            ParsedLine::Ignored
        }
        "event_msg" => {
            if let Some(payload) = obj.get("payload").and_then(Value::as_object) {
                if payload.get("type").and_then(Value::as_str) == Some("thread_settings_applied") {
                    if let Some(model) = payload
                        .get("thread_settings")
                        .and_then(Value::as_object)
                        .and_then(|ts| ts.get("model"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        state.current_model = Some(model.to_string());
                    }
                }
            }
            ParsedLine::Ignored
        }
        "token_usage_record" => {
            let Some(payload) = obj.get("payload").and_then(Value::as_object) else {
                return ParsedLine::Bad;
            };
            let Some(usage) = payload.get("usage").and_then(Value::as_object) else {
                return ParsedLine::Bad;
            };
            let Some(timestamp_ms) = obj
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_timestamp_ms)
            else {
                return ParsedLine::Bad;
            };

            let session_id = payload
                .get("session_id")
                .or_else(|| payload.get("thread_id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or(state.session_id.as_deref());
            let Some(session_id) = session_id else {
                return ParsedLine::Bad;
            };

            let response_id = payload
                .get("response_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let record_id = if let Some(resp) = response_id {
                format!("codex:{}:{}", session_id, resp)
            } else {
                let turn_id = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .unwrap_or("noturn");
                let ordinal = obj.get("ordinal").and_then(Value::as_i64).unwrap_or(0);
                format!("codex:{}:{}:{}:{}", session_id, turn_id, timestamp_ms, ordinal)
            };

            let raw_input = token_value(usage.get("input_tokens"));
            let raw_cached = token_value(usage.get("cached_input_tokens")).unwrap_or(0);
            let raw_cache_write = token_value(usage.get("cache_write_input_tokens")).unwrap_or(0);
            let raw_output = token_value(usage.get("output_tokens"));
            let raw_total = token_value(usage.get("total_tokens"));

            let (Some(raw_input), Some(raw_output)) = (raw_input, raw_output) else {
                return ParsedLine::Bad;
            };
            if raw_input < 0 || raw_cached < 0 || raw_cache_write < 0 || raw_output < 0 {
                return ParsedLine::Bad;
            }
            if raw_cached + raw_cache_write > raw_input {
                return ParsedLine::Bad;
            }
            let net_input = raw_input - raw_cached - raw_cache_write;
            if let Some(total) = raw_total {
                if raw_input + raw_output != total {
                    return ParsedLine::Bad;
                }
            }

            let model = state.current_model.clone().unwrap_or_default();
            let entrypoint = state
                .originator
                .clone()
                .unwrap_or_else(|| "codex".to_string());
            let cwd = state.cwd.clone();
            let cc_version = state.cli_version.clone();

            ParsedLine::Record(UsageRecord {
                record_id,
                session_id: session_id.to_string(),
                timestamp_ms,
                input_tokens: net_input,
                output_tokens: raw_output,
                cache_read_tokens: raw_cached,
                cache_creation_tokens: raw_cache_write,
                model,
                entrypoint,
                is_sidechain: false,
                cwd,
                cc_version,
            })
        }
        _ => ParsedLine::Ignored,
    }
}

/// 计算输入字节流的 SHA-256 哈希前 8 位十六进制字符串
pub fn hash_prefix_8(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut s = String::with_capacity(8);
    for byte in &result[0..4] {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", byte);
    }
    s
}

/// 解析多样化时间戳值（毫秒数字、秒级数字、ISO 8601 字符串）
pub fn parse_ts_value(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(i) = value.as_i64() {
        if i > 1_000_000_000_000 {
            Some(i)
        } else {
            Some(i * 1000)
        }
    } else if let Some(u) = value.as_u64() {
        let i = u.min(i64::MAX as u64) as i64;
        if i > 1_000_000_000_000 {
            Some(i)
        } else {
            Some(i * 1000)
        }
    } else if let Some(s) = value.as_str() {
        if let Ok(num) = s.parse::<i64>() {
            if num > 1_000_000_000_000 {
                Some(num)
            } else {
                Some(num * 1000)
            }
        } else {
            parse_timestamp_ms(s)
        }
    } else {
        None
    }
}

/// 从 Cline / Roo Code 的 `api_conversation_history.json` 内容中提取模型名称
pub fn extract_cline_model(history_content: &str) -> String {
    const ENV_START: &str = "<environment_details>";
    const ENV_END: &str = "</environment_details>";
    let mut last_model = None;
    let mut search_from = 0;
    while let Some(start_rel) = history_content[search_from..].find(ENV_START) {
        let start = search_from + start_rel + ENV_START.len();
        let end = match history_content[start..].find(ENV_END) {
            Some(end_rel) => start + end_rel,
            None => history_content.len(),
        };
        let block = &history_content[start..end];
        if let Some(model) = extract_tag_content(block, "model") {
            last_model = Some(model);
        }
        search_from = end + if end < history_content.len() { ENV_END.len() } else { 0 };
    }
    last_model
        .or_else(|| extract_tag_content(history_content, "model"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn extract_tag_content(text: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{}>", tag);
    let close_tag = format!("</{}>", tag);
    let start = text.find(&open_tag)? + open_tag.len();
    let end = start + text[start..].find(&close_tag)?;
    let val = text[start..end].trim();
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

/// 解析 Cline 或 Roo Code 的 `ui_messages.json`，提取全部 `api_req_started` 用量记录
pub fn parse_cline_roo_entries(
    engine: &str,
    task_id: &str,
    ui_messages_json: &[u8],
    model: &str,
) -> Result<Vec<UsageRecord>, CoreError> {
    let value: Value = serde_json::from_slice(ui_messages_json)
        .map_err(|e| CoreError::Other(format!("JSON parse error: {e}")))?;
    let Some(entries) = value.as_array() else {
        return Ok(Vec::new());
    };

    let mut records = Vec::new();
    for entry in entries {
        let Some(entry_obj) = entry.as_object() else {
            continue;
        };
        if entry_obj.get("type").and_then(Value::as_str) != Some("say")
            || entry_obj.get("say").and_then(Value::as_str) != Some("api_req_started")
        {
            continue;
        }

        let Some(text_str) = entry_obj.get("text").and_then(Value::as_str) else {
            continue;
        };
        let Some(timestamp_ms) = parse_ts_value(entry_obj.get("ts")) else {
            continue;
        };

        let Ok(text_val) = serde_json::from_str::<Value>(text_str) else {
            continue;
        };
        let Some(text_obj) = text_val.as_object() else {
            continue;
        };

        let tokens_in = token_value(text_obj.get("tokensIn")).unwrap_or(0);
        let tokens_out = token_value(text_obj.get("tokensOut")).unwrap_or(0);
        let cache_reads = token_value(text_obj.get("cacheReads")).unwrap_or(0);
        let cache_writes = token_value(text_obj.get("cacheWrites")).unwrap_or(0);
        let api_protocol = text_obj
            .get("apiProtocol")
            .and_then(Value::as_str)
            .unwrap_or("anthropic");

        if tokens_in < 0 || tokens_out < 0 || cache_reads < 0 || cache_writes < 0 {
            continue;
        }

        // D24: 四维 Token 互斥归一化
        let net_input = if api_protocol.eq_ignore_ascii_case("openai") {
            if tokens_in < cache_reads + cache_writes {
                continue;
            }
            tokens_in - cache_reads - cache_writes
        } else {
            // Anthropic 协议下 tokensIn 已是净输入，不能扣减
            tokens_in
        };

        // D26: 稳定内容键，杜绝 Rewind 覆写/双计
        let hash_part = hash_prefix_8(text_str.as_bytes());
        let record_id = format!("{}:{}:{}:{}", engine, task_id, timestamp_ms, hash_part);

        records.push(UsageRecord {
            record_id,
            session_id: task_id.to_string(),
            timestamp_ms,
            input_tokens: net_input,
            output_tokens: tokens_out,
            cache_read_tokens: cache_reads,
            cache_creation_tokens: cache_writes,
            model: model.to_string(),
            entrypoint: engine.to_string(),
            is_sidechain: false,
            cwd: None,
            cc_version: None,
        });
    }

    Ok(records)
}

/// Aider 会话切片追踪器（D29：相邻消息 > 30min 切分新会话）
#[derive(Debug, Default, Clone)]
pub struct AiderSessionTracker {
    pub last_timestamp_ms: i64,
    pub current_session_id: Option<String>,
}

impl AiderSessionTracker {
    pub fn get_or_create_session(&mut self, user_id: &str, timestamp_ms: i64) -> String {
        let gap = (timestamp_ms - self.last_timestamp_ms).abs();
        let should_new = self.current_session_id.is_none() || gap > 1_800_000;
        if should_new {
            let user_short = if user_id.len() >= 8 {
                &user_id[..8]
            } else {
                user_id
            };
            let session_id = format!("aider:{}:{}", user_short, timestamp_ms);
            self.current_session_id = Some(session_id.clone());
            self.last_timestamp_ms = timestamp_ms;
            session_id
        } else {
            self.last_timestamp_ms = timestamp_ms;
            self.current_session_id.clone().unwrap()
        }
    }
}

/// 解析 Aider 的 `analytics.jsonl` 单行
pub fn parse_aider_line(line: &[u8], tracker: &mut AiderSessionTracker) -> ParsedLine {
    let line = trim_bytes(line);
    if line.is_empty() {
        return ParsedLine::Ignored;
    }
    let Ok(val) = serde_json::from_slice::<Value>(line) else {
        return ParsedLine::Bad;
    };
    let Some(obj) = val.as_object() else {
        return ParsedLine::Bad;
    };
    if obj.get("event").and_then(Value::as_str) != Some("message_send") {
        return ParsedLine::Ignored;
    }

    let user_id = obj
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown_user");
    let timestamp_ms = match obj.get("time") {
        Some(Value::Number(n)) => {
            if let Some(secs) = n.as_i64() {
                secs * 1000
            } else if let Some(secs_f) = n.as_f64() {
                (secs_f * 1000.0) as i64
            } else {
                return ParsedLine::Bad;
            }
        }
        _ => return ParsedLine::Bad,
    };

    let Some(props) = obj.get("properties").and_then(Value::as_object) else {
        return ParsedLine::Bad;
    };

    let model = props
        .get("main_model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let prompt_tokens = token_value(props.get("prompt_tokens")).unwrap_or(0);
    let completion_tokens = token_value(props.get("completion_tokens")).unwrap_or(0);

    if prompt_tokens < 0 || completion_tokens < 0 {
        return ParsedLine::Bad;
    }

    let session_id = tracker.get_or_create_session(user_id, timestamp_ms);
    let line_hash = hash_prefix_8(line);
    let record_id = format!("aider:{}:{}:{}", user_id, timestamp_ms, line_hash);

    ParsedLine::Record(UsageRecord {
        record_id,
        session_id,
        timestamp_ms,
        input_tokens: prompt_tokens,
        output_tokens: completion_tokens,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        model: model.to_string(),
        entrypoint: "aider".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
    })
}

/// 解析 omp 的项目 JSONL 会话单行
pub fn parse_omp_line(line: &[u8], session_id: &str) -> ParsedLine {
    let line = trim_bytes(line);
    if line.is_empty() {
        return ParsedLine::Ignored;
    }
    let Ok(val) = serde_json::from_slice::<Value>(line) else {
        return ParsedLine::Bad;
    };
    let Some(obj) = val.as_object() else {
        return ParsedLine::Bad;
    };

    if obj.get("type").and_then(Value::as_str) != Some("message") {
        return ParsedLine::Ignored;
    }
    let Some(msg) = obj.get("message").and_then(Value::as_object) else {
        return ParsedLine::Ignored;
    };
    if msg.get("role").and_then(Value::as_str) != Some("assistant") {
        return ParsedLine::Ignored;
    }
    let Some(usage) = msg.get("usage").and_then(Value::as_object) else {
        return ParsedLine::Ignored;
    };

    let input = token_value(usage.get("input")).unwrap_or(0);
    let output = token_value(usage.get("output")).unwrap_or(0);
    let cache_read = token_value(usage.get("cacheRead")).unwrap_or(0);
    let cache_write = token_value(usage.get("cacheWrite")).unwrap_or(0);

    if input < 0 || output < 0 || cache_read < 0 || cache_write < 0 {
        return ParsedLine::Bad;
    }
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        return ParsedLine::Ignored;
    }

    let timestamp_ms = parse_ts_value(obj.get("timestamp")).unwrap_or(0);

    let msg_id = obj
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id_part = if msg_id.is_empty() {
        hash_prefix_8(line)
    } else {
        msg_id.to_string()
    };

    let model = msg
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let record_id = format!("omp:{}:{}", session_id, id_part);

    ParsedLine::Record(UsageRecord {
        record_id,
        session_id: session_id.to_string(),
        timestamp_ms,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
        model,
        entrypoint: "omp".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
    })
}

/// 解析 OpenCode 消息表的一行记录
pub fn parse_opencode_message(
    msg_id: &str,
    session_id: &str,
    time_created: i64,
    data_json: &str,
) -> Option<UsageRecord> {
    let Ok(data) = serde_json::from_str::<Value>(data_json) else {
        return None;
    };
    let obj = data.as_object()?;
    if obj.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let tokens = obj.get("tokens")?.as_object()?;

    let input = token_value(tokens.get("input")).unwrap_or(0);
    let output = token_value(tokens.get("output")).unwrap_or(0);
    let cache_obj = tokens.get("cache").and_then(Value::as_object);
    let cache_read = cache_obj
        .and_then(|c| token_value(c.get("read")))
        .unwrap_or(0);
    let cache_write = cache_obj
        .and_then(|c| token_value(c.get("write")))
        .unwrap_or(0);

    if input < 0 || output < 0 || cache_read < 0 || cache_write < 0 {
        return None;
    }
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        return None;
    }

    let model = obj
        .get("modelID")
        .and_then(Value::as_str)
        .unwrap_or("opencode-model")
        .to_string();

    let cwd = obj
        .get("path")
        .and_then(Value::as_object)
        .and_then(|p| p.get("cwd"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let record_id = format!("opencode:{}:{}", session_id, msg_id);

    Some(UsageRecord {
        record_id,
        session_id: session_id.to_string(),
        timestamp_ms: time_created,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
        model,
        entrypoint: "opencode".to_string(),
        is_sidechain: false,
        cwd,
        cc_version: None,
    })
}

/// 解析 Continue.dev 会话 JSON 记录
pub fn parse_continue_session(
    file_session_id: &str,
    bytes: &[u8],
) -> Result<Vec<UsageRecord>, CoreError> {
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|e| CoreError::Other(format!("Continue JSON 解析失败: {e}")))?;
    let session_id = root
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or(file_session_id);
    let history = match root.get("history").and_then(Value::as_array) {
        Some(h) => h,
        None => return Ok(Vec::new()),
    };

    let mut records = Vec::new();
    for (idx, item) in history.iter().enumerate() {
        let msg = item.get("message").and_then(Value::as_object);
        let role = msg
            .and_then(|m| m.get("role"))
            .or_else(|| item.get("role"))
            .and_then(Value::as_str);
        if role != Some("assistant") {
            continue;
        }

        let mut input_tokens = 0i64;
        let mut output_tokens = 0i64;
        let mut model_name = "continue-model".to_string();
        let mut found = false;

        if let Some(prompt_logs) = item.get("promptLogs").and_then(Value::as_array) {
            for log in prompt_logs {
                let p = token_value(log.get("promptTokens")).unwrap_or(0);
                let c = token_value(log.get("completionTokens")).unwrap_or(0);
                if p > 0 || c > 0 {
                    input_tokens += p;
                    output_tokens += c;
                    if let Some(m) = log.get("modelTitle").and_then(Value::as_str) {
                        model_name = m.to_string();
                    }
                    found = true;
                }
            }
        }

        if !found {
            let usage = item.get("usage").or_else(|| msg.and_then(|m| m.get("usage")));
            if let Some(u) = usage.and_then(Value::as_object) {
                let p = token_value(u.get("prompt_tokens").or_else(|| u.get("input"))).unwrap_or(0);
                let c = token_value(u.get("completion_tokens").or_else(|| u.get("output"))).unwrap_or(0);
                if p > 0 || c > 0 {
                    input_tokens = p;
                    output_tokens = c;
                    found = true;
                }
            }
        }

        if !found || (input_tokens == 0 && output_tokens == 0) {
            continue;
        }

        let timestamp_ms = parse_ts_value(item.get("timestamp"))
            .or_else(|| msg.and_then(|m| parse_ts_value(m.get("timestamp"))))
            .unwrap_or(0);

        let record_id = format!("continue:{}:{}:{}", session_id, idx, timestamp_ms);

        records.push(UsageRecord {
            record_id,
            session_id: session_id.to_string(),
            timestamp_ms,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: model_name,
            entrypoint: "continue".to_string(),
            is_sidechain: false,
            cwd: None,
            cc_version: None,
        });
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant_line(message: Value, extra: Value) -> Vec<u8> {
        let mut obj = json!({
            "type": "assistant",
            "uuid": "11111111-1111-4111-8111-111111111111",
            "session_id": "sess-1",
            "timestamp": "2026-10-19T10:03:45.883Z",
            "message": message,
        });
        let map = obj.as_object_mut().unwrap();
        for (k, v) in extra.as_object().unwrap() {
            map.insert(k.clone(), v.clone());
        }
        obj.to_string().into_bytes()
    }

    fn usage(input: Value, output: Value, cache_read: Value, cache_creation: Value) -> Value {
        json!({
            "input_tokens": input,
            "output_tokens": output,
            "cache_read_input_tokens": cache_read,
            "cache_creation_input_tokens": cache_creation,
        })
    }

    #[test]
    fn maps_all_contract_fields() {
        let line = assistant_line(
            json!({
                "model": "glm-5.3",
                "requestedModel": "claude-opus-5[1m]",
                "usage": usage(json!(1200), json!(300), json!(50000), json!(400)),
            }),
            json!({
                "entrypoint": "cli",
                "isSidechain": true,
                "cwd": "/tmp/proj",
                "version": "2.1.303",
            }),
        );
        let ParsedLine::Record(r) = parse_line(&line) else {
            panic!("应解析出记录");
        };
        assert_eq!(r.record_id, "11111111-1111-4111-8111-111111111111");
        assert_eq!(r.session_id, "sess-1");
        assert_eq!(r.timestamp_ms, 1_792_404_225_883);
        assert_eq!(r.input_tokens, 1200);
        assert_eq!(r.output_tokens, 300);
        assert_eq!(r.cache_read_tokens, 50_000);
        assert_eq!(r.cache_creation_tokens, 400);
        // 模型维度取 message.model，不是 requestedModel
        assert_eq!(r.model, "glm-5.3");
        assert_eq!(r.entrypoint, "cli");
        assert!(r.is_sidechain);
        assert_eq!(r.cwd.as_deref(), Some("/tmp/proj"));
        assert_eq!(r.cc_version.as_deref(), Some("2.1.303"));
        assert_eq!(r.total_tokens(), 51_900);
    }

    #[test]
    fn record_id_uses_message_id_with_fallback_chain() {
        let usage = usage(json!(100), json!(20), json!(5000), json!(0));
        // message.id 存在 → 逻辑身份 = message.id（D22 修订）
        let line = assistant_line(
            json!({"id": "msg_abc", "usage": usage}),
            json!({"requestRef": "ref-1"}),
        );
        let ParsedLine::Record(r) = parse_line(&line) else {
            panic!()
        };
        assert_eq!(r.record_id, "msg_abc");
        // message.id 缺失 → 回退 requestRef
        let line = assistant_line(json!({"usage": usage}), json!({"requestRef": "ref-2"}));
        let ParsedLine::Record(r) = parse_line(&line) else {
            panic!()
        };
        assert_eq!(r.record_id, "ref-2");
        // 两者都缺 → 回退 uuid（老 schema 行为）
        let line = assistant_line(json!({"usage": usage}), json!({}));
        let ParsedLine::Record(r) = parse_line(&line) else {
            panic!()
        };
        assert_eq!(r.record_id, "11111111-1111-4111-8111-111111111111");
    }

    #[test]
    fn session_id_falls_back_to_session_id_camel_case() {
        let mut line: Value = serde_json::from_slice(&assistant_line(
            json!({"model": "m", "usage": usage(json!(1), json!(1), json!(0), json!(0))}),
            json!({}),
        ))
        .unwrap();
        let map = line.as_object_mut().unwrap();
        map.remove("session_id");
        map.insert("sessionId".into(), json!("camel-session"));
        let ParsedLine::Record(r) = parse_line(line.to_string().as_bytes()) else {
            panic!("应解析出记录");
        };
        assert_eq!(r.session_id, "camel-session");
    }

    #[test]
    fn optional_fields_default_to_empty() {
        let line = assistant_line(
            json!({"usage": usage(json!(1), json!(2), json!(3), json!(4))}),
            json!({}),
        );
        let ParsedLine::Record(r) = parse_line(&line) else {
            panic!("应解析出记录");
        };
        assert_eq!(r.model, "");
        assert_eq!(r.entrypoint, "");
        assert!(!r.is_sidechain);
        assert_eq!(r.cwd, None);
        assert_eq!(r.cc_version, None);
    }

    #[test]
    fn non_usage_lines_are_ignored() {
        let user = br#"{"type":"user","uuid":"u1","message":{"role":"user","content":"hi"}}"#;
        assert_eq!(parse_line(user), ParsedLine::Ignored);
        let attachment = br#"{"type":"attachment","uuid":"a1"}"#;
        assert_eq!(parse_line(attachment), ParsedLine::Ignored);
        let no_usage = br#"{"type":"assistant","uuid":"a2","timestamp":"2026-10-19T10:03:45.883Z","message":{"model":"m"}}"#;
        assert_eq!(parse_line(no_usage), ParsedLine::Ignored);
        assert_eq!(parse_line(b""), ParsedLine::Ignored);
        assert_eq!(parse_line(b"   \r"), ParsedLine::Ignored);
    }

    #[test]
    fn missing_usage_fields_default_to_zero() {
        // 只有 input/output（真实数据中 team/subagent 占位记录的形态）
        let line = assistant_line(
            json!({
                "model": "gpt-6.1-sol",
                "usage": json!({"input_tokens": 0, "output_tokens": 0}),
            }),
            json!({}),
        );
        let ParsedLine::Record(r) = parse_line(&line) else {
            panic!("缺 cache 两项应入库");
        };
        assert_eq!(r.input_tokens, 0);
        assert_eq!(r.output_tokens, 0);
        assert_eq!(r.cache_read_tokens, 0);
        assert_eq!(r.cache_creation_tokens, 0);

        // 只有 cache_read：其余三项补 0
        let line = assistant_line(
            json!({"usage": json!({"cache_read_input_tokens": 42})}),
            json!({}),
        );
        let ParsedLine::Record(r) = parse_line(&line) else {
            panic!("单项存在应入库");
        };
        assert_eq!(r.input_tokens, 0);
        assert_eq!(r.output_tokens, 0);
        assert_eq!(r.cache_read_tokens, 42);
        assert_eq!(r.cache_creation_tokens, 0);

        // input 存在、output 缺失
        let line = assistant_line(json!({"usage": json!({"input_tokens": 9})}), json!({}));
        let ParsedLine::Record(r) = parse_line(&line) else {
            panic!("部分字段存在应入库");
        };
        assert_eq!(r.input_tokens, 9);
        assert_eq!(r.output_tokens, 0);
    }

    #[test]
    fn malformed_lines_are_bad() {
        // 非法 JSON
        assert_eq!(parse_line(br#"{"oops": "#), ParsedLine::Bad);
        // JSON 但不是对象
        assert_eq!(parse_line(b"[1,2,3]"), ParsedLine::Bad);
        let base = json!({
            "type": "assistant",
            "uuid": "u-1",
            "session_id": "s",
            "timestamp": "2026-10-19T10:03:45.883Z",
            "message": {"model": "m", "usage": usage(json!(1), json!(1), json!(0), json!(0))},
        });
        let cases: [fn(&mut Value); 8] = [
            // 四项全缺
            |v: &mut Value| {
                v["message"]["usage"] = json!({});
            },
            // usage 字段非数字
            |v: &mut Value| {
                v["message"]["usage"]["input_tokens"] = json!("12");
            },
            // usage 字段为负
            |v: &mut Value| {
                v["message"]["usage"]["input_tokens"] = json!(-5);
            },
            // 存在但为 null：按「非数字」处理
            |v: &mut Value| {
                v["message"]["usage"]["output_tokens"] = Value::Null;
            },
            // timestamp 非法
            |v: &mut Value| {
                v["timestamp"] = json!("not-a-time");
            },
            // uuid 缺失
            |v: &mut Value| {
                v.as_object_mut().unwrap().remove("uuid");
            },
            // session 双字段都缺失
            |v: &mut Value| {
                v.as_object_mut().unwrap().remove("session_id");
            },
            // usage 不是对象
            |v: &mut Value| {
                v["message"]["usage"] = json!("nope");
            },
        ];
        for mutate in cases {
            let mut v = base.clone();
            mutate(&mut v);
            assert_eq!(
                parse_line(v.to_string().as_bytes()),
                ParsedLine::Bad,
                "变化的行应判为坏行: {v}"
            );
        }
    }

    #[test]
    fn parses_iso_timestamps() {
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_timestamp_ms("2026-10-19T00:00:00.000Z"),
            Some(1_792_368_000_000)
        );
        assert_eq!(
            parse_timestamp_ms("2026-10-19T10:03:45.883Z"),
            Some(1_792_404_225_883)
        );
        // 偏移等价：+08:00 的同一瞬间
        assert_eq!(
            parse_timestamp_ms("2026-10-19T18:03:45.883+08:00"),
            Some(1_792_404_225_883)
        );
        // 分数秒缺省/多精度
        assert_eq!(
            parse_timestamp_ms("2026-10-19T10:03:45Z"),
            Some(1_792_404_225_000)
        );
        assert_eq!(
            parse_timestamp_ms("2026-10-19T10:03:45.883999Z"),
            Some(1_792_404_225_883)
        );
        // 闰日
        assert_eq!(
            parse_timestamp_ms("2024-02-29T00:00:00.000Z"),
            Some(1_709_164_800_000)
        );
        // 非法输入
        for bad in [
            "",
            "not-a-time",
            "2026-13-01T00:00:00.000Z",
            "2026-02-30T00:00:00.000Z",
            "2026-10-19T24:00:00.000Z",
            "2026-10-19T10:03:45.883",
            "2026-10-19T10:03:45.883Zjunk",
            "2026-10-19X10:03:45.883Z",
        ] {
            assert_eq!(parse_timestamp_ms(bad), None, "应拒绝: {bad}");
        }
    }

    #[test]
    fn test_codex_full_lifecycle_and_normalization() {
        let mut state = CodexFileState::new();

        let meta_line = json!({
            "timestamp": "2026-10-20T05:15:53.324Z",
            "ordinal": 0,
            "type": "session_meta",
            "payload": {
                "session_id": "01a157bc-cb63-7ee1-a7c7-b75a46e9c9ac",
                "cwd": "/Volumes/Data/projects/app",
                "originator": "codex-tui",
                "cli_version": "0.163.0"
            }
        }).to_string();
        assert_eq!(parse_codex_line(meta_line.as_bytes(), &mut state), ParsedLine::Ignored);
        assert_eq!(state.session_id.as_deref(), Some("01a157bc-cb63-7ee1-a7c7-b75a46e9c9ac"));
        assert_eq!(state.originator.as_deref(), Some("codex-tui"));
        assert_eq!(state.cwd.as_deref(), Some("/Volumes/Data/projects/app"));
        assert_eq!(state.cli_version.as_deref(), Some("0.163.0"));

        let turn_line = json!({
            "timestamp": "2026-10-20T05:15:55.903Z",
            "ordinal": 7,
            "type": "turn_context",
            "payload": {
                "turn_id": "turn-1",
                "model": "gpt-6.1-sol"
            }
        }).to_string();
        assert_eq!(parse_codex_line(turn_line.as_bytes(), &mut state), ParsedLine::Ignored);
        assert_eq!(state.current_model.as_deref(), Some("gpt-6.1-sol"));

        // OpenAI 包含式：raw input = 27471, cached = 3968, cache_write = 500, out = 210, total = 27681
        let usage_line = json!({
            "timestamp": "2026-10-20T05:16:07.953Z",
            "ordinal": 15,
            "type": "token_usage_record",
            "payload": {
                "session_id": "01a157bc-cb63-7ee1-a7c7-b75a46e9c9ac",
                "turn_id": "turn-1",
                "response_id": "resp-12345",
                "usage": {
                    "input_tokens": 27471,
                    "cached_input_tokens": 3968,
                    "cache_write_input_tokens": 500,
                    "output_tokens": 210,
                    "total_tokens": 27681
                }
            }
        }).to_string();

        let parsed = parse_codex_line(usage_line.as_bytes(), &mut state);
        match parsed {
            ParsedLine::Record(rec) => {
                assert_eq!(rec.record_id, "codex:01a157bc-cb63-7ee1-a7c7-b75a46e9c9ac:resp-12345");
                assert_eq!(rec.session_id, "01a157bc-cb63-7ee1-a7c7-b75a46e9c9ac");
                assert_eq!(rec.model, "gpt-6.1-sol");
                assert_eq!(rec.entrypoint, "codex-tui");
                assert_eq!(rec.input_tokens, 27471 - 3968 - 500); // 纯净输入
                assert_eq!(rec.cache_read_tokens, 3968);
                assert_eq!(rec.cache_creation_tokens, 500);
                assert_eq!(rec.output_tokens, 210);
                assert_eq!(rec.total_tokens(), 27681);
            }
            other => panic!("预期 Record，实际: {:?}", other),
        }
    }

    #[test]
    fn test_codex_thread_settings_applied_switch() {
        let mut state = CodexFileState::new();
        let settings_line = json!({
            "timestamp": "2026-10-20T05:15:55.903Z",
            "ordinal": 8,
            "type": "event_msg",
            "payload": {
                "type": "thread_settings_applied",
                "thread_id": "t-1",
                "thread_settings": {
                    "model": "gemini-4-argon"
                }
            }
        }).to_string();
        assert_eq!(parse_codex_line(settings_line.as_bytes(), &mut state), ParsedLine::Ignored);
        assert_eq!(state.current_model.as_deref(), Some("gemini-4-argon"));
    }

    #[test]
    fn test_codex_cached_overflow_is_bad_line() {
        let mut state = CodexFileState::new();
        state.session_id = Some("s-1".into());
        let bad_line = json!({
            "timestamp": "2026-10-20T05:16:07.953Z",
            "ordinal": 15,
            "type": "token_usage_record",
            "payload": {
                "session_id": "s-1",
                "response_id": "resp-1",
                "usage": {
                    "input_tokens": 1000,
                    "cached_input_tokens": 1200, // 超过了 input_tokens
                    "output_tokens": 100,
                    "total_tokens": 1100
                }
            }
        }).to_string();
        assert_eq!(parse_codex_line(bad_line.as_bytes(), &mut state), ParsedLine::Bad);
    }

    #[test]
    fn test_codex_fallback_record_id_without_response_id() {
        let mut state = CodexFileState::new();
        let line = json!({
            "timestamp": "2026-10-20T05:16:07.953Z",
            "ordinal": 42,
            "type": "token_usage_record",
            "payload": {
                "session_id": "s-1",
                "turn_id": "turn-9",
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 50,
                    "total_tokens": 1050
                }
            }
        }).to_string();
        match parse_codex_line(line.as_bytes(), &mut state) {
            ParsedLine::Record(rec) => {
                assert!(rec.record_id.starts_with("codex:s-1:turn-9:"));
                assert!(rec.record_id.ends_with(":42"));
            }
            other => panic!("预期 Record，实际: {:?}", other),
        }
    }

    #[test]
    fn test_codex_ignored_types() {
        let mut state = CodexFileState::new();
        for typ in ["compacted", "event_msg", "response_item", "world_state", "task_started"] {
            let line = json!({
                "type": typ,
                "payload": {}
            }).to_string();
            assert_eq!(parse_codex_line(line.as_bytes(), &mut state), ParsedLine::Ignored);
        }
    }

    #[test]
    fn test_parse_cline_roo_anthropic_and_openai_normalization() {
        let ui_json = json!([
            {
                "type": "say",
                "say": "api_req_started",
                "ts": 1761516645000i64,
                "text": json!({
                    "tokensIn": 5000,
                    "tokensOut": 200,
                    "cacheReads": 2000,
                    "cacheWrites": 500,
                    "cost": 0.05,
                    "apiProtocol": "anthropic"
                }).to_string()
            },
            {
                "type": "say",
                "say": "api_req_started",
                "ts": 1761516650000i64,
                "text": json!({
                    "tokensIn": 6000,
                    "tokensOut": 300,
                    "cacheReads": 2000,
                    "cacheWrites": 1000,
                    "cost": 0.08,
                    "apiProtocol": "openai"
                }).to_string()
            },
            {
                // OpenAI 破损行：cacheReads + cacheWrites > tokensIn
                "type": "say",
                "say": "api_req_started",
                "ts": 1761516655000i64,
                "text": json!({
                    "tokensIn": 1000,
                    "tokensOut": 50,
                    "cacheReads": 2000,
                    "cacheWrites": 500,
                    "apiProtocol": "openai"
                }).to_string()
            }
        ]).to_string();

        let history = r#"
        <environment_details>
        <model>claude-3-7-sonnet-20250219</model>
        <name>ClineTask</name>
        </environment_details>
        "#;
        let model = extract_cline_model(history);
        assert_eq!(model, "claude-3-7-sonnet-20250219");

        let records = parse_cline_roo_entries("cline", "task-abc", ui_json.as_bytes(), &model).unwrap();
        assert_eq!(records.len(), 2, "第3条破损行应被自动跳过");

        // 第 1 条 (Anthropic)：tokensIn 已是净输入，不能减
        assert_eq!(records[0].input_tokens, 5000);
        assert_eq!(records[0].output_tokens, 200);
        assert_eq!(records[0].cache_read_tokens, 2000);
        assert_eq!(records[0].cache_creation_tokens, 500);
        assert_eq!(records[0].entrypoint, "cline");
        assert!(records[0].record_id.starts_with("cline:task-abc:1761516645000:"));

        // 第 2 条 (OpenAI)：6000 - 2000 - 1000 = 3000
        assert_eq!(records[1].input_tokens, 3000);
        assert_eq!(records[1].output_tokens, 300);
        assert_eq!(records[1].cache_read_tokens, 2000);
        assert_eq!(records[1].cache_creation_tokens, 1000);
    }

    #[test]
    fn test_parse_aider_sessionization_30min() {
        let mut tracker = AiderSessionTracker::default();
        let line1 = json!({
            "event": "message_send",
            "user_id": "u1234567890",
            "time": 1755100000,
            "properties": {
                "main_model": "gpt-5.2",
                "prompt_tokens": 1200,
                "completion_tokens": 150,
                "cost": 0.02
            }
        }).to_string();

        // 10 分钟后（同会话）
        let line2 = json!({
            "event": "message_send",
            "user_id": "u1234567890",
            "time": 1755100600,
            "properties": {
                "main_model": "gpt-5.2",
                "prompt_tokens": 1500,
                "completion_tokens": 200,
                "cost": 0.03
            }
        }).to_string();

        // 40 分钟后（> 30 分钟，触发新会话切片）
        let line3 = json!({
            "event": "message_send",
            "user_id": "u1234567890",
            "time": 1755103100,
            "properties": {
                "main_model": "gemini-3-flash",
                "prompt_tokens": 800,
                "completion_tokens": 80,
                "cost": 0.01
            }
        }).to_string();

        let r1 = match parse_aider_line(line1.as_bytes(), &mut tracker) {
            ParsedLine::Record(r) => r,
            other => panic!("预期 Record，实际: {:?}", other),
        };
        let r2 = match parse_aider_line(line2.as_bytes(), &mut tracker) {
            ParsedLine::Record(r) => r,
            other => panic!("预期 Record，实际: {:?}", other),
        };
        let r3 = match parse_aider_line(line3.as_bytes(), &mut tracker) {
            ParsedLine::Record(r) => r,
            other => panic!("预期 Record，实际: {:?}", other),
        };

        assert_eq!(r1.session_id, r2.session_id, "10分钟内的消息应属于同一会话");
        assert_ne!(r1.session_id, r3.session_id, "超过30分钟的消息应切分新会话");
        assert_eq!(r1.input_tokens, 1200);
        assert_eq!(r1.output_tokens, 150);
        assert_eq!(r1.model, "gpt-5.2");
        assert_eq!(r1.entrypoint, "aider");
        assert_eq!(r3.model, "gemini-3-flash");
    }
}
