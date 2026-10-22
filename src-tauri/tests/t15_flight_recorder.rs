//! t15_flight_recorder：会话飞行记录仪（Flight Recorder）深度透视与聚合测试
//!
//! 1. 不存在会话安全返回 None
//! 2. 单请求会话无除零崩溃且耗时为 0
//! 3. 多轮交互会话全量准确聚合（Totals、TokenCost、模型占比、缓存命中率）
//! 4. 主链与子链（sidechain）隔离：子链交错不触发虚假 model_switched
//! 5. 突刺（Spike）科学双门限判定与客观分类标签
//! 6. 有界事件流截断：pulse_strip 保持全量，events 按 limit 截断最新
//! 7. 未定价模型安全降级与未定价 Token 统计

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use token_watcher_lib::core::agg;
use token_watcher_lib::core::parse::UsageRecord;
use token_watcher_lib::core::pricing::PriceTable;
use token_watcher_lib::core::store::Store;

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
        let path = std::env::temp_dir().join(format!("tw-t15-{}-{nanos}-{n}", std::process::id()));
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

#[allow(clippy::too_many_arguments)]
fn make_record(
    session_id: &str,
    record_id: &str,
    model: &str,
    t_ms: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
    is_sidechain: bool,
) -> UsageRecord {
    UsageRecord {
        record_id: record_id.to_string(),
        session_id: session_id.to_string(),
        timestamp_ms: t_ms,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_creation,
        model: model.to_string(),
        entrypoint: "cli".to_string(),
        is_sidechain,
        cwd: Some("/Volumes/Data/projects/token-watcher".to_string()),
        cc_version: Some("1.0.0".to_string()),
        duration_ms: None,
        thoughts_tokens: 0,
    }
}

#[test]
fn test_nonexistent_session() {
    let dir = TempDir::new();
    let db_path = dir.path().join("test.db");
    let store = Store::open(&db_path).unwrap();
    let prices = PriceTable::bundled();

    let res =
        agg::session_flight_recorder(&store, Some(&prices), "non-existent-sess", None, 100_000)
            .unwrap();
    assert!(res.is_none());
}

#[test]
fn test_single_record_session() {
    let dir = TempDir::new();
    let db_path = dir.path().join("test.db");
    let mut store = Store::open(&db_path).unwrap();
    let prices = PriceTable::bundled();

    let rec = make_record(
        "sess-single",
        "msg-1",
        "claude-3-5-haiku-20241022",
        10_000,
        100,
        200,
        500,
        0,
        false,
    );
    store.upsert_records(&[rec]).unwrap();

    let fr = agg::session_flight_recorder(&store, Some(&prices), "sess-single", None, 10_000)
        .unwrap()
        .expect("should find session");

    assert_eq!(fr.session_id, "sess-single");
    assert_eq!(fr.title, "token-watcher");
    assert_eq!(fr.start_time_ms, 10_000);
    assert_eq!(fr.end_time_ms, 10_000);
    assert_eq!(fr.duration_seconds, 0);
    assert!(fr.is_active);
    assert_eq!(fr.total_requests, 1);
    assert_eq!(fr.totals.input, 100);
    assert_eq!(fr.totals.output, 200);
    assert_eq!(fr.totals.cache_read, 500);
    assert_eq!(fr.totals.total, 800);
    // 缓存命中率: 500 / (100 + 500) = 500 / 600 ≈ 0.8333
    assert!((fr.cache_hit_ratio - (500.0 / 600.0)).abs() < 1e-4);
    assert_eq!(fr.pulse_strip.len(), 1);
    assert_eq!(fr.events.len(), 1);
    assert!(!fr.events_truncated);
    assert!(!fr.events[0].is_spike);
    assert!(!fr.events[0].model_switched);
    assert!(fr.cost.is_complete);
    assert!(fr.cost.total > 0.0);
}

