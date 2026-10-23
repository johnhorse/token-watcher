//! T21: v0.7.2 可靠性收尾 —— T1（根事件单根补扫）与 T2（兜底轮询矩阵 + 语义修订）
//!
//! 覆盖测试计划十项：
//! 1. 根目录事件只触发该根补扫（T1 / R8）
//! 2. 无 fs 事件时轮询捡起新增 JSONL 字节（①）
//! 3. 主库不变仅 WAL 增长：事件路径 + 轮询路径都能捡起（② / R1）
//! 4. 空闲 60s 慢扫捡起新增字节（P3 自举悖论兜底）
//! 5. 丢 Create 被⑤根轻扫发现，含非 JSONL 源；并断言⑤独立 ≥30s 节拍（R3）
//! 6. 持续事件流下首批文件 ≤2s 入账（R6）
//! 7. 同数量原地更新仍通知 UI（R7）
//! 8. OpenCode 同时间戳第二条行入账（R5）
//! 9. OpenCode 事后补 usage 行入账（R5）
//! 10. ⑤文件数超入队上限，轮转续扫最终全部入账（R9）

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::store::Store;
use token_watcher_lib::watch::{
    DataEngine, DataPaths, EventSink, Message, DEBOUNCE_MAX, POLL_FAST, POLL_IDLE,
    ROOT_SCAN_ENQUEUE_CAP,
};

// -----------------------------------------------------------------------------
// Harness
// -----------------------------------------------------------------------------

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("tw-v072-{}-{}", name, std::process::id()));
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

#[derive(Clone, Default)]
struct RecordingSink {
    events: Arc<Mutex<Vec<String>>>,
}

impl RecordingSink {
    fn named(&self, name: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| *event == name)
            .count()
    }
}

impl EventSink for RecordingSink {
    fn is_visible(&self) -> bool {
        true
    }
    fn emit_json(&self, event: &str, _payload: serde_json::Value) {
        self.events.lock().unwrap().push(event.to_string());
    }
}

/// 测试用路径：Antigravity 根按真实目录名（含 /antigravity/）命名，
/// 保证 detect_file_kind 能按路径识别 .db 源。
fn test_paths(temp: &TempDir) -> DataPaths {
    let mut paths = DataPaths::for_test(temp.path());
    paths.antigravity_conversations_root = temp.path().join("antigravity/conversations");
    paths
}

fn engine_for(paths: &DataPaths) -> (DataEngine<RecordingSink>, RecordingSink, Receiver<Message>) {
    let sink = RecordingSink::default();
    let (tx, rx) = mpsc::channel();
    let engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    (engine, sink, rx)
}

fn usage_line(uuid: &str, session: &str, timestamp: &str, input: i64, output: i64) -> String {
    json!({
        "type": "assistant",
        "uuid": uuid,
        "session_id": session,
        "sessionId": session,
        "timestamp": timestamp,
        "message": {
            "model": "glm-5.3",
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0
            }
        }
    })
    .to_string()
}

fn codex_lines(session: &str, response: &str, input: i64, output: i64) -> String {
    format!(
        "{}\n{}\n{}\n",
        json!({
            "type": "session_meta",
            "payload": {"session_id": session, "cwd": "/tmp/demo", "originator": "codex-tui"}
        }),
        json!({
            "type": "turn_context",
            "payload": {"turn_id": "t-1", "model": "gpt-6.1-sol"}
        }),
        json!({
            "timestamp": "2026-10-23T02:00:00.000Z",
            "type": "token_usage_record",
            "payload": {
                "session_id": session,
                "turn_id": "t-1",
                "response_id": response,
                "usage": {"input_tokens": input, "output_tokens": output, "total_tokens": input + output}
            }
        })
    )
}

fn cline_ui_messages(tokens_in: i64, tokens_out: i64) -> String {
    json!([{
        "type": "say",
        "say": "api_req_started",
        "ts": 1761516645000i64,
        "text": json!({
            "tokensIn": tokens_in,
            "tokensOut": tokens_out,
            "cacheReads": 0,
            "cacheWrites": 0,
            "cost": 0.01,
            "apiProtocol": "anthropic"
        }).to_string()
    }])
    .to_string()
}

