//! T3+T4 集成测试：app 数据编排（watcher / 去抖增量 / settings / 写者锁 / 首扫进度）
//!
//! 全部使用真实临时目录 + notify（D18：仅 cargo test）。fs 事件异步到达，
//! 断言用「有限时长的 pump + 二次 flush」轮询，超时即失败，不依赖固定 sleep。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use token_watcher_lib::app_lock::{self, AppLock, LockError};
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::store::Store;
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
        let path = std::env::temp_dir().join(format!("tw-t34-{}-{nanos}-{n}", std::process::id()));
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

/// 收集 fs 事件并交给 engine，最多等待 `duration`；结束时强制处理一批。
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

/// D21：watcher 已注册、枚举尚未开始的窗口内写入的文件不能丢。
/// 枚举走 checkpoint 续读，与事件路径重叠也幂等。
#[test]
fn d21_file_written_between_watcher_register_and_scan_is_ingested() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    // DataEngine::new 内部先注册 watcher
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);

    // 注册之后、枚举之前写入
    let file = paths.projects_root.join("handoff.jsonl");
    fs::write(
        &file,
        format!(
            "{}\n",
            usage_line("u-handoff", "s1", "2026-10-19T10:00:00Z", 10, 2, 3, 4)
        ),
    )
    .unwrap();

    engine.initial_scan();

    let store = Store::open(&paths.db_path).unwrap();
    assert_eq!(store.count_records().unwrap(), 1);
    let progress = sink.named("scan-progress");
    assert!(progress.iter().any(|p| p["phase"] == "done"));
}

/// 追加行 → fs 事件 → 增量 ingest；重复 flush 不双计；新记录发 usage-pulse。
#[test]
fn append_triggers_incremental_ingest_without_double_counting() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    let file = paths.projects_root.join("session-a.jsonl");
    fs::write(
        &file,
        format!(
            "{}\n",
            usage_line("u-1", "s1", "2026-10-19T10:00:00Z", 10, 1, 0, 0)
        ),
    )
    .unwrap();

    let (tx, rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();
    assert_eq!(
        Store::open(&paths.db_path)
            .unwrap()
            .count_records()
            .unwrap(),
        1
    );
    // 首扫不应产生 usage-pulse（历史记录不是「到达」）
    assert!(sink.named("usage-pulse").is_empty());

    // 追加一条新记录
    fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(
            format!(
                "{}\n",
                usage_line("u-2", "s1", "2026-10-19T10:00:05Z", 20, 5, 7, 0)
            )
            .as_bytes(),
        )
        .unwrap();

    pump(&mut engine, &rx, Duration::from_secs(3));

    let store = Store::open(&paths.db_path).unwrap();
    assert_eq!(store.count_records().unwrap(), 2);

    let pulses = sink.named("usage-pulse");
    assert_eq!(pulses.len(), 1, "pulses: {pulses:?}");
    assert_eq!(pulses[0]["input"], 20);
    assert_eq!(pulses[0]["output"], 5);
    assert_eq!(pulses[0]["cache_read"], 7);
    assert_eq!(pulses[0]["cache_creation"], 0);

    // 再等一轮：迟到的重复事件只能读到 0 条新行，不双计、不重发 pulse
    pump(&mut engine, &rx, Duration::from_millis(1200));
    assert_eq!(
        Store::open(&paths.db_path)
            .unwrap()
            .count_records()
            .unwrap(),
        2
    );
    assert_eq!(sink.named("usage-pulse").len(), 1);
}

