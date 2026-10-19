//! T1：JSONL 行级解析
//!
//! 把 Claude Code 的 assistant 用量行映射为 [`UsageRecord`]，返回行级三态
//! （完整行 / 忽略 / 坏行）。字段映射与语义以
//! docs/data-source/claude-code-jsonl.md 为准（D22 定案）：
//! - 只处理 `type == "assistant"` 且带 `message.usage` 的行，其余忽略且不计坏行；
//! - 四项 usage 判定（契约「四项判定」）：至少一项存在且为非负整数 → 入库，
//!   缺失项补 0；四项均缺失，或任一存在的字段非数字/为负 → 坏行；
//! - uuid/session/timestamp 无效 → 坏行；
//! - 模型维度取 `message.model`（不是 `requestedModel`）；
//! - 未知字段一律忽略，schema 漂移不得 panic。

use serde_json::Value;

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

    let Some(record_id) = obj.get("uuid").and_then(Value::as_str) else {
        return ParsedLine::Bad;
    };
    if record_id.is_empty() {
        return ParsedLine::Bad;
    }
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
}
