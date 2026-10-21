//! T14 主题切换器（Theme Switcher: Slate/Mint）集成测试
//!
//! 覆盖：
//! 1. AppSettings 主题默认值与 as_str
//! 2. ThemeMode 序列化/反序列化（kebab-case）
//! 3. 旧版 settings.json（无 theme 字段）向前兼容，保留所有已有字段
//! 4. 手改或未知主题名容错回退（ThemeMode::Unknown -> meter-face 语义），不破坏已有设置
//! 5. from_str_loose 宽容转换
//! 6. 磁盘 save / load 往返与损坏容错重建

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use token_watcher_lib::app_settings::{AppSettings, ThemeMode};
use token_watcher_lib::core::agg::TrayMetric;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("tw-t14-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn default_theme_is_meter_face() {
    let settings = AppSettings::default();
    assert_eq!(settings.theme, ThemeMode::MeterFace);
    assert_eq!(settings.theme.as_str(), "meter-face");
}

#[test]
fn theme_mode_as_str_and_from_str_loose() {
    assert_eq!(ThemeMode::MeterFace.as_str(), "meter-face");
    assert_eq!(ThemeMode::Slate.as_str(), "slate");
    assert_eq!(ThemeMode::Mint.as_str(), "mint");
    assert_eq!(ThemeMode::Unknown.as_str(), "meter-face");

    assert_eq!(ThemeMode::from_str_loose("slate"), ThemeMode::Slate);
    assert_eq!(ThemeMode::from_str_loose("mint"), ThemeMode::Mint);
    assert_eq!(
        ThemeMode::from_str_loose("meter-face"),
        ThemeMode::MeterFace
    );
    assert_eq!(
        ThemeMode::from_str_loose("unknown-val"),
        ThemeMode::MeterFace
    );
    assert_eq!(ThemeMode::from_str_loose(""), ThemeMode::MeterFace);
}

#[test]
fn serialization_format() {
    let settings_slate = AppSettings {
        theme: ThemeMode::Slate,
        ..Default::default()
    };
    let json = serde_json::to_string(&settings_slate).unwrap();
    assert!(json.contains(r#""theme":"slate""#));

    let settings_mint = AppSettings {
        theme: ThemeMode::Mint,
        ..Default::default()
    };
    let json = serde_json::to_string(&settings_mint).unwrap();
    assert!(json.contains(r#""theme":"mint""#));

    let settings_meter = AppSettings {
        theme: ThemeMode::MeterFace,
        ..Default::default()
    };
    let json = serde_json::to_string(&settings_meter).unwrap();
    assert!(json.contains(r#""theme":"meter-face""#));
}

#[test]
fn backward_compatibility_without_theme_field() {
    // 模拟 v0.1 旧版 settings.json，没有任何 theme 字段
    let legacy_json = r#"{
        "tray_metric": "cache_read",
        "hud_visible": true,
        "hud_locked": true,
        "hud_x": 120,
        "hud_y": 240
    }"#;

    let settings: AppSettings = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(settings.tray_metric, TrayMetric::CacheRead);
    assert!(settings.hud_visible);
    assert!(settings.hud_locked);
    assert_eq!(settings.hud_x, Some(120));
    assert_eq!(settings.hud_y, Some(240));
    assert_eq!(settings.theme, ThemeMode::MeterFace);
    assert_eq!(settings.theme.as_str(), "meter-face");
}

#[test]
fn resilience_against_unknown_theme_string() {
    // 模拟手改或未来版本降级写下的未知主题名
    let unknown_theme_json = r#"{
        "tray_metric": "input",
        "hud_visible": true,
        "hud_locked": false,
        "hud_x": 450,
        "hud_y": 800,
        "theme": "future-cyberpunk-neon"
    }"#;

    let settings: AppSettings = serde_json::from_str(unknown_theme_json).unwrap();
    // 关键断言：其它字段绝不丢失
    assert_eq!(settings.tray_metric, TrayMetric::Input);
    assert!(settings.hud_visible);
    assert!(!settings.hud_locked);
    assert_eq!(settings.hud_x, Some(450));
    assert_eq!(settings.hud_y, Some(800));
    // 未知主题容错为 Unknown，且 as_str 回退至 meter-face
    assert_eq!(settings.theme, ThemeMode::Unknown);
    assert_eq!(settings.theme.as_str(), "meter-face");
}

#[test]
fn disk_roundtrip_and_corruption_recovery() {
    let dir = temp_dir("theme-settings");
    let path = dir.join("settings.json");

    let original = AppSettings {
        theme: ThemeMode::Mint,
        hud_x: Some(300),
        hud_y: Some(150),
        ..Default::default()
    };
    original.save(&path).unwrap();

    let loaded = AppSettings::load(&path);
    assert_eq!(loaded, original);
    assert_eq!(loaded.theme, ThemeMode::Mint);

    // 写入未知 theme，load 应依然成功解析且不覆写文件
    let custom_json = r#"{
        "tray_metric": "total",
        "hud_visible": false,
        "hud_locked": false,
        "hud_x": 10,
        "hud_y": 20,
        "theme": "mysterious-theme"
    }"#;
    fs::write(&path, custom_json).unwrap();
    let loaded = AppSettings::load(&path);
    assert_eq!(loaded.hud_x, Some(10));
    assert_eq!(loaded.hud_y, Some(20));
    assert_eq!(loaded.theme, ThemeMode::Unknown);
    assert_eq!(loaded.theme.as_str(), "meter-face");

    let _ = fs::remove_dir_all(&dir);
}
