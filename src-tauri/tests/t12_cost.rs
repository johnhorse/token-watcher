//! t12_cost：成本估算引擎与聚合测试
//!
//! 1. 已知模型（Claude、DeepSeek、GLM）四项 Token 单价精准累乘
//! 2. 提供商前缀剥离与动态最新版本别名匹配
//! 3. 未知私有模型未定价降级（cost: None, unpriced_tokens 统计, is_complete=false）
//! 4. 空范围防除以零（total_tokens == 0 -> coverage=1.0, is_complete=true, cost=0.0）
//! 5. 免费模型（0元）正常计费且计为已定价
//! 6. 多档案多模型分摊与 Profile 级 cost 聚合
//! 7. 磁盘缓存读取与有效条目阈值容错

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use token_watcher_lib::core::agg::{self, Range};
use token_watcher_lib::core::parse::UsageRecord;
use token_watcher_lib::core::pricing::{PriceManager, PriceTable};
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
        let path = std::env::temp_dir().join(format!("tw-t12-{}-{nanos}-{n}", std::process::id()));
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

fn make_record(
    id: &str,
    model: &str,
    t_ms: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
) -> UsageRecord {
    UsageRecord {
        record_id: id.to_string(),
        session_id: "sess-1".to_string(),
        timestamp_ms: t_ms,
        model: model.to_string(),
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_creation,
        entrypoint: "cli".to_string(),
        is_sidechain: false,
        cwd: Some("/test".to_string()),
        cc_version: None,
    }
}

// 1. 已知核心模型四项 Token 单价计算精度
#[test]
fn test_bundled_claude_sonnet_pricing() {
    let prices = PriceTable::bundled();
    let unit = prices.get_unit_price("claude-3-5-sonnet-20241022").unwrap();

    // 1M input ($3) + 1M output ($15) + 1M cache_read ($0.30) + 1M cache_creation ($3.75) = $22.05
    let cost = unit.calculate(1_000_000, 1_000_000, 1_000_000, 1_000_000);
    assert!((cost - 22.05).abs() < 1e-6, "Sonnet 4 项各 1M 花费应为 $22.05，实测 {cost}");
}

// 2. 前缀剥离与动态别名匹配
#[test]
fn test_prefix_stripping_and_alias_matching() {
    let prices = PriceTable::bundled();

    // 剥离 provider 前缀
    let p1 = prices.get_unit_price("anthropic/claude-3-5-sonnet-20241022");
    assert!(p1.is_some(), "anthropic/ 前缀应成功剥离");

    let p2 = prices.get_unit_price("bedrock/claude-3-5-sonnet-20241022");
    assert!(p2.is_some(), "bedrock/ 前缀应成功剥离");

    // 动态别名匹配
    let alias_sonnet = prices.get_unit_price("claude-sonnet");
    assert!(alias_sonnet.is_some(), "claude-sonnet 应匹配到最新 sonnet");

    let alias_opus = prices.get_unit_price("claude-opus");
    assert!(alias_opus.is_some(), "claude-opus 应匹配到最新 opus");

    let alias_haiku = prices.get_unit_price("claude-haiku");
    assert!(alias_haiku.is_some(), "claude-haiku 应匹配到最新 haiku");
}

// 3. 空范围防御（除以零安全）
#[test]
fn test_empty_range_coverage_and_cost() {
    let store = Store::open_in_memory().unwrap();
    let dashboard = agg::dashboard_at(&store, Range::Today, 1000).unwrap();

    assert_eq!(dashboard.totals.total, 0);
    assert_eq!(dashboard.cost.total, 0.0);
    assert_eq!(dashboard.cost.coverage, 1.0, "空范围 coverage 必须为 1.0 防 NaN");
    assert!(dashboard.cost.is_complete, "空范围 is_complete 必须为 true");
    assert_eq!(dashboard.cost.unpriced_tokens, 0);
}

// 4. 混合模型计费与未定价降级
#[test]
fn test_mixed_models_cost_aggregation() {
    let mut store = Store::open_in_memory().unwrap();
    let now = 10_000;

    // 1) 已知模型: claude-3-5-sonnet-20241022: 100k input ($0.3), 10k output ($0.15) -> $0.45
    let r1 = make_record("u1", "claude-3-5-sonnet-20241022", now - 1000, 100_000, 10_000, 0, 0);
    // 2) 未知模型: my-custom-relay-model: 10k input, 10k output (总计 20k 未定价)
    let r2 = make_record("u2", "my-custom-relay-model", now - 500, 10_000, 10_000, 0, 0);
    store.upsert_records(&[r1, r2]).unwrap();

    let dashboard = agg::dashboard_at(&store, Range::Today, now).unwrap();
    assert_eq!(dashboard.totals.total, 130_000);

    // 总花费应为已知模型的 $0.45
    assert!((dashboard.cost.total - 0.45).abs() < 1e-6);
    // 覆盖率: 110_000 / 130_000 ≈ 0.846
    assert!((dashboard.cost.coverage - (110_000.0 / 130_000.0)).abs() < 1e-4);
    assert!(!dashboard.cost.is_complete, "含未知模型时 is_complete 必须为 false");
    assert_eq!(dashboard.cost.unpriced_tokens, 20_000);

    // 模型切片
    let sonnet_slice = dashboard.models.iter().find(|m| m.model.contains("sonnet")).unwrap();
    assert!(sonnet_slice.cost.is_some());
    assert!((sonnet_slice.cost.unwrap() - 0.45).abs() < 1e-6);

    let custom_slice = dashboard.models.iter().find(|m| m.model == "my-custom-relay-model").unwrap();
    assert!(custom_slice.cost.is_none(), "未知模型 cost 应为 None");
}