/// settings.json 原子写（tmp + rename）被捕获；坏 JSON 保持上一态。
#[test]
fn settings_atomic_replace_is_applied_and_bad_json_is_ignored() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    fs::create_dir_all(&paths.claude_dir).unwrap();

    let (tx, rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths.clone(), sink.clone(), tx);
    engine.initial_scan();

    let write_atomic = |body: &str| {
        let tmp = paths.claude_dir.join("settings.json.tmp");
        fs::write(&tmp, body).unwrap();
        fs::rename(&tmp, &paths.settings_path).unwrap();
    };

    write_atomic(
        &json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://api.deepseek.com",
                "ANTHROPIC_AUTH_TOKEN": "sk-test"
            }
        })
        .to_string(),
    );
    pump(&mut engine, &rx, Duration::from_secs(3));

    let store = Store::open(&paths.db_path).unwrap();
    let profiles = store.profiles().unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].endpoint, "https://api.deepseek.com");
    let intervals = store.timeline_intervals().unwrap();
    assert_eq!(intervals.len(), 1);
    assert_eq!(intervals[0].end_ms, None);

    // 瞬时坏 JSON：不得产生时间线变化
    write_atomic("{ this is not json");
    pump(&mut engine, &rx, Duration::from_secs(3));

    let store = Store::open(&paths.db_path).unwrap();
    assert_eq!(store.profiles().unwrap().len(), 1);
    let intervals = store.timeline_intervals().unwrap();
    assert_eq!(intervals.len(), 1);
    assert_eq!(intervals[0].end_ms, None);
}

/// 写者互斥：同一进程两个句柄也不能同时持有；释放后可以再拿。
#[test]
fn writer_lock_excludes_second_handle() {
    let temp = TempDir::new();
    let lock_path = temp.path().join("owner.lock");

    let first = AppLock::acquire(&lock_path).unwrap();
    assert_eq!(
        app_lock::read_owner_pid(&lock_path),
        Some(std::process::id())
    );

    let second = AppLock::acquire(&lock_path);
    assert!(matches!(second, Err(LockError::Busy)));

    drop(first);
    assert!(AppLock::acquire(&lock_path).is_ok());
}

/// scan_dir_with_progress：枚举后回调 (0,total)，每文件递增，收尾 (total,total)。
#[test]
fn scan_dir_with_progress_reports_monotonic_counts() {
    let temp = TempDir::new();
    let root = temp.path().join("projects");
    fs::create_dir_all(&root).unwrap();
    for i in 0..3 {
        fs::write(
            root.join(format!("s{i}.jsonl")),
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

    let mut store = Store::open(&temp.path().join("db.sqlite3")).unwrap();
    let mut calls: Vec<(u64, u64)> = Vec::new();
    let stats =
        ingest::scan_dir_with_progress(&mut store, &root, |done, total| calls.push((done, total)))
            .unwrap();

    assert_eq!(stats.files_scanned, 3);
    assert_eq!(calls.first(), Some(&(0, 3)));
    assert_eq!(calls.last(), Some(&(3, 3)));
    assert!(calls.iter().all(|(_, total)| *total == 3));
    assert!(calls.windows(2).all(|w| w[0].0 <= w[1].0));
}

/// D17 第三档：数据库打不开 → data-health 报 db_readonly，且骨架态结束
#[test]
fn db_open_failure_reports_readonly_health() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();
    // 让 db 路径是个目录，SQLite 必然打不开
    fs::create_dir_all(&paths.db_path).unwrap();

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    let mut engine = DataEngine::new(paths, sink.clone(), tx);
    engine.initial_scan();

    let health = sink.named("data-health");
    assert!(
        health.iter().any(|h| h["db_readonly"] == true),
        "health: {health:?}"
    );
    assert!(sink
        .named("scan-progress")
        .iter()
        .any(|p| p["phase"] == "done"));
}

/// 窗口隐藏时挂起 emit；重新显示发一次 dashboard-dirty（契约水合语义）
#[test]
fn hidden_window_suspends_events_and_show_sends_dirty() {
    let temp = TempDir::new();
    let paths = paths_for(temp.path());
    fs::create_dir_all(&paths.projects_root).unwrap();

    let (tx, _rx) = mpsc::channel();
    let sink = MockSink::new();
    sink.visible.store(false, Ordering::Relaxed);
    let mut engine = DataEngine::new(paths, sink.clone(), tx);
    engine.initial_scan();

    assert!(sink.named("scan-progress").is_empty());
    assert!(sink.named("dashboard-dirty").is_empty());

    sink.visible.store(true, Ordering::Relaxed);
    engine.handle(Message::WindowShown);
    assert_eq!(sink.named("dashboard-dirty").len(), 1);
}