fn cline_history(model: &str) -> String {
    format!("<environment_details>\n<model>{model}</model>\n</environment_details>\n")
}

fn file_event(path: &Path) -> notify::Event {
    notify::Event {
        kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
        paths: vec![path.to_path_buf()],
        attrs: Default::default(),
    }
}

fn dir_event(path: &Path) -> notify::Event {
    notify::Event {
        kind: notify::EventKind::Create(notify::event::CreateKind::Any),
        paths: vec![path.to_path_buf()],
        attrs: Default::default(),
    }
}

fn antigravity_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/antigravity/sample_conversation.db")
}

fn db_fingerprint(path: &Path) -> (u64, u128) {
    let metadata = fs::metadata(path).unwrap();
    let mtime = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    (metadata.len(), mtime)
}

fn wal_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", db.display()))
}

/// 复制 fixture 中第 `src` 行的 blob 到新 idx（制造一条合法的 WAL-only 新记录）
fn copy_antigravity_row(writer: &rusqlite::Connection, idx: i64, src: i64) {
    writer
        .execute(
            "INSERT OR REPLACE INTO gen_metadata (idx, data)
             SELECT ?1, data FROM gen_metadata WHERE idx = ?2",
            rusqlite::params![idx, src],
        )
        .unwrap();
    writer
        .execute(
            "INSERT OR REPLACE INTO steps (idx, metadata)
             SELECT ?1, metadata FROM steps WHERE idx = ?2",
            rusqlite::params![idx, src],
        )
        .unwrap();
}

fn open_opencode_db(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            data TEXT NOT NULL
        );",
    )
    .unwrap();
    conn
}

fn opencode_data(input: i64, output: i64) -> String {
    json!({
        "role": "assistant",
        "modelID": "claude-sonnet-4.6",
        "tokens": {"input": input, "output": output, "cache": {"read": 0, "write": 0}},
        "path": {"cwd": "/projects/demo"}
    })
    .to_string()
}

fn insert_opencode_message(
    conn: &rusqlite::Connection,
    id: &str,
    session: &str,
    ts: i64,
    data: &str,
) {
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        rusqlite::params![id, session, ts, data],
    )
    .unwrap();
}

// -----------------------------------------------------------------------------
// 1. T1：根目录事件只补扫该根（R8）
// -----------------------------------------------------------------------------
#[test]
fn test_root_dir_event_triggers_single_root_catchup() {
    let temp = TempDir::new("root-catchup");
    let paths = test_paths(&temp);

    let proj_dir = paths.projects_root.join("proj-a");
    fs::create_dir_all(&proj_dir).unwrap();
    fs::write(
        proj_dir.join("s-1.jsonl"),
        format!(
            "{}\n",
            usage_line("u-1", "s-1", "2026-10-23T01:00:00Z", 100, 10)
        ),
    )
    .unwrap();

    let codex_dir = paths.codex_sessions_root.join("2026/10/23");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
        codex_dir.join("rollout-a.jsonl"),
        codex_lines("s-codex-a", "resp-a", 50, 5),
    )
    .unwrap();

    let (mut engine, _sink, _rx) = engine_for(&paths);
    engine.initial_scan();
    assert_eq!(engine.record_count(), 2, "首扫应入库两个根的记录");

    // 两个根各静默出现一个新文件（无任何 fs 事件）
    fs::write(
        proj_dir.join("s-2.jsonl"),
        format!(
            "{}\n",
            usage_line("u-2", "s-2", "2026-10-23T01:01:00Z", 200, 20)
        ),
    )
    .unwrap();
    let codex_new = codex_dir.join("rollout-b.jsonl");
    fs::write(&codex_new, codex_lines("s-codex-b", "resp-b", 60, 6)).unwrap();

    // 只给 projects 根一个目录事件
    engine.handle(Message::Fs(dir_event(&paths.projects_root)));
    engine.flush();

    assert_eq!(
        engine.record_count(),
        3,
        "根目录事件只应补扫事件所属的 projects 根"
    );
    assert!(
        !engine.pending_files_contains(&codex_new),
        "其他根的待扫文件不得被入队（不得回归为全根补扫）"
    );

    // 再给 codex 根事件 → 该根同样被补扫
    engine.handle(Message::Fs(dir_event(&paths.codex_sessions_root)));
    engine.flush();
    assert_eq!(engine.record_count(), 4);
}

