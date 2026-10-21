//! T18: 多数据源生态 2.0（OpenCode / Continue.dev / omp 接入）集成测试套件
//!
//! 验证点（基于 docs/designs/v0.5-multi-source-v2-spec.md 全部 D31-D35 决议）：
//! 1. OpenCode: SQLite (`opencode.db`) 只读增量查询与 `max(time_created)` 游标；
//! 2. Continue.dev: 会话 JSON 文件解析与 `(mtime, size)` 跳过；
//! 3. omp: 项目目录结构下的 JSONL 增量行解析与四维归一化；
//! 4. 聚合层 `detect_engine` 正确识别 "opencode"、"continue"、"omp"；
//! 5. DataEngine 首扫并发接入与数据隔离。

use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use token_watcher_lib::core::agg::detect_engine;
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::store::Store;
use token_watcher_lib::watch::{DataEngine, DataPaths, EventSink};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "tw-test-multi-source-v2-{}-{}",
            name,
            std::process::id()
        ));
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
fn test_opencode_sqlite_ingest_and_checkpoint() {
    let temp = TempDir::new("opencode");
    let mut store = Store::open(&temp.path().join("db.sqlite3")).unwrap();

    // 1. 创建模拟的 opencode.db
    let opencode_db_path = temp.path().join("opencode.db");
    let conn = rusqlite::Connection::open(&opencode_db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            data TEXT NOT NULL
        );",
    )
    .unwrap();

    let msg1_data = json!({
        "role": "assistant",
        "modelID": "claude-sonnet-4.6",
        "tokens": {
            "total": 1500,
            "input": 1200,
            "output": 300,
            "cache": {
                "write": 0,
                "read": 200
            }
        },
        "path": {"cwd": "/projects/test-opencode"}
    })
    .to_string();

    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["msg_1", "ses_abc", 1777275000000i64, 1777275000000i64, msg1_data],
    ).unwrap();

    // 用户消息（应当被自动忽略）
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["msg_user", "ses_abc", 1777275000010i64, 1777275000010i64, r#"{"role":"user"}"#],
    ).unwrap();

    // 首次 ingest
    let (stats1, records1) =
        ingest::ingest_file_with_records(&mut store, &opencode_db_path).unwrap();
    assert_eq!(stats1.records, 1);
    assert_eq!(records1.len(), 1);
    assert_eq!(records1[0].record_id, "opencode:ses_abc:msg_1");
    assert_eq!(records1[0].input_tokens, 1200);
    assert_eq!(records1[0].output_tokens, 300);
    assert_eq!(records1[0].cache_read_tokens, 200);
    assert_eq!(records1[0].model, "claude-sonnet-4.6");
    assert_eq!(records1[0].entrypoint, "opencode");
    assert_eq!(records1[0].cwd.as_deref(), Some("/projects/test-opencode"));

    // 第二次无新消息调用：应返回 0 增量
    let (stats2, records2) =
        ingest::ingest_file_with_records(&mut store, &opencode_db_path).unwrap();
    assert_eq!(stats2.records, 0);
    assert_eq!(records2.len(), 0);

    // 写入第二条新消息
    let msg2_data = json!({
        "role": "assistant",
        "modelID": "glm-5-turbo",
        "tokens": {
            "total": 500,
            "input": 400,
            "output": 100,
            "cache": {
                "write": 50,
                "read": 0
            }
        }
    })
    .to_string();

    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["msg_2", "ses_abc", 1777275100000i64, 1777275100000i64, msg2_data],
    ).unwrap();

    let (stats3, records3) =
        ingest::ingest_file_with_records(&mut store, &opencode_db_path).unwrap();
    assert_eq!(stats3.records, 1);
    assert_eq!(records3.len(), 1);
    assert_eq!(records3[0].record_id, "opencode:ses_abc:msg_2");
    assert_eq!(records3[0].cache_creation_tokens, 50);
}

#[test]
fn test_continue_session_ingest_and_mtime_skip() {
    let temp = TempDir::new("continue");
    let mut store = Store::open(&temp.path().join("db.sqlite3")).unwrap();

    let continue_dir = temp.path().join(".continue").join("sessions");
    fs::create_dir_all(&continue_dir).unwrap();

    let session_file = continue_dir.join("b47c093a-861f-4efc-8b83-ff621e281512.json");
    let session_content = json!({
        "sessionId": "b47c093a-861f-4efc-8b83-ff621e281512",
        "title": "Fix bug in parser",
        "history": [
            {
                "message": { "role": "user", "content": "hello" }
            },
            {
                "message": { "role": "assistant", "content": "world" },
                "promptLogs": [
                    {
                        "modelTitle": "claude-3-5-sonnet",
                        "promptTokens": 1000,
                        "completionTokens": 250
                    }
                ],
                "timestamp": 1777276000000i64
            }
        ]
    })
    .to_string();

    fs::write(&session_file, &session_content).unwrap();

    let (stats1, records1) = ingest::ingest_file_with_records(&mut store, &session_file).unwrap();
    assert_eq!(stats1.records, 1);
    assert_eq!(records1.len(), 1);
    assert_eq!(
        records1[0].record_id,
        "continue:b47c093a-861f-4efc-8b83-ff621e281512:1:1777276000000"
    );
    assert_eq!(records1[0].input_tokens, 1000);
    assert_eq!(records1[0].output_tokens, 250);
    assert_eq!(records1[0].model, "claude-3-5-sonnet");
    assert_eq!(records1[0].entrypoint, "continue");

    // 未发生修改直接跳过
    let (stats2, records2) = ingest::ingest_file_with_records(&mut store, &session_file).unwrap();
    assert_eq!(stats2.records, 0);
    assert_eq!(records2.len(), 0);
}