#[test]
fn test_multi_turn_aggregation_and_models() {
    let dir = TempDir::new();
    let db_path = dir.path().join("test.db");
    let mut store = Store::open(&db_path).unwrap();
    let prices = PriceTable::bundled();

    let recs = vec![
        make_record(
            "sess-multi",
            "msg-1",
            "claude-3-5-haiku-20241022",
            10_000,
            1000,
            200,
            0,
            0,
            false,
        ),
        make_record(
            "sess-multi",
            "msg-2",
            "claude-3-5-haiku-20241022",
            20_000,
            500,
            100,
            1500,
            0,
            false,
        ),
        make_record(
            "sess-multi",
            "msg-3",
            "claude-3-7-sonnet-20250219",
            40_000,
            2000,
            800,
            4000,
            1000,
            false,
        ),
    ];
    store.upsert_records(&recs).unwrap();

    let fr = agg::session_flight_recorder(&store, Some(&prices), "sess-multi", None, 50_000)
        .unwrap()
        .expect("should find session");

    assert_eq!(fr.total_requests, 3);
    assert_eq!(fr.start_time_ms, 10_000);
    assert_eq!(fr.end_time_ms, 40_000);
    assert_eq!(fr.duration_seconds, 30);
    assert_eq!(fr.models_used.len(), 2);
    // Sonnet 总 token = 7800, Haiku = 1200 + 2100 = 3300
    assert_eq!(fr.models_used[0].model, "claude-3-7-sonnet-20250219");
    assert_eq!(fr.models_used[1].model, "claude-3-5-haiku-20241022");
    assert!(fr.models_used[0].share > fr.models_used[1].share);

    // 检查第 3 次调用的 model_switched 标记
    assert!(!fr.events[0].model_switched);
    assert!(!fr.events[1].model_switched);
    assert!(fr.events[2].model_switched);
}

#[test]
fn test_sidechain_isolation_and_model_switch() {
    let dir = TempDir::new();
    let db_path = dir.path().join("test.db");
    let mut store = Store::open(&db_path).unwrap();
    let prices = PriceTable::bundled();

    // 场景：主链用 Sonnet，中间插了一条子链用 Haiku 执行小任务，随后主链继续用 Sonnet
    let recs = vec![
        make_record(
            "sess-chain",
            "msg-1",
            "claude-3-7-sonnet-20250219",
            10_000,
            1000,
            100,
            0,
            0,
            false,
        ),
        make_record(
            "sess-chain",
            "msg-2",
            "claude-3-5-haiku-20241022",
            15_000,
            200,
            50,
            0,
            0,
            true,
        ), // sidechain!
        make_record(
            "sess-chain",
            "msg-3",
            "claude-3-7-sonnet-20250219",
            20_000,
            1000,
            100,
            0,
            0,
            false,
        ),
        make_record(
            "sess-chain",
            "msg-4",
            "claude-3-opus-20240229",
            30_000,
            1000,
            100,
            0,
            0,
            false,
        ),
    ];
    store.upsert_records(&recs).unwrap();

    let fr = agg::session_flight_recorder(&store, Some(&prices), "sess-chain", None, 35_000)
        .unwrap()
        .expect("should find session");

    assert_eq!(fr.total_requests, 4);

    // msg-1: 主链起始
    assert!(!fr.events[0].is_sidechain);
    assert!(!fr.events[0].model_switched);

    // msg-2: 子链，is_sidechain 为 true，不计入主链切换
    assert!(fr.events[1].is_sidechain);
    assert!(!fr.events[1].model_switched);

    // msg-3: 主链继续用 Sonnet，与前一条主链 msg-1 相同，严禁因夹杂的子链而误报切换！
    assert!(!fr.events[2].is_sidechain);
    assert!(
        !fr.events[2].model_switched,
        "主链前后均为 Sonnet，中间子链不应触发误报切换"
    );

    // msg-4: 主链切换为 Opus，必须正确识别为切换
    assert!(!fr.events[3].is_sidechain);
    assert!(
        fr.events[3].model_switched,
        "主链由 Sonnet 切换至 Opus，必须正确标记"
    );
}