// -----------------------------------------------------------------------------
// 2. 无 fs 事件：轮询①捡起新增 JSONL 字节
// -----------------------------------------------------------------------------
#[test]
fn test_poll_picks_up_new_jsonl_bytes_without_fs_event() {
    let temp = TempDir::new("poll-jsonl");
    let paths = test_paths(&temp);
    let proj_dir = paths.projects_root.join("proj-a");
    fs::create_dir_all(&proj_dir).unwrap();
    let file = proj_dir.join("s-1.jsonl");
    fs::write(
        &file,
        format!(
            "{}\n",
            usage_line("u-1", "s-1", "2026-10-23T01:00:00Z", 100, 10)
        ),
    )
    .unwrap();

    let (mut engine, _sink, _rx) = engine_for(&paths);
    engine.initial_scan();
    assert_eq!(engine.record_count(), 1);

    // 追加字节，但事件被吞（不调用 handle）
    let mut content = fs::read_to_string(&file).unwrap();
    content.push_str(&usage_line("u-2", "s-1", "2026-10-23T01:02:00Z", 200, 20));
    content.push('\n');
    fs::write(&file, content).unwrap();

    engine.poll_now();
    assert_eq!(
        engine.record_count(),
        2,
        "① stat size vs byte_offset 必须发现未消费的追加字节"
    );
}

