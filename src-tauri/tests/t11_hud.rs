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
        duration_ms: None,
        thoughts_tokens: 0,
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
    // 活跃记录：10 秒前产生，耗时 5000ms（5秒），输出 200，速率 = 200 / 5 = 40 tokens/s
    let mut r = sample_record("msg_active", now_ms - 10_000, (800, 200, 1000, 0));
    r.model = "claude-sonnet".to_string();
    r.duration_ms = Some(5000);
    store.upsert_records(&[r]).unwrap();

    let summary = hud_summary_at(&store, false, now_ms).unwrap();
    assert_eq!(summary.total_today, 2000);
    assert_eq!(summary.session_total, 2000);
    assert_eq!(summary.current_model, "claude-sonnet");
    assert_eq!(summary.input_tokens, 800);
    assert_eq!(summary.output_tokens, 200);
    // 缓存率：1000 / (800 + 1000) = 55.55%
    assert!((summary.cache_rate - 55.55).abs() < 0.1);
    // 纯输出生成速率 (tokens/s)：200 / 5.0 = 40 tokens/s
    assert!((summary.current_rate - 40.0).abs() < 1e-4);
    assert_eq!(summary.active_sessions.len(), 1);
    assert_eq!(summary.active_sessions[0].current_rate, 40.0);
}

#[test]
fn test_hud_summary_two_rounds_rate_with_duration() {
    let mut store = Store::open_in_memory().unwrap();
    let now_ms = 1_700_000_020_000;

    // 轮次 1：20 秒前，输出 300，耗时 6000ms
    let mut r1 = sample_record("turn_1", now_ms - 20_000, (1000, 300, 0, 0));
    r1.session_id = "session_rate".to_string();
    r1.duration_ms = Some(6000);

    // 轮次 2：10 秒前，输出 200，耗时 4000ms
    let mut r2 = sample_record("turn_2", now_ms - 10_000, (1000, 200, 0, 0));
    r2.session_id = "session_rate".to_string();
    r2.duration_ms = Some(4000);

    store.upsert_records(&[r1, r2]).unwrap();

    let summary = hud_summary_at(&store, false, now_ms).unwrap();
    // 最近 2 轮总输出 = 300 + 200 = 500
    // 最近 2 轮总耗时 = 6s + 4s = 10s
    // 速率 = 500 / 10 = 50 tokens/s
    assert_eq!(summary.output_tokens, 500);
    assert!((summary.current_rate - 50.0).abs() < 1e-4);
}

#[test]
fn test_hud_summary_two_rounds_rate_ts_diff_fallback() {
    let mut store = Store::open_in_memory().unwrap();
    let now_ms = 1_700_000_030_000;

    // 轮次 1：25 秒前，输出 400，无精确耗时
    let mut r1 = sample_record("fallback_1", now_ms - 25_000, (1000, 400, 0, 0));
    r1.session_id = "session_diff".to_string();
    r1.duration_ms = None;

    // 轮次 2：15 秒前（时间差 10 秒），输出 200，无精确耗时
    let mut r2 = sample_record("fallback_2", now_ms - 15_000, (1000, 200, 0, 0));
    r2.session_id = "session_diff".to_string();
    r2.duration_ms = None;

    store.upsert_records(&[r1, r2]).unwrap();

    let summary = hud_summary_at(&store, false, now_ms).unwrap();
    // 最近 2 轮总输出 = 400 + 200 = 600
    // 时间差 = 25s - 15s = 10s (<= 120s 窗口)
    // 速率 = 600 / 10 = 60 tokens/s
    assert!((summary.current_rate - 60.0).abs() < 1e-4);
}