#[test]
fn test_spike_detection_and_reasons() {
    let dir = TempDir::new();
    let db_path = dir.path().join("test.db");
    let mut store = Store::open(&db_path).unwrap();
    let prices = PriceTable::bundled();

    // 10 条常规小请求（均值约 500 token）
    let mut recs = Vec::new();
    for i in 1..=10 {
        recs.push(make_record(
            "sess-spike",
            &format!("msg-{i}"),
            "claude-3-5-haiku-20241022",
            i * 10_000,
            300,
            100,
            100,
            0,
            false,
        ));
    }
    // 突刺 1：输入主导大文件读入 (45k in / 1k out / 5k cache)
    recs.push(make_record(
        "sess-spike",
        "msg-spike-1",
        "claude-3-7-sonnet-20250219",
        110_000,
        45_000,
        1_000,
        5_000,
        0,
        false,
    ));
    // 突刺 2：缓存写入填充 (2k in / 500 out / 0 read / 30k creation)
    recs.push(make_record(
        "sess-spike",
        "msg-spike-2",
        "claude-3-7-sonnet-20250219",
        120_000,
        2_000,
        500,
        0,
        30_000,
        false,
    ));

    store.upsert_records(&recs).unwrap();

    let fr = agg::session_flight_recorder(&store, Some(&prices), "sess-spike", None, 130_000)
        .unwrap()
        .expect("should find session");

    // 前 10 条均非突刺
    for i in 0..10 {
        assert!(!fr.events[i].is_spike);
        assert!(fr.events[i].spike_reason.is_none());
        assert!(!fr.pulse_strip[i].is_spike);
    }

    // 突刺 1 (index 10)
    assert!(fr.events[10].is_spike);
    assert_eq!(
        fr.events[10].spike_reason.as_deref(),
        Some("未缓存输入主导")
    );
    assert!(fr.pulse_strip[10].is_spike);

    // 突刺 2 (index 11)
    assert!(fr.events[11].is_spike);
    assert_eq!(
        fr.events[11].spike_reason.as_deref(),
        Some("缓存写入/上下文填充")
    );
    assert!(fr.pulse_strip[11].is_spike);
}

#[test]
fn test_truncation_and_paging_limit() {
    let dir = TempDir::new();
    let db_path = dir.path().join("test.db");
    let mut store = Store::open(&db_path).unwrap();
    let prices = PriceTable::bundled();

    let mut recs = Vec::new();
    for i in 1..=10 {
        recs.push(make_record(
            "sess-trunc",
            &format!("msg-{i}"),
            "claude-3-5-haiku-20241022",
            i * 1000,
            100,
            50,
            0,
            0,
            false,
        ));
    }
    store.upsert_records(&recs).unwrap();

    // 限制仅取最新 4 条
    let fr = agg::session_flight_recorder(&store, Some(&prices), "sess-trunc", Some(4), 20_000)
        .unwrap()
        .expect("should find session");

    assert_eq!(fr.total_requests, 10);
    // pulse_strip 必须保留全景 10 根柱子
    assert_eq!(fr.pulse_strip.len(), 10);
    // events 截断为最新 4 条
    assert_eq!(fr.events.len(), 4);
    assert!(fr.events_truncated);
    // 最新 4 条的 request_index 仍保持原序号 7, 8, 9, 10
    assert_eq!(fr.events[0].request_index, 7);
    assert_eq!(fr.events[3].request_index, 10);
}

#[test]
fn test_unpriced_model_handling() {
    let dir = TempDir::new();
    let db_path = dir.path().join("test.db");
    let mut store = Store::open(&db_path).unwrap();
    let prices = PriceTable::bundled();

    let rec = make_record(
        "sess-unpriced",
        "msg-1",
        "custom-private-model",
        10_000,
        1000,
        500,
        0,
        0,
        false,
    );
    store.upsert_records(&[rec]).unwrap();

    let fr = agg::session_flight_recorder(&store, Some(&prices), "sess-unpriced", None, 15_000)
        .unwrap()
        .expect("should find session");

    assert!(!fr.cost.is_complete);
    assert_eq!(fr.cost.unpriced_tokens, 1500);
    assert_eq!(fr.cost.total, 0.0);
    assert_eq!(fr.events[0].cost.total, 0.0);
    assert!(!fr.events[0].cost.is_complete);
}
