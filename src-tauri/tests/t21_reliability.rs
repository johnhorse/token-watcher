//! T21: v0.7.2 可靠性收尾测试（T1：根目录事件 → 单根补扫）

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde_json::json;
use token_watcher_lib::watch::{DataEngine, DataPaths, EventSink, Message};

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

fn dir_event(path: &Path) -> notify::Event {
    notify::Event {
        kind: notify::EventKind::Create(notify::event::CreateKind::Any),
        paths: vec![path.to_path_buf()],
        attrs: Default::default(),
    }
}

#[test]
fn test_root_dir_event_triggers_single_root_catchup() {
    let temp = TempDir::new("root-catchup");
    let paths = DataPaths::for_test(temp.path());

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

    let (tx, _rx) = mpsc::channel();
    let mut engine = DataEngine::new(paths.clone(), MockSink, tx);
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