#[test]
fn test_hud_summary_multi_sessions_and_opacity_settings() {
    let mut store = Store::open_in_memory().unwrap();
    let now_ms = 1_700_000_050_000;

    for i in 1..=4 {
        let mut r = sample_record(
            &format!("sess_rec_{i}"),
            now_ms - (i * 5_000),
            (500, 100 * i, 0, 0),
        );
        r.session_id = format!("sess_{i}");
        r.model = format!("model_{i}");
        r.duration_ms = Some(2000);
        store.upsert_records(&[r]).unwrap();
    }

    // 请求 limit = 3
    let summary = token_watcher_lib::core::agg::hud_summary_with_options_at(
        &store,
        false,
        &["current_rate".to_string(), "session_total".to_string()],
        3,
        now_ms,
    )
    .unwrap();

    assert_eq!(summary.active_sessions.len(), 3);
    assert_eq!(summary.hud_active_sessions_count, 3);
    assert_eq!(summary.active_sessions[0].session_id, "sess_1");
    assert_eq!(summary.active_sessions[1].session_id, "sess_2");
    assert_eq!(summary.active_sessions[2].session_id, "sess_3");

    // AppSettings opacity 序列化与默认值
    let dir = std::env::temp_dir().join(format!("tw_test_opacity_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");

    let mut settings = AppSettings::default();
    assert!((settings.capsule_opacity - 0.9).abs() < 1e-4);
    assert!((settings.widget_opacity - 0.95).abs() < 1e-4);
    assert_eq!(settings.hud_active_sessions_count, 3);

    settings.capsule_opacity = 0.8;
    settings.widget_opacity = 0.7;
    settings.hud_active_sessions_count = 5;
    settings.save(&path).unwrap();

    let loaded = AppSettings::load(&path);
    assert!((loaded.capsule_opacity - 0.8).abs() < 1e-4);
    assert!((loaded.widget_opacity - 0.7).abs() < 1e-4);
    assert_eq!(loaded.hud_active_sessions_count, 5);
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

#[test]
fn test_app_settings_hud_mode_roundtrip() {
    use token_watcher_lib::app_settings::HudMode;
    let dir = std::env::temp_dir().join(format!("tw_test_hud_mode_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");

    let mut settings = AppSettings::default();
    assert_eq!(settings.hud_mode, HudMode::Widget);
    assert_eq!(settings.hud_mode.as_str(), "widget");

    settings.hud_mode = HudMode::Capsule;
    assert_eq!(settings.hud_mode.as_str(), "capsule");
    settings.save(&path).unwrap();

    let loaded = AppSettings::load(&path);
    assert_eq!(loaded.hud_mode, HudMode::Capsule);

    // 校验尺寸映射
    assert_eq!(
        token_watcher_lib::hud::hud_size_for_mode(HudMode::Widget),
        (320.0, 390.0)
    );
    assert_eq!(
        token_watcher_lib::hud::hud_size_for_mode(HudMode::Capsule),
        (250.0, 34.0)
    );
}

#[test]
fn test_app_settings_independent_mode_positions() {
    use token_watcher_lib::app_settings::HudMode;
    let dir = std::env::temp_dir().join(format!("tw_test_hud_pos_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");

    let mut settings = AppSettings::default();
    assert_eq!(settings.widget_x, None);
    assert_eq!(settings.widget_y, None);
    assert_eq!(settings.capsule_x, None);
    assert_eq!(settings.capsule_y, None);

    // 分别设置小组件与胶囊的不同坐标
    settings.set_position_for_mode(HudMode::Widget, 1200, 30);
    settings.set_position_for_mode(HudMode::Capsule, 500, 20);

    assert_eq!(settings.widget_x, Some(1200));
    assert_eq!(settings.widget_y, Some(30));
    assert_eq!(settings.capsule_x, Some(500));
    assert_eq!(settings.capsule_y, Some(20));
    // 前向兼容：设置形态坐标时同步镜像到旧版 hud_x/hud_y
    assert_eq!(settings.hud_x, Some(500));
    assert_eq!(settings.hud_y, Some(20));

    assert_eq!(
        settings.get_position_for_mode(HudMode::Widget),
        (Some(1200), Some(30))
    );
    assert_eq!(
        settings.get_position_for_mode(HudMode::Capsule),
        (Some(500), Some(20))
    );
    settings.save(&path).unwrap();
    let loaded = AppSettings::load(&path);
    assert_eq!(
        loaded.get_position_for_mode(HudMode::Widget),
        (Some(1200), Some(30))
    );
    assert_eq!(
        loaded.get_position_for_mode(HudMode::Capsule),
        (Some(500), Some(20))
    );

    // 向后兼容测试：旧版 JSON 仅有 hud_x / hud_y
    let legacy_json = r#"{
        "hud_x": 880,
        "hud_y": 60,
        "hud_mode": "widget",
        "hud_visible": true
    }"#;
    let legacy: AppSettings = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(legacy.widget_x, None);
    assert_eq!(legacy.capsule_x, None);
    // 小组件向后兼容继承旧版坐标
    assert_eq!(
        legacy.get_position_for_mode(HudMode::Widget),
        (Some(880), Some(60))
    );
    // 胶囊保持独立 None，以触发专属屏幕居中顶部默认位置
    assert_eq!(legacy.get_position_for_mode(HudMode::Capsule), (None, None));
}

#[test]
fn test_hud_widget_height_bounds() {
    use token_watcher_lib::app_settings::HudMode;
    use token_watcher_lib::hud::{hud_size_for_mode, HUD_WIDGET_HEIGHT, HUD_WIDGET_WIDTH};

    // 基础桌面小组件初始视口尺寸规范为 320x390
    assert_eq!(HUD_WIDGET_WIDTH, 320.0);
    assert_eq!(HUD_WIDGET_HEIGHT, 390.0);
    assert_eq!(hud_size_for_mode(HudMode::Widget), (320.0, 390.0));
}