#[test]
fn test_omp_jsonl_ingest() {
    let temp = TempDir::new("omp");
    let mut store = Store::open(&temp.path().join("db.sqlite3")).unwrap();

    let omp_project_dir = temp
        .path()
        .join(".omp")
        .join("agent")
        .join("sessions")
        .join("my-project");
    fs::create_dir_all(&omp_project_dir).unwrap();

    let session_file = omp_project_dir.join("2026-10-21T02-43-34-502Z_sess-omp-123.jsonl");

    let line0 = json!({
        "type": "session",
        "id": "sess-omp-123",
        "timestamp": "2026-10-21T02:43:34.000Z",
        "cwd": "/Volumes/Data/projects/my-project"
    })
    .to_string();

    let line1 = json!({
        "type": "message",
        "id": "msg_omp_1",
        "timestamp": "2026-10-21T02:43:35.000Z",
        "message": {
            "role": "assistant",
            "model": "gemini-3.8-flash",
            "usage": {
                "input": 2000,
                "output": 400,
                "cacheRead": 800,
                "cacheWrite": 100
            }
        }
    })
    .to_string();

    fs::write(&session_file, format!("{}\n{}\n", line0, line1)).unwrap();

    let (stats1, records1) = ingest::ingest_file_with_records(&mut store, &session_file).unwrap();
    assert_eq!(stats1.records, 1);
    assert_eq!(records1.len(), 1);
    assert!(records1[0].record_id.contains("omp:"));
    assert!(records1[0].record_id.contains("msg_omp_1"));
    assert_eq!(records1[0].input_tokens, 2000);
    assert_eq!(records1[0].output_tokens, 400);
    assert_eq!(records1[0].cache_read_tokens, 800);
    assert_eq!(records1[0].cache_creation_tokens, 100);
    assert_eq!(records1[0].model, "gemini-3.8-flash");
    assert_eq!(records1[0].entrypoint, "omp");
    assert_eq!(
        records1[0].cwd.as_deref(),
        Some("/Volumes/Data/projects/my-project")
    );
}

#[test]
fn test_omp_extract_cwd_from_file_or_dir() {
    let temp = TempDir::new("omp-cwd");
    let proj = temp.path().join("--Volumes-Data-projects-token-watcher--");
    fs::create_dir_all(&proj).unwrap();
    let file = proj.join("sess.jsonl");
    fs::write(
        &file,
        "{\"type\":\"session\",\"cwd\":\"/Volumes/Data/projects/token-watcher\"}\n",
    )
    .unwrap();
    let cwd = ingest::extract_omp_cwd(&file);
    assert_eq!(cwd.as_deref(), Some("/Volumes/Data/projects/token-watcher"));
}

