//! T5 托盘集成测试：纯逻辑部分（GUI 托盘本体走手验清单）
//!
//! 覆盖：
//! 1. 紧凑数字格式化各档位（999 / 1.2K / 184K / 1.2M / 12M / 1.2B）
//! 2. 完整数字千分位；无数据（0）隐藏 title
//! 3. 设置读写（JSON 形状 { "tray_metric": "cache_read" }）与损坏回退重建
//! 4. 四指标切换的聚合查询正确（复用 T7 fixture 手法，SC7 同源）
//! 5. 托盘文案 i18n：zh* → 中文、其余英文；tooltip 指标名同步双语

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use token_watcher_lib::app_settings::AppSettings;
use token_watcher_lib::core::agg::{self, tray_metric_at, TrayMetric};
use token_watcher_lib::core::parse::UsageRecord;
use token_watcher_lib::core::store::Store;
use token_watcher_lib::tray::{compact_number, display, full_number, lang_from_locale, Lang};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("tw-t5-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn compact_number_tiers() {
    // 原样
    assert_eq!(compact_number(0), "0");
    assert_eq!(compact_number(7), "7");
    assert_eq!(compact_number(999), "999");
    // K：<10K 一位小数（.0 省略），≥10K 整数
    assert_eq!(compact_number(1_000), "1K");
    assert_eq!(compact_number(1_500), "1.5K");
    assert_eq!(compact_number(9_999), "10K");
    assert_eq!(compact_number(12_345), "12K");
    assert_eq!(compact_number(184_321), "184K");
    assert_eq!(compact_number(999_499), "999K");
    // M：999.5K 起进位，避免 "1000K"
    assert_eq!(compact_number(999_500), "1M");
    assert_eq!(compact_number(1_234_567), "1.2M");
    assert_eq!(compact_number(9_999_999), "10M");
    assert_eq!(compact_number(12_345_678), "12M");
    assert_eq!(compact_number(999_499_999), "999M");
    // B
    assert_eq!(compact_number(999_500_000), "1B");
    assert_eq!(compact_number(1_234_567_890), "1.2B");
    assert_eq!(compact_number(12_345_678_901), "12B");
}

#[test]
fn full_number_groups_thousands() {
    assert_eq!(full_number(0), "0");
    assert_eq!(full_number(999), "999");
    assert_eq!(full_number(1_000), "1,000");
    assert_eq!(full_number(184_321), "184,321");
    assert_eq!(full_number(1_234_567), "1,234,567");
}

#[test]
fn display_hides_title_when_empty() {
    let empty = display(Lang::Zh, TrayMetric::Total, 0);
    assert_eq!(empty.title, None);
    assert_eq!(empty.tooltip, "总 Token: 0");

    let value = display(Lang::Zh, TrayMetric::CacheRead, 184_321);
    assert_eq!(value.title.as_deref(), Some("184K"));
    assert_eq!(value.tooltip, "缓存读取 Token: 184,321");

    let english = display(Lang::En, TrayMetric::CacheRead, 184_321);
    assert_eq!(english.title.as_deref(), Some("184K"));
    assert_eq!(english.tooltip, "Cache Read Tokens: 184,321");

    // 聚合返回非负；防御性负值按 0 处理
    assert_eq!(display(Lang::Zh, TrayMetric::Input, -5).title, None);
}

#[test]
fn locale_detection_prefers_chinese_only_for_zh() {
    assert_eq!(lang_from_locale(Some("zh-Hans-CN")), Lang::Zh);
    assert_eq!(lang_from_locale(Some("zh-TW")), Lang::Zh);
    assert_eq!(lang_from_locale(Some("ZH-cn")), Lang::Zh);
    assert_eq!(lang_from_locale(Some("en-US")), Lang::En);
    assert_eq!(lang_from_locale(Some("ja-JP")), Lang::En);
    assert_eq!(lang_from_locale(None), Lang::En);
}

