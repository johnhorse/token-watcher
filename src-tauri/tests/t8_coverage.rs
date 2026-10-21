//! T8 覆盖补齐：对照 Test Plan 的缺口
//!
//! - SC3：真实重启（新连接 + 新 engine）后重扫不双计
//! - SC4：删除源 JSONL（直接 scan_dir 与运行期 fs 删除事件）历史仍在
//! - 空态：~/.claude/projects 不存在 / 存在但无 jsonl；空态后首条数据能接上
//! - SC10/D17 第二、三档：单文件不可读跳过计数、根目录不可读横幅
//! - SC6：空闲态无待处理 deadline（无轮询）
//! - 首扫事件序：scanning → done 后紧跟 dashboard-dirty

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::store::{Store, Totals};
use token_watcher_lib::watch::{DataEngine, DataPaths, EventSink, Message};

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
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tw-t8-{}-{nanos}-{n}", std::process::id()));
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

fn paths_for(temp: &Path) -> DataPaths {
    DataPaths::for_test(temp)
}

fn usage_line(
    uuid: &str,
    session: &str,
    timestamp: &str,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
) -> String {
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
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": cache_creation
            }
        }
    })
    .to_string()
}

fn write_records(path: &Path, lines: &[String]) {
    fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
}

fn all_totals(store: &Store) -> Totals {
    store.totals_between(i64::MIN, i64::MAX).unwrap()
}

#[derive(Clone, Default)]
struct MockSink {
    events: Arc<Mutex<Vec<(String, Value)>>>,
    visible: Arc<AtomicBool>,
}

impl MockSink {
    fn new() -> Self {
        let sink = Self::default();
        sink.visible.store(true, Ordering::Relaxed);
        sink
    }

    fn named(&self, name: &str) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(event, _)| event == name)
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    fn events(&self) -> Vec<(String, Value)> {
        self.events.lock().unwrap().clone()
    }
}

impl EventSink for MockSink {
    fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    fn emit_json(&self, event: &str, payload: Value) {
        self.events
            .lock()
            .unwrap()
            .push((event.to_string(), payload));
    }
}

fn pump<S: EventSink>(engine: &mut DataEngine<S>, rx: &Receiver<Message>, duration: Duration) {
    let end = Instant::now() + duration;
    while let Some(remaining) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(Message::Shutdown) => break,
            Ok(msg) => engine.handle(msg),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    engine.flush();
}

/// SC4：删掉源 JSONL（乃至整个源目录）后，历史统计仍在；重扫不重删、不双计。
#[test]
fn sc4_deleted_source_file_keeps_history() {
    let temp = TempDir::new();
    let root = temp.path().join("projects");
    fs::create_dir_all(&root).unwrap();
    let file_a = root.join("session-a.jsonl");
    let file_b = root.join("session-b.jsonl");
    write_records(
        &file_a,
        &[
            usage_line("a-1", "s-a", "2026-10-19T10:00:00Z", 100, 10, 0, 0),
            usage_line("a-2", "s-a", "2026-10-19T10:00:05Z", 200, 20, 0, 0),
        ],
    );
    write_records(
        &file_b,
        &[
            usage_line("b-1", "s-b", "2026-10-19T10:01:00Z", 300, 30, 0, 0),
            usage_line("b-2", "s-b", "2026-10-19T10:01:05Z", 400, 40, 0, 0),
        ],
    );

    let db_path = temp.path().join("db.sqlite3");
    let mut store = Store::open(&db_path).unwrap();
    let first = ingest::scan_dir(&mut store, &root).unwrap();
    assert_eq!(first.files_scanned, 2);
    assert_eq!(first.records, 4);
    let before = all_totals(&store);
    assert_eq!(before.input, 1_000);
    assert_eq!(before.output, 100);
    assert_eq!(store.count_records().unwrap(), 4);

    // 删除其中一个源文件：剩余文件 checkpoint 续读，历史（含已删文件）不变
    fs::remove_file(&file_b).unwrap();
    let rescan = ingest::scan_dir(&mut store, &root).unwrap();
    assert_eq!(rescan.files_scanned, 1);
    assert_eq!(rescan.records, 0, "checkpoint 续读，剩余文件无新行");
    assert_eq!(all_totals(&store), before);
    assert_eq!(store.count_records().unwrap(), 4);

    // 删除整个源目录：按空态容忍，历史仍在
    fs::remove_dir_all(&root).unwrap();
    let empty = ingest::scan_dir(&mut store, &root).unwrap();
    assert_eq!(empty, ingest::ScanStats::default());
    assert_eq!(all_totals(&store), before);
    assert_eq!(store.count_records().unwrap(), 4);

    // 关库重开（模拟重启）后历史仍在
    drop(store);
    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(all_totals(&reopened), before);
    assert_eq!(reopened.count_records().unwrap(), 4);
}

