//! T17: 多数据源扩展（Cline / Roo Code / Aider / Cursor 接入）集成测试套件
//!
//! 验证点（基于 docs/designs/v0.4-multi-source-spec.md 全部 D24-D30 决议）：
//! 1. Cline & Roo Code 数据源扫描与解析（ui_messages.json + api_conversation_history.json）；
//! 2. D24: 四维 Token 互斥归一化（Anthropic 排他口径不减，OpenAI 包含式扣减）；
//! 3. D26: 稳定内容主键，Rewind/Retry 不双计；
//! 4. D28: Store 层归因严格物理隔离（非 Claude/Codex 记录默认归属 Unknown，不串入 Claude 订阅）；
//! 5. D29: Aider 30 分钟时间切片与独立会话化；
//! 6. 聚合层 `detect_engine` 正确识别多引擎（"cline" | "roo" | "aider" | "claude" | "codex"），并支持会话飞行记录仪下钻。

use std::fs;
use std::path::{Path, PathBuf};
use serde_json::json;
use token_watcher_lib::core::agg::{self, detect_engine};
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::store::Store;
use token_watcher_lib::watch::{DataEngine, DataPaths, EventSink};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("tw-test-multi-source-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
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

#[derive(Default, Clone)]
struct MockSink;

impl EventSink for MockSink {
    fn is_visible(&self) -> bool {
        true
    }
    fn emit_json(&self, _event: &str, _payload: serde_json::Value) {}
}

#[test]
fn test_cline_and_roo_ingest_and_normalization() {
    let temp = TempDir::new("cline-roo");
    let mut store = Store::open(&temp.path().join("db.sqlite3")).unwrap();

    // 1. 模拟 Cline 任务目录
    let cline_task_dir = temp.path().join("cline_tasks").join("task-c1");
    fs::create_dir_all(&cline_task_dir).unwrap();

    let cline_ui_json = json!([
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
        }
    ]).to_string();
    fs::write(cline_task_dir.join("ui_messages.json"), cline_ui_json).unwrap();

    let cline_history = r#"
    <environment_details>
    <model>claude-3-7-sonnet-20250219</model>
    </environment_details>
    "#;
    fs::write(cline_task_dir.join("api_conversation_history.json"), cline_history).unwrap();

    // 2. 模拟 Roo Code 任务目录（OpenAI 协议）
    let roo_task_dir = temp.path().join("roo_tasks").join("task-r1");
    fs::create_dir_all(&roo_task_dir).unwrap();

    let roo_ui_json = json!([
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
        }
    ]).to_string();
    fs::write(roo_task_dir.join("ui_messages.json"), roo_ui_json).unwrap();

    let roo_history = r#"
    <environment_details>
    <model>deepseek-chat</model>
    </environment_details>
    "#;
    fs::write(roo_task_dir.join("api_conversation_history.json"), roo_history).unwrap();

    // 执行 ingest
    let (stats_c, recs_c) = ingest::ingest_file_with_records(&mut store, &cline_task_dir.join("ui_messages.json")).unwrap();
    assert_eq!(stats_c.records, 1);
    assert_eq!(recs_c.len(), 1);
    assert_eq!(recs_c[0].model, "claude-3-7-sonnet-20250219");
    assert_eq!(recs_c[0].entrypoint, "cline");
    // D24: Anthropic 口径净输入不减
    assert_eq!(recs_c[0].input_tokens, 5000);
    assert_eq!(recs_c[0].cache_read_tokens, 2000);
    assert_eq!(recs_c[0].cache_creation_tokens, 500);

    let (stats_r, recs_r) = ingest::ingest_file_with_records(&mut store, &roo_task_dir.join("ui_messages.json")).unwrap();
    assert_eq!(stats_r.records, 1);
    assert_eq!(recs_r.len(), 1);
    assert_eq!(recs_r[0].model, "deepseek-chat");
    assert_eq!(recs_r[0].entrypoint, "roo");
    // D24: OpenAI 口径净输入扣减: 6000 - 2000 - 1000 = 3000
    assert_eq!(recs_r[0].input_tokens, 3000);
    assert_eq!(recs_r[0].cache_read_tokens, 2000);
    assert_eq!(recs_r[0].cache_creation_tokens, 1000);

    // 引擎识别断言
    assert_eq!(detect_engine(&recs_c[0].record_id, &recs_c[0].entrypoint), "cline");
    assert_eq!(detect_engine(&recs_r[0].record_id, &recs_r[0].entrypoint), "roo");

    // D26 稳定内容主键验证：再次调用 ingest，由于 mtime/size 未变，直接跳过
    let (stats_c2, recs_c2) = ingest::ingest_file_with_records(&mut store, &cline_task_dir.join("ui_messages.json")).unwrap();
    assert_eq!(stats_c2.records, 0);
    assert_eq!(recs_c2.len(), 0);
}

