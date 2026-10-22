//! t19_engine_slicing: 多引擎聚合切片与可用引擎统计测试

use std::fs;
use std::path::PathBuf;
use token_watcher_lib::core::agg::{dashboard_at_with_prices_and_engine, Range};
use token_watcher_lib::core::parse::UsageRecord;
use token_watcher_lib::core::store::Store;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "tw-test-engine-slicing-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn rec(
    id: &str,
    session: &str,
    ts_ms: i64,
    model: &str,
    entrypoint: &str,
    input: i64,
    output: i64,
) -> UsageRecord {
    UsageRecord {
        record_id: id.to_string(),
        session_id: session.to_string(),
        timestamp_ms: ts_ms,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        model: model.to_string(),
        entrypoint: entrypoint.to_string(),
        is_sidechain: false,
        cwd: Some("/Volumes/Data/projects/token-watcher".to_string()),
        cc_version: None,
        duration_ms: None,
        thoughts_tokens: 0,
    }
}

#[test]
fn test_engine_slicing_and_available_engines() {
    let temp = TempDir::new("slicing");
    let mut store = Store::open(&temp.path.join("db.sqlite3")).unwrap();
    let now = 1_000_000_000i64;

    // 录入多引擎数据：
    // 1. Claude: 1000 input, 200 output = 1200 tokens
    // 2. Cline: 3000 input, 500 output = 3500 tokens
    // 3. omp: 5000 input, 1000 output = 6000 tokens
    let records = vec![
        rec(
            "claude-uuid-1",
            "sess-claude",
            now - 1000,
            "claude-3-7-sonnet",
            "claude",
            1000,
            200,
        ),
        rec(
            "cline:task-1:123:abc",
            "task-1",
            now - 2000,
            "deepseek-chat",
            "cline",
            3000,
            500,
        ),
        rec(
            "omp:sess-omp-1:msg1",
            "sess-omp-1",
            now - 3000,
            "gemini-3.8-flash",
            "omp",
            5000,
            1000,
        ),
    ];
    store.upsert_records(&records).unwrap();

    // 1. 全量全局聚合（无 engine 过滤）
    let dash_all =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, None).unwrap();
    assert_eq!(dash_all.totals.total, 1200 + 3500 + 6000);
    assert_eq!(dash_all.sessions.len(), 3);
    assert_eq!(dash_all.models.len(), 3);
    assert_eq!(dash_all.engine, None);

    // 验证 available_engines
    assert_eq!(dash_all.available_engines.len(), 3);
    let omp_slice = dash_all
        .available_engines
        .iter()
        .find(|e| e.engine == "omp")
        .unwrap();
    assert_eq!(omp_slice.tokens, 6000);
    let cline_slice = dash_all
        .available_engines
        .iter()
        .find(|e| e.engine == "cline")
        .unwrap();
    assert_eq!(cline_slice.tokens, 3500);
    let claude_slice = dash_all
        .available_engines
        .iter()
        .find(|e| e.engine == "claude")
        .unwrap();
    assert_eq!(claude_slice.tokens, 1200);

    // 2. 切片为 Cline
    let dash_cline =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("cline")).unwrap();
    assert_eq!(dash_cline.engine.as_deref(), Some("cline"));
    assert_eq!(dash_cline.totals.total, 3500);
    assert_eq!(dash_cline.totals.input, 3000);
    assert_eq!(dash_cline.totals.output, 500);
    assert_eq!(dash_cline.sessions.len(), 1);
    assert_eq!(dash_cline.sessions[0].session_id, "task-1");
    assert_eq!(dash_cline.sessions[0].engine, "cline");
    assert_eq!(dash_cline.models.len(), 1);
    assert_eq!(dash_cline.models[0].model, "deepseek-chat");
    // available_engines 保持全局统计，便于前端展示导航徽章
    assert_eq!(dash_cline.available_engines.len(), 3);

    // 3. 切片为 omp
    let dash_omp =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("omp")).unwrap();
    assert_eq!(dash_omp.engine.as_deref(), Some("omp"));
    assert_eq!(dash_omp.totals.total, 6000);
    assert_eq!(dash_omp.sessions.len(), 1);
    assert_eq!(dash_omp.sessions[0].session_id, "sess-omp-1");
    assert_eq!(dash_omp.sessions[0].engine, "omp");
    assert_eq!(dash_omp.sessions[0].title, "token-watcher");
    assert_eq!(dash_omp.models.len(), 1);
    assert_eq!(dash_omp.models[0].model, "gemini-3.8-flash");

    // 4. 切片为 Claude
    let dash_claude =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("claude")).unwrap();
    assert_eq!(dash_claude.engine.as_deref(), Some("claude"));
    assert_eq!(dash_claude.totals.total, 1200);
    assert_eq!(dash_claude.sessions.len(), 1);
    assert_eq!(dash_claude.sessions[0].engine, "claude");

    // 5. 传入 "all" 语义与 None 等价
    let dash_all_str =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("all")).unwrap();
    assert_eq!(dash_all_str.totals.total, dash_all.totals.total);
    assert_eq!(dash_all_str.engine, None);
}