/// SC4 运行期路径：watcher 收到 Remove 事件不得删历史或触发重扫。
#[test]
fn sc4_delete_event_keeps_history_while_watching() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    let file = paths.projects_root.join("session-a.jsonl");
    write_records(
        &file,
        &[
            usage_line("a-1", "s-a", "2026-10-19T10:00:00Z", 100, 10, 0, 0),
            usage_line("a-2", "s-a", "2026-10-19T10:00:05Z", 200, 20, 0, 0),
        ],
    );

    let (tx, rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();

    fs::remove_file(&file).unwrap();
    pump(&mut engine, &rx, Duration::from_secs(2));

    let store = Store::open(&paths.db_path).unwrap();
    assert_eq!(store.count_records().unwrap(), 2, "删除源文件不得删历史");
    let totals = all_totals(&store);
    assert_eq!(totals.input, 300);
    assert_eq!(totals.output, 30);
    assert!(
        sink.named("data-health")
            .iter()
            .all(|h| h["skipped_files"] == 0),
        "删除竞态不应计入 D17 跳过数"
    );
}

/// SC3：真实重启（新连接/新 engine）后重扫，checkpoint 续读、总量不变。
#[test]
fn sc3_restart_rescan_does_not_double_count() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    let file_a = paths.projects_root.join("session-a.jsonl");
    let file_b = paths.projects_root.join("session-b.jsonl");
    write_records(
        &file_a,
        &[
            usage_line("a-1", "s-a", "2026-10-19T10:00:00Z", 100, 10, 0, 0),
            usage_line("a-2", "s-a", "2026-10-19T10:00:05Z", 200, 20, 0, 0),
        ],
    );
    write_records(
        &file_b,
        &[
            usage_line("b-1", "s-b", "2026-10-19T10:01:00Z", 300, 30, 0, 0),
            usage_line("b-2", "s-b", "2026-10-19T10:01:05Z", 400, 40, 0, 0),
        ],
    );

    // 第一次启动：首扫落库
    {
        let (tx, _rx) = mpsc::channel();
        let sink = MockSink::new();
        let mut engine = DataEngine::new(paths.clone(), sink, tx);
        engine.initial_scan();
    }

    let before = {
        let store = Store::open(&paths.db_path).unwrap();
        assert_eq!(store.count_records().unwrap(), 4);
        all_totals(&store)
    };

    // 第二次启动：新 engine 重扫同一目录
    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();
    assert!(
        sink.named("scan-progress")
            .iter()
            .any(|p| p["phase"] == "done"),
        "重启首扫应正常完成"
    );

    // 再直接全量枚举一次：checkpoint 已在，0 条新记录
    let mut store = Store::open(&paths.db_path).unwrap();
    let stats = ingest::scan_dir(&mut store, &paths.projects_root).unwrap();
    assert_eq!(stats.files_scanned, 2);
    assert_eq!(stats.records, 0, "重启重扫不得重复解析已落库行");
    assert_eq!(store.count_records().unwrap(), 4);
    assert_eq!(all_totals(&store), before);

    // checkpoint 已持久化到磁盘（重启续读的依据）
    let key = ingest::checkpoint_key(&file_a);
    assert!(
        store.get_checkpoint(&key).unwrap().is_some(),
        "checkpoint 应跨连接持久化"
    );
}

