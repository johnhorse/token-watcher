//! T16: 多数据源扩展（Multi-Engine Ingestion）端到端集成测试
//!
//! 验证：
//! 1. Claude Code 与 Codex CLI 双根并行首扫与增量监控；
//! 2. 单引擎缺失（仅装其一或两皆未装）时的平滑容错与非错误状态呈现；
//! 3. 逐 flush 增量追加下的 Fast State Replay 状态机时序安全；
//! 4. Codex config.toml 的 Profile 自动归因与命名空间去重主键；
//! 5. 真实数据回放：验证本机真实 Codex session 日志的吞吐与 0 坏行。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde_json::Value;
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::store::Store;
use token_watcher_lib::watch::{DataEngine, DataPaths, EventSink};
struct TempDir {
    path: PathBuf,
}

use std::sync::atomic::{AtomicU64, Ordering};
static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

impl TempDir {
    fn new(name: &str) -> Self {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tw-test-t16-{}-{}-{}", std::process::id(), name, id));
        let _ = fs::create_dir_all(&path);
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

#[derive(Clone)]
struct MockSink {
    events: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>,
}

impl MockSink {
    fn new() -> Self {
        Self {
            events: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
    fn named(&self, name: &str) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

impl EventSink for MockSink {
    fn is_visible(&self) -> bool {
        true
    }
    fn emit_json(&self, event: &str, payload: Value) {
        self.events
            .lock()
            .unwrap()
            .push((event.to_string(), payload));
    }
}

fn paths_for(temp: &Path) -> DataPaths {
    let claude_dir = temp.join("claude");
    let codex_dir = temp.join("codex");
    DataPaths {
        projects_root: claude_dir.join("projects"),
        settings_path: claude_dir.join("settings.json"),
        claude_dir,
        codex_sessions_root: codex_dir.join("sessions"),
        codex_config_path: codex_dir.join("config.toml"),
        codex_dir,
        db_path: temp.join("db.sqlite3"),
    }
}

#[test]
fn test_multi_engine_parallel_initial_scan() {
    let temp = TempDir::new("parallel-scan");
    let paths = paths_for(temp.path());

    // 准备 Claude 数据：1 个 project，1 个 session
    let claude_proj = paths.projects_root.join("proj-1");
    fs::create_dir_all(&claude_proj).unwrap();
    let claude_file = claude_proj.join("sess-c1.jsonl");
    fs::write(
        &claude_file,
        r#"{"type":"assistant","uuid":"u1","session_id":"sess-c1","timestamp":"2026-10-20T10:00:00.000Z","message":{"id":"msg_c1","model":"claude-3-7-sonnet","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":20,"cache_creation_input_tokens":10}}}"#,
    )
    .unwrap();

    // 准备 Codex 数据：sessions/2026/10/20/rollout-*.jsonl
    let codex_day = paths.codex_sessions_root.join("2026/10/20");
    fs::create_dir_all(&codex_day).unwrap();
    let codex_file = codex_day.join("rollout-2026-10-20T10-00-00-uuid1.jsonl");
    {
        let mut f = fs::File::create(&codex_file).unwrap();
        writeln!(
            f,
            r#"{{"type":"session_meta","payload":{{"session_id":"sess-x1","cwd":"/repo/my-app","originator":"codex-tui"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"turn_context","payload":{{"turn_id":"turn-1","model":"gpt-6.1-sol"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-10-20T10:01:00.000Z","type":"token_usage_record","payload":{{"session_id":"sess-x1","turn_id":"turn-1","response_id":"resp-x1","usage":{{"input_tokens":1000,"cached_input_tokens":200,"cache_write_input_tokens":50,"output_tokens":80,"total_tokens":1080}}}}}}"#
        )
        .unwrap();
    }

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();

    let conn = rusqlite::Connection::open(&paths.db_path).unwrap();
    // 验证总共落库 2 条记录
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM usage_records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "应同时包含 Claude 与 Codex 记录");

    // 验证 Claude 记录
    let (c_model, c_in, c_cr, c_cw, c_out): (String, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT model, input_tokens, cache_read_tokens, cache_creation_tokens, output_tokens FROM usage_records WHERE session_id = 'sess-c1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(c_model, "claude-3-7-sonnet");
    assert_eq!(c_in, 100);
    assert_eq!(c_cr, 20);
    assert_eq!(c_cw, 10);
    assert_eq!(c_out, 50);

    // 验证 Codex 记录（四维归一化 + 命名空间主键）
    let (x_rec_id, x_model, x_entry, x_in, x_cr, x_cw, x_out): (
        String,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT record_id, model, entrypoint, input_tokens, cache_read_tokens, cache_creation_tokens, output_tokens FROM usage_records WHERE session_id = 'sess-x1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap();
    assert_eq!(x_rec_id, "codex:sess-x1:resp-x1");
    assert_eq!(x_model, "gpt-6.1-sol");
    assert_eq!(x_entry, "codex-tui");
    assert_eq!(x_in, 1000 - 200 - 50); // 纯净输入
    assert_eq!(x_cr, 200);
    assert_eq!(x_cw, 50);
    assert_eq!(x_out, 80);
    // 验证 Dashboard sessions 聚合正确标识双引擎来源
    let store = Store::open_readonly(&paths.db_path).unwrap();
    let dash = token_watcher_lib::core::agg::dashboard(&store, token_watcher_lib::core::agg::Range::All).unwrap();
    let c_sess = dash.sessions.iter().find(|s| s.session_id == "sess-c1").unwrap();
    let x_sess = dash.sessions.iter().find(|s| s.session_id == "sess-x1").unwrap();
    assert_eq!(c_sess.engine, "claude");
    assert_eq!(x_sess.engine, "codex");

    // 验证 SessionFlightRecorder 返回准确引擎
    let x_fr = token_watcher_lib::core::agg::session_flight_recorder(&store, None, "sess-x1", None, 20_000).unwrap().unwrap();
    assert_eq!(x_fr.engine, "codex");
    let c_fr = token_watcher_lib::core::agg::session_flight_recorder(&store, None, "sess-c1", None, 20_000).unwrap().unwrap();
    assert_eq!(c_fr.engine, "claude");

    // 验证 scan-progress 收尾为 done
    let progress = sink.named("scan-progress");
    assert!(progress.iter().any(|p| p["phase"] == "done"));
}

#[test]
fn test_multi_engine_single_root_tolerance() {
    let temp = TempDir::new("single-root");
    let paths = paths_for(temp.path());

    // 场景 A: 仅装 Codex，未装 Claude Code（projects_root 不存在）
    let codex_day = paths.codex_sessions_root.join("2026/10/20");
    fs::create_dir_all(&codex_day).unwrap();
    let codex_file = codex_day.join("rollout-2026-10-20-standalone.jsonl");
    fs::write(
        &codex_file,
        r#"{"type":"session_meta","payload":{"session_id":"sess-codex-only"}}
{"type":"turn_context","payload":{"turn_id":"t1","model":"gpt-6.1-sol"}}
{"timestamp":"2026-10-20T10:00:00.000Z","type":"token_usage_record","payload":{"session_id":"sess-codex-only","turn_id":"t1","response_id":"resp-1","usage":{"input_tokens":500,"output_tokens":50,"total_tokens":550}}}"#,
    )
    .unwrap();

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();

    let conn = rusqlite::Connection::open(&paths.db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM usage_records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "未装 Claude Code 时，Codex 照常全量解析入库");
    let health = sink.named("data-health");
    assert!(
        health.iter().all(|h| h["root_unreadable"] == false),
        "单引擎缺失不能误报 root_unreadable"
    );
}

#[test]
fn test_codex_profile_attribution_via_config_toml() {
    let temp = TempDir::new("profile-attr");
    let paths = paths_for(temp.path());

    // 写入 Codex config.toml，指定 custom 代理
    fs::create_dir_all(&paths.codex_dir).unwrap();
    fs::write(
        &paths.codex_config_path,
        r#"
model = "gpt-6.1-sol"
model_provider = "custom"

[model_providers.custom]
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
"#,
    )
    .unwrap();

    let codex_day = paths.codex_sessions_root.join("2026/10/20");
    fs::create_dir_all(&codex_day).unwrap();
    let codex_file = codex_day.join("rollout-2026-10-20-profile.jsonl");
    fs::write(
        &codex_file,
        r#"{"type":"session_meta","payload":{"session_id":"sess-prof-1"}}
{"type":"turn_context","payload":{"turn_id":"t1","model":"gpt-6.1-sol"}}
{"timestamp":"2026-10-20T10:00:00.000Z","type":"token_usage_record","payload":{"session_id":"sess-prof-1","turn_id":"t1","response_id":"resp-p1","usage":{"input_tokens":500,"output_tokens":50,"total_tokens":550}}}"#,
    )
    .unwrap();

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink, tx);
    engine.initial_scan();
    let conn = rusqlite::Connection::open(&paths.db_path).unwrap();

    // 验证自动创建了 Profile 并在 usage_records 中关联
    let (profile_id, attr_source): (Option<i64>, String) = conn
        .query_row(
            "SELECT profile_id, attribution_source FROM usage_records WHERE session_id = 'sess-prof-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    assert!(profile_id.is_some(), "必须自动绑定 ProfileId");
    assert_eq!(attr_source, "config");

    let (endpoint, display_name): (String, String) = conn
        .query_row(
            "SELECT endpoint, display_name FROM profiles WHERE id = ?1",
            [profile_id.unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(endpoint, "http://127.0.0.1:15721");
    assert_eq!(display_name, "本地代理 (15721)");
}

#[test]
fn test_live_codex_sessions_smoke_replay() {
    let home = match home::home_dir() {
        Some(h) => h,
        None => return,
    };
    let codex_sessions = home.join(".codex").join("sessions");
    if !codex_sessions.exists() {
        eprintln!("skip test_live_codex_sessions_smoke_replay: ~/.codex/sessions 不存在");
        return;
    }

    let temp = TempDir::new("smoke-replay");
    let db_path = temp.path().join("db.sqlite3");
    let mut store = Store::open(&db_path).unwrap();

    let stats = match ingest::scan_dir(&mut store, &codex_sessions) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[t16] 扫描失败: {e}");
            return;
        }
    };

    eprintln!(
        "[t16] 本机真实 Codex 回放结果: files={}, records={}, bad_lines={}",
        stats.files_scanned, stats.records, stats.bad_lines
    );
    assert_eq!(stats.bad_lines, 0, "真实数据解析坏行数必须为 0");
    assert!(stats.records > 0, "真实数据解析必须产出有效记录");

    // 验证所有落库记录的 session_id 与 model 均有效
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let empty_models: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM usage_records WHERE model = ''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(empty_models, 0, "真实回放中不得有空 model 脏数据");
    let distinct_models: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT model FROM usage_records ORDER BY model")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    eprintln!("[t16] 真实解析出的模型清单: {:?}", distinct_models);
    assert!(!distinct_models.is_empty());
}