#[test]
fn metric_labels_cover_all_four_in_both_languages() {
    assert_eq!(Lang::Zh.metric_label(TrayMetric::Total), "总 Token");
    assert_eq!(Lang::Zh.metric_label(TrayMetric::Input), "输入 Token");
    assert_eq!(
        Lang::Zh.metric_label(TrayMetric::CacheRead),
        "缓存读取 Token"
    );
    assert_eq!(Lang::Zh.metric_label(TrayMetric::Output), "输出 Token");

    assert_eq!(Lang::En.metric_label(TrayMetric::Total), "Total Tokens");
    assert_eq!(Lang::En.metric_label(TrayMetric::Input), "Input Tokens");
    assert_eq!(
        Lang::En.metric_label(TrayMetric::CacheRead),
        "Cache Read Tokens"
    );
    assert_eq!(Lang::En.metric_label(TrayMetric::Output), "Output Tokens");
}

#[test]
fn settings_roundtrip_shape_and_missing_file() {
    let dir = temp_dir("settings");
    let path = dir.join("settings.json");

    // 首次缺失 → 默认值，不创建文件
    assert_eq!(AppSettings::load(&path), AppSettings::default());
    assert!(!path.exists());

    let custom = AppSettings {
        tray_metric: TrayMetric::CacheRead,
        ..Default::default()
    };
    custom.save(&path).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["tray_metric"], "cache_read");
    assert_eq!(AppSettings::load(&path), custom);
}

#[test]
fn settings_corrupt_falls_back_and_rebuilds() {
    let dir = temp_dir("corrupt");
    let path = dir.join("settings.json");
    fs::write(&path, "{ this is not json").unwrap();

    assert_eq!(AppSettings::load(&path), AppSettings::default());
    // 「重建」：文件已被覆盖为合法默认值
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["tray_metric"], "total");
}

#[test]
fn settings_unknown_metric_and_partial_object_use_default() {
    let dir = temp_dir("unknown");
    let path = dir.join("settings.json");

    fs::write(&path, r#"{"tray_metric":"bogus"}"#).unwrap();
    assert_eq!(AppSettings::load(&path), AppSettings::default());

    fs::write(&path, "{}").unwrap();
    assert_eq!(AppSettings::load(&path), AppSettings::default());
}

fn record(ts_ms: i64) -> UsageRecord {
    UsageRecord {
        record_id: "r1".to_string(),
        session_id: "s1".to_string(),
        timestamp_ms: ts_ms,
        input_tokens: 100,
        output_tokens: 200,
        cache_read_tokens: 300,
        cache_creation_tokens: 400,
        model: "glm-5.3".to_string(),
        entrypoint: "cli".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
    }
}

#[test]
fn tray_metric_query_matches_selected_metric() {
    let mut store = Store::open_in_memory().unwrap();
    let now = agg::now_ms();
    let today = store.local_today().unwrap();
    let (start, end) = store.local_day_bounds(&today).unwrap();
    // 落在今天且尽量处于激活窗口内；临近午夜时退化为今日累计路径
    let ts = (now - 60_000).max(start + 1).min(end - 1);
    store.upsert_records(&[record(ts)]).unwrap();

    assert_eq!(
        tray_metric_at(&store, TrayMetric::Total, now).unwrap(),
        1_000
    );
    assert_eq!(tray_metric_at(&store, TrayMetric::Input, now).unwrap(), 100);
    assert_eq!(
        tray_metric_at(&store, TrayMetric::CacheRead, now).unwrap(),
        300
    );
    assert_eq!(
        tray_metric_at(&store, TrayMetric::Output, now).unwrap(),
        200
    );

    // 设置文件中的指标值反序列化后直接驱动同一查询（菜单切换语义）
    let settings: AppSettings = serde_json::from_str(r#"{"tray_metric":"output"}"#).unwrap();
    assert_eq!(
        tray_metric_at(&store, settings.tray_metric, now).unwrap(),
        200
    );
}