// -----------------------------------------------------------------------------
// 3. 主库不变仅 WAL 增长：事件路径 + 轮询路径双覆盖（R1）
// -----------------------------------------------------------------------------
#[test]
fn test_wal_only_growth_picked_up_on_event_and_poll_paths() {
    let temp = TempDir::new("wal-only");
    let paths = test_paths(&temp);
    fs::create_dir_all(&paths.antigravity_conversations_root).unwrap();
    let db_path = paths.antigravity_conversations_root.join("conv-1.db");
    fs::copy(antigravity_fixture(), &db_path).unwrap();

    // 切 WAL 且关闭自动 checkpoint：后续写入只增长 -wal，主库 stat 不变
    let writer = rusqlite::Connection::open(&db_path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();

    let (mut engine, _sink, _rx) = engine_for(&paths);
    engine.initial_scan();
    assert_eq!(engine.record_count(), 5, "fixture 应解出 5 条记录");

    let main_before = db_fingerprint(&db_path);
    copy_antigravity_row(&writer, 5, 0);
    let main_after = db_fingerprint(&db_path);
    assert_eq!(
        main_before, main_after,
        "WAL-only 写入不得改变主库 mtime/size（否则测不到短路绕过）"
    );

    // 路径 A：.db-wal 事件 → 事件入队路径必须同时标记强制重读
    engine.handle(Message::Fs(file_event(&wal_path(&db_path))));
    engine.flush();
    assert_eq!(
        engine.record_count(),
        6,
        ".db-wal 事件路径必须绕过 Antigravity 主库 (mtime,size) 短路"
    );

    // 路径 B：无事件，WAL 再增长 → ② 指纹不匹配触发强制重读
    copy_antigravity_row(&writer, 6, 0);
    engine.poll_now();
    assert_eq!(
        engine.record_count(),
        7,
        "轮询路径也必须绕过短路（WAL 指纹不匹配 → force-read）"
    );
}

// -----------------------------------------------------------------------------
// 4. 空闲 60s 慢扫捡起新增字节
// -----------------------------------------------------------------------------
#[test]
fn test_idle_slow_scan_picks_up_new_bytes() {
    let temp = TempDir::new("idle-slow");
    let paths = test_paths(&temp);
    let proj_dir = paths.projects_root.join("proj-a");
    fs::create_dir_all(&proj_dir).unwrap();
    let file = proj_dir.join("s-1.jsonl");
    fs::write(
        &file,
        format!(
            "{}\n",
            usage_line("u-1", "s-1", "2026-10-23T01:00:00Z", 100, 10)
        ),
    )
    .unwrap();

    let (mut engine, _sink, _rx) = engine_for(&paths);
    engine.initial_scan();
    assert_eq!(engine.record_count(), 1);

    let mut content = fs::read_to_string(&file).unwrap();
    content.push_str(&usage_line("u-2", "s-1", "2026-10-23T01:02:00Z", 300, 30));
    content.push('\n');
    fs::write(&file, content).unwrap();

    // 模拟空闲 5 分钟以上：进入 60s 慢扫档
    engine.force_idle();
    engine.poll_now();
    assert_eq!(
        engine.record_count(),
        2,
        "空闲期慢扫必须执行同一检查矩阵（自举悖论兜底）"
    );
    assert!(
        engine.next_poll_due_in() >= POLL_IDLE - Duration::from_secs(2),
        "空闲期节拍必须放宽到 60s"
    );
}

// -----------------------------------------------------------------------------
// 5. 丢 Create 被⑤发现（非 JSONL 源）+ R3 独立 30s 节拍
// -----------------------------------------------------------------------------
#[test]
fn test_missed_create_discovered_by_root_scan() {
    let temp = TempDir::new("missed-create");
    let paths = test_paths(&temp);
    fs::create_dir_all(&paths.cline_tasks_root).unwrap();
    fs::create_dir_all(&paths.antigravity_conversations_root).unwrap();

    let (mut engine, _sink, _rx) = engine_for(&paths);
    engine.initial_scan();
    assert_eq!(engine.record_count(), 0);

    // 非 JSONL 源：新 Cline 任务（③ 只轮询已登记文件，① 只认 JSONL，
    // 只有⑤能发现它），且没有任何 fs 事件
    let task = paths.cline_tasks_root.join("task-1");
    fs::create_dir_all(&task).unwrap();
    fs::write(task.join("ui_messages.json"), cline_ui_messages(5000, 200)).unwrap();
    fs::write(
        task.join("api_conversation_history.json"),
        cline_history("claude-3-7-sonnet"),
    )
    .unwrap();

    // 非 JSONL 源：新 Antigravity 库
    let new_ag = paths.antigravity_conversations_root.join("conv-new.db");
    fs::copy(antigravity_fixture(), &new_ag).unwrap();

    // 3s 快档（⑤ 未到期）：Cline 新任务不得被发现
    engine.poll_now();
    assert!(
        engine.root_scan_due_in() > POLL_FAST * 2,
        "⑤ 必须保持独立 ≥30s 节拍，不得跟 3s 档（R3）"
    );
    assert_eq!(
        engine.record_count(),
        5,
        "3s 快档不得跑⑤；此时只应有②首观测捡起的 5 条 Antigravity 记录"
    );

    // ⑤ 到期：Cline 新任务 + Antigravity 新库都被根轻扫发现
    engine.make_root_scan_due();
    engine.poll_now();
    assert_eq!(
        engine.record_count(),
        6,
        "⑤ 根轻扫必须发现被吞 Create 的非 JSONL 源（Cline ui_messages.json）"
    );
}

// -----------------------------------------------------------------------------
// 6. 持续事件流下首批 ≤2s 入账（R6）
// -----------------------------------------------------------------------------
#[test]
fn test_debounce_batch_max_wait_flushes_within_2s() {
    let temp = TempDir::new("debounce-max");
    let paths = test_paths(&temp);
    let proj_dir = paths.projects_root.join("proj-a");
    fs::create_dir_all(&proj_dir).unwrap();
    let file = proj_dir.join("s-1.jsonl");
    fs::write(
        &file,
        format!(
            "{}\n",
            usage_line("u-1", "s-1", "2026-10-23T01:00:00Z", 100, 10)
        ),
    )
    .unwrap();

    let (mut engine, _sink, _rx) = engine_for(&paths);
    engine.initial_scan();
    assert_eq!(engine.record_count(), 1);

    // 追加内容后进入持续事件流（每 100ms 一个事件，持续 2.5s）
    let mut content = fs::read_to_string(&file).unwrap();
    content.push_str(&usage_line("u-2", "s-1", "2026-10-23T01:02:00Z", 200, 20));
    content.push('\n');
    fs::write(&file, content).unwrap();

    let t0 = Instant::now();
    engine.handle(Message::Fs(file_event(&file)));
    while t0.elapsed() < Duration::from_millis(2500) {
        thread::sleep(Duration::from_millis(100));
        engine.handle(Message::Fs(file_event(&file)));
    }

    let deadline = engine
        .next_deadline()
        .expect("有待处理批次时必须有到期时刻");
    assert!(
        deadline <= t0 + DEBOUNCE_MAX + Duration::from_millis(100),
        "去抖批次最大等待 2s 必须封顶持续事件流（当前 deadline 距首事件 {:?}）",
        deadline.saturating_duration_since(t0)
    );

    thread::sleep(deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20));
    engine.on_tick();
    assert_eq!(
        engine.record_count(),
        2,
        "到期必 flush：持续事件流下首批文件也必须落库"
    );
}