/// 首扫事件序：scanning(0,0) → 进度中段 → done(total,total) → dashboard-dirty。
#[test]
fn first_scan_progress_then_dirty_sequence() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    for i in 0..2 {
        fs::write(
            paths.projects_root.join(format!("s{i}.jsonl")),
            format!(
                "{}\n",
                usage_line(
                    &format!("u-{i}"),
                    "s",
                    "2026-10-19T10:00:00Z",
                    i as i64 + 1,
                    0,
                    0,
                    0
                )
            ),
        )
        .unwrap();
    }

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths, sink.clone(), tx);
    engine.initial_scan();

    let events = sink.events();
    let progress: Vec<&Value> = events
        .iter()
        .filter(|(name, _)| name == "scan-progress")
        .map(|(_, payload)| payload)
        .collect();
    assert!(!progress.is_empty());
    assert_eq!(progress.first().unwrap()["phase"], "scanning");
    assert_eq!(progress.first().unwrap()["files_total"], 0);
    let last = progress.last().unwrap();
    assert_eq!(last["phase"], "done");
    assert_eq!(last["files_done"], 2);
    assert_eq!(last["files_total"], 2);
    assert_eq!(last["percent"], 100.0);

    let last_progress_idx = events
        .iter()
        .rposition(|(name, _)| name == "scan-progress")
        .unwrap();
    assert!(
        events
            .iter()
            .skip(last_progress_idx + 1)
            .any(|(name, _)| name == "dashboard-dirty"),
        "done 之后应紧发 dashboard-dirty 触发 UI 水合"
    );
}

