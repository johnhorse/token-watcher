//! HUD 模式集成测试：轻量聚合、指标口径、坐标持久化与穿透标志验证

use std::fs;

use token_watcher_lib::app_settings::AppSettings;
use token_watcher_lib::core::agg::hud_summary_at;
use token_watcher_lib::core::parse::UsageRecord;
use token_watcher_lib::core::store::Store;

fn sample_record(msg_id: &str, ts_ms: i64, tokens: (i64, i64, i64, i64)) -> UsageRecord {
    UsageRecord {
        record_id: msg_id.into(),
        session_id: "sess_hud_test".into(),
        timestamp_ms: ts_ms,
        input_tokens: tokens.0,
        output_tokens: tokens.1,
        cache_read_tokens: tokens.2,
        cache_creation_tokens: tokens.3,
        model: "claude-3-5-sonnet".into(),
        entrypoint: "cli".into(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
    }
}

#[test]
fn test_hud_summary_empty_store() {
    let store = Store::open_in_memory().unwrap();
    let now_ms = 1_700_000_000_000;
    let summary = hud_summary_at(&store, false, now_ms).unwrap();
    assert_eq!(summary.total_today, 0);
    assert_eq!(summary.rate_per_min, 0);
    assert!(!summary.hud_locked);

    let summary_locked = hud_summary_at(&store, true, now_ms).unwrap();
    assert!(summary_locked.hud_locked);
}

#[test]
fn test_hud_summary_recent_pulses_and_rate() {
    let mut store = Store::open_in_memory().unwrap();
    let now_ms = 1_700_000_300_000; // 当前时间点

    // 记录 1：1 分钟前（落在 5 分钟窗口内，输入 500，输出 500 = 1000 tokens）
    let r1 = sample_record("msg_1", now_ms - 60_000, (500, 500, 0, 0));
    // 记录 2：3 分钟前（落在 5 分钟窗口内，输入 1000，输出 1000 = 2000 tokens）
    let r2 = sample_record("msg_2", now_ms - 180_000, (1000, 1000, 0, 0));
    // 记录 3：10 分钟前（超出 5 分钟速率窗口，但是在今日之内，输入 3000，输出 0 = 3000 tokens）
    let r3 = sample_record("msg_3", now_ms - 600_000, (3000, 0, 0, 0));

    store.upsert_records(&[r1, r2, r3]).unwrap();

    let summary = hud_summary_at(&store, false, now_ms).unwrap();
    // 今日累计总 Token = 1000 + 2000 + 3000 = 6000
    assert_eq!(summary.total_today, 6000);
    // 5 分钟内燃烧速率 = (1000 + 2000) / 5 = 3000 / 5 = 600 tokens/min
    assert_eq!(summary.rate_per_min, 600);
}

#[test]
fn test_hud_summary_rate_idle_after_inactive() {
    let mut store = Store::open_in_memory().unwrap();
    let now_ms = 1_700_000_000_000;

    // 记录在 20 分钟前
    let r_old = sample_record("msg_old", now_ms - 20 * 60 * 1000, (10_000, 2_000, 0, 0));
    store.upsert_records(&[r_old]).unwrap();

    let summary = hud_summary_at(&store, false, now_ms).unwrap();
    assert_eq!(summary.total_today, 12_000);
    // 超过 5 分钟窗口，燃烧速率应当归零静息
    assert_eq!(summary.rate_per_min, 0);
}

#[test]
fn test_hud_summary_six_metrics_and_real_token_rate() {
    let mut store = Store::open_in_memory().unwrap();
    let now_ms = 1_700_000_010_000;

    // 活跃记录：10 秒前产生，包含 800 输入、200 输出、1000 缓存读取，模型为 claude-sonnet
    let mut r = sample_record("msg_active", now_ms - 10_000, (800, 200, 1000, 0));
    r.model = "claude-sonnet".to_string();
    store.upsert_records(&[r]).unwrap();

    let summary = hud_summary_at(&store, false, now_ms).unwrap();
    assert_eq!(summary.total_today, 2000);
    assert_eq!(summary.session_total, 2000);
    assert_eq!(summary.current_model, "claude-sonnet");
    assert_eq!(summary.input_tokens, 800);
    assert_eq!(summary.output_tokens, 200);
    // 缓存率：1000 / (800 + 1000) = 55.55%
    assert!((summary.cache_rate - 55.55).abs() < 0.1);
    // 纯输出生成速率 (tokens/s)：200 / 10s = 20 tokens/s（不再是上千的 K/s）
    assert!((summary.current_rate - 20.0).abs() < 1.0);
}

#[test]
fn test_app_settings_hud_fields_roundtrip() {
    let dir = std::env::temp_dir().join(format!("tw_test_settings_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");

    let mut settings = AppSettings::default();
    assert!(!settings.hud_visible);
    assert!(!settings.hud_locked);
    assert_eq!(settings.hud_x, None);
    assert_eq!(settings.hud_y, None);

    settings.hud_visible = true;
    settings.hud_locked = true;
    settings.hud_x = Some(1440);
    settings.hud_y = Some(80);
    settings.save(&path).unwrap();

    let loaded = AppSettings::load(&path);
    assert_eq!(loaded, settings);
    assert!(loaded.hud_visible);
    assert!(loaded.hud_locked);
    assert_eq!(loaded.hud_x, Some(1440));
    assert_eq!(loaded.hud_y, Some(80));

    // 负坐标支持（副屏位于左侧或上方场景）
    settings.hud_x = Some(-320);
    settings.hud_y = Some(-100);
    settings.save(&path).unwrap();
    let loaded_neg = AppSettings::load(&path);
    assert_eq!(loaded_neg.hud_x, Some(-320));
    assert_eq!(loaded_neg.hud_y, Some(-100));
}