// -----------------------------------------------------------------------------
// 7. 同数量原地更新仍通知 UI（R7）
// -----------------------------------------------------------------------------
#[test]
fn test_in_place_update_emits_dirty() {
    let temp = TempDir::new("in-place-update");
    let paths = test_paths(&temp);
    let task = paths.cline_tasks_root.join("task-1");
    fs::create_dir_all(&task).unwrap();
    let ui_path = task.join("ui_messages.json");
    fs::write(&ui_path, cline_ui_messages(5000, 200)).unwrap();
    let history_path = task.join("api_conversation_history.json");
    fs::write(&history_path, cline_history("claude-3-7-sonnet")).unwrap();

    let (mut engine, sink, _rx) = engine_for(&paths);
    engine.initial_scan();
    assert_eq!(engine.record_count(), 1);
    let dirty_before = sink.named("dashboard-dirty");
    thread::sleep(Duration::from_millis(10));

    // 记录数不变的原地更新：usage 行数仍为 1（内容哈希主键不变，D26 不动），
    // 但模型变化 + 文件追加非用量条目 → 整文件重写。
    fs::write(&history_path, cline_history("gpt-6.1-sol")).unwrap();
    let mut entries: serde_json::Value =
        serde_json::from_str(&cline_ui_messages(5000, 200)).unwrap();
    entries.as_array_mut().unwrap().push(
        json!({"type": "say", "say": "task_progress", "ts": 1761516650000i64, "text": "done"}),
    );
    fs::write(&ui_path, entries.to_string()).unwrap();

    engine.poll_now();

    assert_eq!(
        engine.record_count(),
        1,
        "记录数不变：D26 内容哈希主键保持不动，同一行被 UPSERT 覆盖"
    );
    assert!(
        sink.named("dashboard-dirty") > dirty_before,
        "同数量原地更新也必须通知 UI（emit 由 upsert changed 行数驱动）"
    );
}

// -----------------------------------------------------------------------------
// 8. OpenCode 同时间戳第二条行入账（R5）
// -----------------------------------------------------------------------------
#[test]
fn test_opencode_same_timestamp_second_row_ingested() {
    let temp = TempDir::new("opencode-same-ts");
    let db_path = temp.path().join("opencode.db");
    let writer = open_opencode_db(&db_path);
    insert_opencode_message(
        &writer,
        "msg_a",
        "ses_1",
        1777275000000,
        &opencode_data(100, 10),
    );

    let mut store = Store::open(&temp.path().join("db.sqlite3")).unwrap();
    let (stats1, _records1) = ingest::ingest_file_with_records(&mut store, &db_path).unwrap();
    assert_eq!(stats1.records, 1);
    assert_eq!(store.count_records().unwrap(), 1);

    // 同毫秒并列行（并行会话）：旧游标 `>` 会永久跳过；
    // `>=` 会重读边界行（幂等）+ 读到并列新行
    insert_opencode_message(
        &writer,
        "msg_b",
        "ses_1",
        1777275000000,
        &opencode_data(200, 20),
    );
    let (stats2, records2) = ingest::ingest_file_with_records(&mut store, &db_path).unwrap();
    assert_eq!(stats2.records, 2, "游标必须含边界（>=），边界行幂等重读");
    assert!(
        records2
            .iter()
            .any(|record| record.record_id == "opencode:ses_1:msg_b"),
        "同时间戳并列的第二条行必须入账"
    );
    assert_eq!(store.count_records().unwrap(), 2, "边界重读不得双计");
}