/// 空态（~/.claude 不存在）：scanning → empty，非错误态，数据库空。
#[test]
fn empty_state_missing_claude_dir() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();

    let phases: Vec<String> = sink
        .named("scan-progress")
        .iter()
        .map(|p| p["phase"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(phases, ["scanning", "empty"]);

    let last = sink.named("scan-progress").pop().unwrap();
    assert_eq!(last["files_total"], 0);
    assert_eq!(last["percent"], 0.0);
    assert_eq!(sink.named("dashboard-dirty").len(), 1);
    assert!(
        sink.named("data-health")
            .iter()
            .all(|h| h["root_unreadable"] == false),
        "根目录不存在是空态，不是不可读错误"
    );
    assert_eq!(
        Store::open(&paths.db_path)
            .unwrap()
            .count_records()
            .unwrap(),
        0
    );
}

/// 空态（projects 存在但无 jsonl）→ 首条数据到达后无缝接上。
#[test]
fn empty_projects_dir_becomes_ready_when_first_file_arrives() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();

    let (tx, rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();

    let phases: Vec<String> = sink
        .named("scan-progress")
        .iter()
        .map(|p| p["phase"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(phases.first().map(String::as_str), Some("scanning"));
    assert_eq!(phases.last().map(String::as_str), Some("empty"));
    assert!(!phases.contains(&"done".to_string()));
    assert_eq!(
        Store::open(&paths.db_path)
            .unwrap()
            .count_records()
            .unwrap(),
        0
    );

    // 空态后第一条数据落盘 → watcher 增量 ingest
    fs::write(
        paths.projects_root.join("session.jsonl"),
        format!(
            "{}\n",
            usage_line("late-1", "s-late", "2026-10-19T10:00:00Z", 42, 7, 0, 0)
        ),
    )
    .unwrap();
    pump(&mut engine, &rx, Duration::from_secs(3));

    let store = Store::open(&paths.db_path).unwrap();
    assert_eq!(store.count_records().unwrap(), 1);
    let totals = all_totals(&store);
    assert_eq!(totals.input, 42);
    assert_eq!(totals.output, 7);
    assert!(
        sink.named("dashboard-dirty").len() >= 2,
        "空态首扫一次 + 数据到达一次"
    );
}

/// SC10/D17 第二档：单文件不可读 → 跳过计数 + 不中断整轮扫描。
#[cfg(unix)]
#[test]
fn sc10_unreadable_file_is_skipped_and_reported() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    fs::write(
        paths.projects_root.join("good.jsonl"),
        format!(
            "{}\n",
            usage_line("good-1", "s", "2026-10-19T10:00:00Z", 10, 1, 0, 0)
        ),
    )
    .unwrap();
    let bad = paths.projects_root.join("bad.jsonl");
    fs::write(
        &bad,
        format!(
            "{}\n",
            usage_line("bad-1", "s", "2026-10-19T10:00:01Z", 999, 999, 0, 0)
        ),
    )
    .unwrap();
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).unwrap();

    // root 用户会绕过权限位：此时本测试退化为跳过
    if fs::read(&bad).is_ok() {
        let _ = fs::set_permissions(&bad, fs::Permissions::from_mode(0o600));
        eprintln!("skip sc10_unreadable_file_is_skipped_and_reported: 进程有 root 级读权限");
        return;
    }

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();

    let health = sink.named("data-health");
    assert!(
        health.iter().any(|h| h["skipped_files"] == 1),
        "health: {health:?}"
    );

    let store = Store::open(&paths.db_path).unwrap();
    assert_eq!(
        store.count_records().unwrap(),
        1,
        "坏文件跳过，好文件照常入库"
    );
    assert_eq!(all_totals(&store).input, 10);

    let _ = fs::set_permissions(&bad, fs::Permissions::from_mode(0o600));
}

/// SC10/D17 第一档：根目录存在但不可读 → root_unreadable 横幅 + 空态收尾。
#[cfg(unix)]
#[test]
fn sc10_unreadable_root_reports_banner() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    fs::set_permissions(&paths.projects_root, fs::Permissions::from_mode(0o000)).unwrap();

    if fs::read_dir(&paths.projects_root).is_ok() {
        let _ = fs::set_permissions(&paths.projects_root, fs::Permissions::from_mode(0o700));
        eprintln!("skip sc10_unreadable_root_reports_banner: 进程有 root 级读权限");
        return;
    }

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();

    // 先恢复权限，保证 TempDir 能清理
    fs::set_permissions(&paths.projects_root, fs::Permissions::from_mode(0o700)).unwrap();

    let health = sink.named("data-health");
    assert!(
        health.iter().any(|h| h["root_unreadable"] == true),
        "health: {health:?}"
    );
    assert!(
        sink.named("scan-progress")
            .iter()
            .any(|p| p["phase"] == "empty"),
        "不可读也要结束骨架态"
    );
    assert_eq!(
        Store::open(&paths.db_path)
            .unwrap()
            .count_records()
            .unwrap(),
        0
    );
}

/// SC6：空闲（无 fs 事件/去抖批次）时没有待处理 deadline —— 不轮询。
#[test]
fn sc6_idle_engine_has_no_pending_deadline() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    fs::write(
        paths.projects_root.join("s.jsonl"),
        format!(
            "{}\n",
            usage_line("u-1", "s", "2026-10-19T10:00:00Z", 5, 1, 0, 0)
        ),
    )
    .unwrap();

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths, sink.clone(), tx);
    engine.initial_scan();
    engine.flush();

    assert!(
        engine.next_deadline().is_none(),
        "首扫完成且无待处理批次时应可无限阻塞等消息"
    );
    let dirty_before = sink.named("dashboard-dirty").len();
    engine.on_tick();
    assert!(
        engine.next_deadline().is_none(),
        "空闲 tick 不得制造新的 deadline"
    );
    assert_eq!(
        sink.named("dashboard-dirty").len(),
        dirty_before,
        "空闲 tick 不得产生额外刷新"
    );
}
