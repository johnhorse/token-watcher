//! T22: v0.7.2 P1 滚动日志器行为测试
//!
//! - 滚动：超单文件上限 → current 轮转为 .1，保留份数受 keep 约束；
//! - IO 失败：目录不可创建/不可写时静默降级 no-op，绝不 panic；
//! - 数据引擎埋点：首扫 / 轮询 / flush 行真实落盘；stat ENOENT 只记一次
//!   （内存 skip-set，避免每轮 poll 重复报错）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde_json::json;
use token_watcher_lib::log::{self, RollingLog, DEFAULT_KEEP, DEFAULT_MAX_BYTES};
use token_watcher_lib::watch::{DataEngine, DataPaths, EventSink};

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

struct MockSink;

impl EventSink for MockSink {
    fn is_visible(&self) -> bool {
        true
    }
    fn emit_json(&self, _event: &str, _payload: serde_json::Value) {}
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

#[test]
fn test_rolling_log_rotation_and_keep() {
    let temp = TempDir::new("log-rotate");
    let dir = temp.path().join("log");
    let mut rolling = RollingLog::new(dir.clone(), 512, 2);
    for index in 0..40 {
        rolling.info(&format!("line-{index:02}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
    }

    let current = dir.join(log::FILE_NAME);
    let first = dir.join(format!("{}.1", log::FILE_NAME));
    assert!(current.is_file(), "当前日志文件必须存在");
    assert!(first.is_file(), "超过单文件上限必须滚动出 .1");
    assert!(
        !dir.join(format!("{}.2", log::FILE_NAME)).exists(),
        "keep=2 时不得保留第三份"
    );
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        2,
        "文件数不得超过保留份数"
    );
    assert!(
        fs::metadata(&current).unwrap().len() <= 512 + 128,
        "单文件必须受上限约束（允许一条溢出）"
    );

    let text = fs::read_to_string(&current).unwrap();
    assert!(!text.is_empty());
    assert!(
        text.lines().all(|line| line.starts_with('[')),
        "每行必须带 ISO8601 时间戳前缀"
    );
}

#[test]
fn test_rolling_log_io_failure_is_noop() {
    let temp = TempDir::new("log-noop");
    // 目录位置被普通文件占用 → create_dir_all / open 全部失败
    let blocked = temp.path().join("blocked");
    fs::write(&blocked, "not a dir").unwrap();

    let mut rolling = RollingLog::new(blocked.clone(), 1024, 2);
    for index in 0..50 {
        // 关键断言：任何 IO 失败都不得 panic / 阻塞
        rolling.info(&format!("line-{index}"));
    }
    assert!(blocked.is_file(), "日志器不得破坏占用路径的既有文件");
}

#[test]
fn test_engine_poll_logging_and_missing_skip_set() {
    let temp = TempDir::new("log-engine");
    let log_dir = temp.path().join("logs");
    log::init(log_dir.clone(), DEFAULT_MAX_BYTES, DEFAULT_KEEP);
    assert!(log::enabled(), "进程级日志器必须可用");

    let paths = DataPaths::for_test(temp.path());
    let proj_dir = paths.projects_root.join("proj-a");
    fs::create_dir_all(&proj_dir).unwrap();
    let file = proj_dir.join("s.jsonl");
    fs::write(
        &file,
        format!(
            "{}\n",
            usage_line("u-1", "s-1", "2026-10-23T01:00:00Z", 100, 10)
        ),
    )
    .unwrap();

    let (tx, _rx) = mpsc::channel();
    let mut engine = DataEngine::new(paths, MockSink, tx);
    engine.initial_scan();

    // 无事件追加字节 → 下一轮①检出并 flush 落库
    let mut content = fs::read_to_string(&file).unwrap();
    content.push_str(&usage_line("u-2", "s-1", "2026-10-23T01:02:00Z", 200, 20));
    content.push('\n');
    fs::write(&file, content).unwrap();
    engine.poll_now();
    assert_eq!(engine.record_count(), 2);

    // 源文件被删除：第一次 poll 记一条 missing，之后走 skip-set 不再重复记
    fs::remove_file(&file).unwrap();
    engine.poll_now();
    engine.poll_now();
    drop(engine);

    let text = fs::read_to_string(log_dir.join(log::FILE_NAME)).unwrap();
    assert!(text.contains("scan: done"), "首扫摘要必须落盘:\n{text}");
    assert!(text.contains("poll active="), "轮询动作必须落盘");
    assert!(text.contains("flush files="), "flush 批次必须落盘");
    assert!(
        text.contains("ingest path="),
        "ingest / checkpoint 推进必须落盘"
    );
    let missing = text
        .lines()
        .filter(|line| line.contains("source missing") && line.contains("s.jsonl"))
        .count();
    assert_eq!(missing, 1, "ENOENT 只记一次（内存 skip-set）:\n{text}");
}