// -----------------------------------------------------------------------------
// 9. OpenCode 事后补 usage 行入账（R5）
// -----------------------------------------------------------------------------
#[test]
fn test_opencode_late_usage_row_ingested() {
    let temp = TempDir::new("opencode-late-usage");
    let db_path = temp.path().join("opencode.db");
    let writer = open_opencode_db(&db_path);
    insert_opencode_message(
        &writer,
        "msg_a",
        "ses_1",
        1777275000000,
        &opencode_data(100, 10),
    );

    let mut store = Store::open(&temp.path().join("db.sqlite3")).unwrap();
    let (stats1, _) = ingest::ingest_file_with_records(&mut store, &db_path).unwrap();
    assert_eq!(stats1.records, 1);

    // 先落一行暂无 usage 的 assistant 消息：解析无记录，但游标被推进到该时间戳
    let no_usage = json!({"role": "assistant", "modelID": "claude-sonnet-4.6"}).to_string();
    insert_opencode_message(&writer, "msg_late", "ses_1", 1777275000001, &no_usage);
    let (stats2, records2) = ingest::ingest_file_with_records(&mut store, &db_path).unwrap();
    assert_eq!(stats2.records, 1, "只有边界行（msg_a）被幂等重读");
    assert!(
        records2
            .iter()
            .all(|record| record.record_id != "opencode:ses_1:msg_late"),
        "无 usage 的行暂不产生记录"
    );

    // 事后补上 usage（时间戳不变）：旧游标已越过该行，`>=` 才能重读
    writer
        .execute(
            "UPDATE message SET data = ?1 WHERE id = 'msg_late'",
            rusqlite::params![opencode_data(300, 30)],
        )
        .unwrap();
    let (stats3, records3) = ingest::ingest_file_with_records(&mut store, &db_path).unwrap();
    assert_eq!(stats3.records, 1, "事后补 usage 行必须入账");
    assert_eq!(records3[0].record_id, "opencode:ses_1:msg_late");
    assert_eq!(store.count_records().unwrap(), 2);
}

// -----------------------------------------------------------------------------
// 10. ⑤文件数超入队上限：轮转续扫最终全部入账（R9）
// -----------------------------------------------------------------------------
#[test]
fn test_root_scan_cap_rotation_ingests_all() {
    let temp = TempDir::new("root-scan-cap");
    let paths = test_paths(&temp);
    fs::create_dir_all(&paths.projects_root).unwrap();

    let (mut engine, _sink, _rx) = engine_for(&paths);
    engine.initial_scan();
    assert_eq!(engine.record_count(), 0);

    // 静默灌入超过单轮上限的新文件（无事件，只有⑤能发现）
    let total = ROOT_SCAN_ENQUEUE_CAP + 50;
    for index in 0..total {
        let uuid = format!("u-{index:04}");
        fs::write(
            paths.projects_root.join(format!("{uuid}.jsonl")),
            format!(
                "{}\n",
                usage_line(&uuid, "s-cap", "2026-10-23T03:00:00Z", index as i64 + 1, 1)
            ),
        )
        .unwrap();
    }

    engine.make_root_scan_due();
    engine.poll_now();
    assert_eq!(
        engine.record_count(),
        ROOT_SCAN_ENQUEUE_CAP as i64,
        "单轮入队必须被上限截断（防风暴）"
    );

    engine.make_root_scan_due();
    engine.poll_now();
    assert_eq!(
        engine.record_count(),
        total as i64,
        "轮转续扫必须让超上限的文件最终全部入账（不得饿死尾部）"
    );
}