#[test]
fn test_aider_jsonl_ingest_and_sessionization() {
    let temp = TempDir::new("aider");
    let mut store = Store::open(&temp.path().join("db.sqlite3")).unwrap();
    let aider_file = temp.path().join(".aider.analytics.jsonl");

    // 写入 3 条 aider 记录：前 2 条间隔 10 分钟，第 3 条间隔 40 分钟
    let l1 = json!({
        "event": "message_send",
        "user_id": "aid-usr-1234",
        "time": 1755100000,
        "properties": {
            "main_model": "gpt-5.2",
            "prompt_tokens": 1200,
            "completion_tokens": 150
        }
    }).to_string();
    let l2 = json!({
        "event": "message_send",
        "user_id": "aid-usr-1234",
        "time": 1755100600,
        "properties": {
            "main_model": "gpt-5.2",
            "prompt_tokens": 1500,
            "completion_tokens": 200
        }
    }).to_string();
    let l3 = json!({
        "event": "message_send",
        "user_id": "aid-usr-1234",
        "time": 1755103100,
        "properties": {
            "main_model": "gemini-3-flash",
            "prompt_tokens": 800,
            "completion_tokens": 80
        }
    }).to_string();

    fs::write(&aider_file, format!("{}\n{}\n{}\n", l1, l2, l3)).unwrap();

    let (stats, recs) = ingest::ingest_file_with_records(&mut store, &aider_file).unwrap();
    assert_eq!(stats.records, 3);
    assert_eq!(recs.len(), 3);

    // D29: 会话切分验证
    assert_eq!(recs[0].session_id, recs[1].session_id);
    assert_ne!(recs[0].session_id, recs[2].session_id);
    assert_eq!(recs[0].entrypoint, "aider");
    assert_eq!(detect_engine(&recs[0].record_id, &recs[0].entrypoint), "aider");
    assert_eq!(recs[0].model, "gpt-5.2");
    assert_eq!(recs[2].model, "gemini-3-flash");
}

#[test]
fn test_attribution_isolation_and_flight_recorder() {
    let temp = TempDir::new("attr-isolation");
    let paths = DataPaths::for_test(temp.path());
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut engine = DataEngine::new(paths.clone(), MockSink, tx);
    let claude_dir = paths.projects_root.join("p1");
    fs::create_dir_all(&claude_dir).unwrap();
    let claude_line = json!({
        "type": "assistant",
        "uuid": "c-msg-1",
        "session_id": "c-sess-1",
        "timestamp": "2026-10-21T10:00:00.000Z",
        "cwd": "/projects/p1",
        "message": {
            "model": "claude-3-5-sonnet",
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 100
            }
        }
    }).to_string();
    fs::write(claude_dir.join("session.jsonl"), format!("{}\n", claude_line)).unwrap();

    // 写入 Cline 任务
    let cline_dir = paths.cline_tasks_root.join("cline-task-99");
    fs::create_dir_all(&cline_dir).unwrap();
    let cline_ui = json!([
        {
            "type": "say",
            "say": "api_req_started",
            "ts": 1761516645000i64,
            "text": json!({
                "tokensIn": 2500,
                "tokensOut": 120,
                "cacheReads": 0,
                "cacheWrites": 0,
                "apiProtocol": "anthropic"
            }).to_string()
        }
    ]).to_string();
    fs::write(cline_dir.join("ui_messages.json"), cline_ui).unwrap();
    fs::write(cline_dir.join("api_conversation_history.json"), "<environment_details><model>claude-3-7-sonnet</model></environment_details>").unwrap();

    // 触发 initial_scan
    engine.initial_scan();

    // 从 Store 查询归因情况
    let store = Store::open(&paths.db_path).unwrap();
    let now_ms = 1761516645000i64 + 1000;
    let dash = agg::dashboard(&store, agg::Range::All).unwrap();

    // 会话列表中应同时存在 Claude 与 Cline，且分别打上正确的 engine 标识
    assert_eq!(dash.sessions.len(), 2);
    let s_claude = dash.sessions.iter().find(|s| s.engine == "claude").expect("应包含 Claude 会话");
    let s_cline = dash.sessions.iter().find(|s| s.engine == "cline").expect("应包含 Cline 会话");
    assert_eq!(s_claude.title, "p1");
    assert_eq!(s_cline.title, "cline-ta");

    // D28 归因隔离检查：查询数据库，cline 记录的 profile_id 必须为 NULL
    let direct_conn = rusqlite::Connection::open(&paths.db_path).unwrap();
    let mut check_stmt = direct_conn.prepare("SELECT profile_id FROM usage_records WHERE record_id LIKE 'cline:%'").unwrap();
    let cline_profile_id: Option<i64> = check_stmt.query_row([], |row| row.get(0)).unwrap();
    assert_eq!(cline_profile_id, None, "D28: 新引擎记录绝不可污染用户 Claude 订阅 profile_id");

    // Flight Recorder 下钻验证
    let fr = agg::session_flight_recorder(&store, None, "cline-task-99", None, now_ms).unwrap().expect("应能下钻");
    assert_eq!(fr.engine, "cline");
    assert_eq!(fr.title, "cline-ta");
    assert_eq!(fr.totals.input, 2500);
    assert_eq!(fr.total_requests, 1);
}