#[test]
fn test_omp_watcher_lock_and_dir_linkage() {
    use notify::Event;
    use std::sync::mpsc::channel;
    use token_watcher_lib::watch::{DataEngine, DataPaths, EventSink};

    struct DummySink;
    impl EventSink for DummySink {
        fn is_visible(&self) -> bool {
            true
        }
        fn emit_json(&self, _event: &str, _payload: serde_json::Value) {}
    }

    let temp = TempDir::new("omp-watch");
    let paths = DataPaths {
        projects_root: temp.path().join("projects"),
        claude_dir: temp.path().join("claude"),
        settings_path: temp.path().join("settings.json"),
        codex_sessions_root: temp.path().join("codex_sessions"),
        codex_dir: temp.path().join("codex"),
        codex_config_path: temp.path().join("config.toml"),
        cline_tasks_root: temp.path().join("cline_tasks"),
        roo_tasks_root: temp.path().join("roo_tasks"),
        aider_analytics_path: temp.path().join("aider.jsonl"),
        opencode_db_path: temp.path().join("opencode.db"),
        continue_sessions_root: temp.path().join("continue_sessions"),
        omp_sessions_root: temp.path().join("omp_sessions"),
        db_path: temp.path().join("db.sqlite3"),
    };
    let (tx, _rx) = channel();
    let mut engine = DataEngine::new(paths.clone(), DummySink, tx);

    let proj_dir = paths
        .omp_sessions_root
        .join("--Volumes-Data-projects-token-watcher--");
    fs::create_dir_all(&proj_dir).unwrap();
    let jsonl_file = proj_dir.join("2026-10-21T07-27-26-821Z_sess1.jsonl");
    fs::write(
        &jsonl_file,
        "{\"type\":\"session\",\"cwd\":\"/Volumes/Data/projects/token-watcher\"}\n",
    )
    .unwrap();

    // 1. 模拟 .lock.os 事件到达
    let lock_file = proj_dir.join(".2026-10-21T07-27-26-821Z_sess1.jsonl.lock.os");
    fs::write(&lock_file, "").unwrap();
    let lock_ev = Event {
        kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
        paths: vec![lock_file.clone()],
        attrs: Default::default(),
    };
    engine.handle(token_watcher_lib::watch::Message::Fs(lock_ev));
    assert!(
        engine.pending_files_contains(&jsonl_file),
        "锁文件事件应成功联动 .jsonl 文件"
    );

    // 2. 模拟工具日志子目录文件事件到达
    let tool_dir = proj_dir.join("2026-10-21T07-27-26-821Z_sess1");
    fs::create_dir_all(&tool_dir).unwrap();
    let tool_log = tool_dir.join("3.bash.log");
    fs::write(&tool_log, "tool output").unwrap();
    let tool_ev = Event {
        kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
        paths: vec![tool_log],
        attrs: Default::default(),
    };
    engine.handle(token_watcher_lib::watch::Message::Fs(tool_ev));
    assert!(
        engine.pending_files_contains(&jsonl_file),
        "工具日志文件事件应成功联动对应 .jsonl 会话文件"
    );
}

#[test]
fn test_detect_engine_coverage() {
    assert_eq!(
        detect_engine("opencode:ses_1:msg_1", "opencode"),
        "opencode"
    );
    assert_eq!(
        detect_engine("continue:ses_2:0:123", "continue"),
        "continue"
    );
    assert_eq!(detect_engine("omp:ses_3:msg_2", "omp"), "omp");
    assert_eq!(detect_engine("cline:task_1:123:abc", "cline"), "cline");
    assert_eq!(detect_engine("roo:task_2:123:abc", "roo"), "roo");
    assert_eq!(detect_engine("aider:u1:123:abc", "aider"), "aider");
    assert_eq!(detect_engine("codex:s1:turn1", "codex"), "codex");
    assert_eq!(
        detect_engine("11111111-1111-4111-8111-111111111111", "claude"),
        "claude"
    );
}

#[test]
fn test_data_engine_multi_source_v2_discovery() {
    let temp = TempDir::new("engine-v2");
    let paths = DataPaths::for_test(temp.path());

    // 创建测试目录与文件
    fs::create_dir_all(&paths.projects_root).unwrap();
    fs::create_dir_all(&paths.continue_sessions_root).unwrap();
    fs::create_dir_all(&paths.omp_sessions_root).unwrap();

    // 1. Continue session
    let continue_session_file = paths.continue_sessions_root.join("test-sess.json");
    fs::write(
        &continue_session_file,
        json!({
            "sessionId": "test-sess",
            "history": [
                {
                    "message": { "role": "assistant" },
                    "promptLogs": [{ "promptTokens": 500, "completionTokens": 100, "modelTitle": "gpt-5" }]
                }
            ]
        }).to_string(),
    ).unwrap();

    // 2. Omp session
    let omp_session_file = paths.omp_sessions_root.join("omp-1.jsonl");
    fs::write(
        &omp_session_file,
        format!(
            "{}\n",
            json!({
                "type": "message",
                "id": "o1",
                "message": {
                    "role": "assistant",
                    "model": "deepseek-v3",
                    "usage": { "input": 800, "output": 200, "cacheRead": 0, "cacheWrite": 0 }
                }
            })
        ),
    )
    .unwrap();

    // 3. OpenCode db
    let conn = rusqlite::Connection::open(&paths.opencode_db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);"
    ).unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "op1",
            "s1",
            1777277000000i64,
            1777277000000i64,
            json!({
                "role": "assistant",
                "modelID": "claude-sonnet-4.6",
                "tokens": { "total": 900, "input": 700, "output": 200 }
            }).to_string()
        ],
    ).unwrap();

    let (tx, _rx) = std::sync::mpsc::channel();
    let mut engine = DataEngine::new(paths.clone(), MockSink, tx);
    engine.initial_scan();

    // 检查 Store 中各引擎记录是否齐全落库
    let store = Store::open(&paths.db_path).unwrap();
    let totals = store.totals_between(0, i64::MAX).unwrap();
    assert_eq!(totals.input, 500 + 800 + 700);
    assert_eq!(totals.output, 100 + 200 + 200);
}