// 5. 免费模型定价（如 glm-4-flash）
#[test]
fn test_free_model_pricing() {
    let prices = PriceTable::bundled();
    let unit = prices.get_unit_price("glm-4-flash").unwrap();
    let cost = unit.calculate(1_000_000, 1_000_000, 1_000_000, 1_000_000);
    assert_eq!(cost, 0.0);

    let mut store = Store::open_in_memory().unwrap();
    let r = make_record("u1", "glm-4-flash", 1000, 500_000, 500_000, 0, 0);
    store.upsert_records(&[r]).unwrap();

    let dashboard = agg::dashboard_at(&store, Range::Today, 2000).unwrap();
    assert_eq!(dashboard.cost.total, 0.0);
    assert_eq!(dashboard.cost.coverage, 1.0, "免费模型计为已定价 (100%)");
    assert!(dashboard.cost.is_complete);
    assert_eq!(dashboard.cost.unpriced_tokens, 0);

    let slice = &dashboard.models[0];
    assert_eq!(slice.cost, Some(0.0), "免费模型 cost 为 Some(0.0) 而非 None");
}

// 6. 多档案分摊与 Profile 级 cost 聚合
#[test]
fn test_profile_level_cost_aggregation() {
    let mut store = Store::open_in_memory().unwrap();
    let now = 20_000;

    // 建立两个 Profile
    let p1 = store.ensure_profile("Anthropic 官方", Some("https://api.anthropic.com")).unwrap();
    let p2 = store.ensure_profile("DeepSeek", Some("https://api.deepseek.com")).unwrap();

    // 档案 1: 使用 sonnet (100k input = $0.3)
    let r1 = make_record("u1", "claude-3-5-sonnet-20241022", now - 2000, 100_000, 0, 0, 0);
    // 档案 2: 使用 deepseek-chat (1M input = $0.14)
    let r2 = make_record("u2", "deepseek-chat", now - 1000, 1_000_000, 0, 0, 0);
    store.upsert_records(&[r1, r2]).unwrap();

    // 手动指定归因区间
    store.reassign_profile_range(Some(p1), now - 2000, now - 1999).unwrap();
    store.reassign_profile_range(Some(p2), now - 1000, now - 999).unwrap();

    let dashboard = agg::dashboard_at(&store, Range::Today, now).unwrap();
    assert_eq!(dashboard.profiles.len(), 2);

    let card_p1 = dashboard.profiles.iter().find(|p| p.profile_id == Some(p1)).unwrap();
    assert!(card_p1.cost.is_some());
    assert!((card_p1.cost.unwrap().total - 0.3).abs() < 1e-6);
    assert!(card_p1.cost.unwrap().is_complete);

    let card_p2 = dashboard.profiles.iter().find(|p| p.profile_id == Some(p2)).unwrap();
    assert!(card_p2.cost.is_some());
    assert!((card_p2.cost.unwrap().total - 0.14).abs() < 1e-6);
    assert!(card_p2.cost.unwrap().is_complete);
}

// 7. PriceManager 磁盘加载与低于阈值容错
#[test]
fn test_price_manager_disk_fallback() {
    let temp = TempDir::new();
    let cache_file = temp.path().join("pricing.json");

    // 写入一个条目数只有 2 个的简陋 JSON（远低于 1000）
    let bad_json = r#"{
        "claude-3-5-sonnet-20241022": { "input_cost_per_token": 0.000003 },
        "sample_spec": {}
    }"#;
    fs::write(&cache_file, bad_json).unwrap();

    // PriceManager 读取该目录，应检测到条目数不足并平稳回退至 bundled
    let mgr = PriceManager::new(temp.path());
    let table = mgr.table();
    let guard = table.read().unwrap();

    // bundled 表包含完整核心模型，不会受 bad_json 破坏
    assert!(guard.len() >= 15);
    assert!(guard.get_unit_price("claude-3-opus-20240229").is_some());
}
