//! T5 托盘：macOS 菜单栏数字 / Windows 静态图标 + tooltip + 点击弹窗
//!
//! - **macOS**：NSStatusItem 文本 title 显示紧凑数字（1.2K / 184K / 1.2M），
//!   无数据（0）时隐藏 title；tooltip 放完整数字。
//! - **Windows**：静态图标（数字入图标方案已否决：16-20px 画不下），
//!   tooltip 放「指标名: 完整数字」；左键点击显示并聚焦主窗。
//! - **同源（SC7）**：数字一律走 [`crate::core::agg::tray_metric`]，
//!   command/UI 与托盘不各写 SQL。
//! - **刷新（SC2 ≤60s）**：数据线程落库后经内部 channel 通知 + 60s 兜底 tick；
//!   距上次刷新不足 [`MIN_INTERVAL`] 的通知合并到窗口末尾（突发落库不狂查库）。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::app_settings::AppSettings;
use crate::core::agg::{self, TrayMetric};
use crate::core::store::Store;
use crate::watch::WINDOW_LABEL;
/// `tauri.conf.json` 的 `app.trayIcon` 未指定 id 时 Tauri 使用的默认 id
const TRAY_ID: &str = "main";

/// 兜底刷新周期：即使没有数据变化也每 60s 校准一次（SC2 上限）
const TICK: Duration = Duration::from_secs(60);
/// 通知合并窗口：距离上次刷新不足 3s 的数据变化合并成一次查询
const MIN_INTERVAL: Duration = Duration::from_secs(3);

const ID_TOTAL: &str = "metric-total";
const ID_INPUT: &str = "metric-input";
const ID_CACHE_READ: &str = "metric-cache-read";
const ID_OUTPUT: &str = "metric-output";
const ID_CACHE_RATE: &str = "metric-cache-rate";
const ID_TOGGLE_WINDOW: &str = "toggle-window";
const ID_QUIT: &str = "quit";
const ID_HUD_VISIBLE: &str = "hud-visible";
const ID_HUD_MODE: &str = "hud-mode";
const ID_HUD_LOCKED: &str = "hud-locked";
const ID_AUTOSTART: &str = "autostart";
/// 托盘线程命令：数据变化来自数据线程，指标切换来自菜单
pub enum TrayCommand {
    DataChanged,
    MetricChanged(TrayMetric),
    MetricsChanged(Vec<TrayMetric>),
}

/// 一次刷新的显示内容（纯函数计算结果，便于单测）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayDisplay {
    /// macOS title：无数据（0）时为 None（隐藏）
    pub title: Option<String>,
    /// 两平台 tooltip：「指标名: 完整数字」
    pub tooltip: String,
}

/// 托盘文案语言（菜单项 / tooltip）
///
/// 启动时按系统 locale 定一次（zh* → 中文，其余英文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

/// 系统语言检测：`zh-Hans-CN` / `zh-TW` 等一律中文，其余（含检测失败）英文
pub fn detect_lang() -> Lang {
    lang_from_locale(sys_locale::get_locale().as_deref())
}

/// locale → 托盘语言（纯函数，便于测试）
pub fn lang_from_locale(locale: Option<&str>) -> Lang {
    match locale {
        Some(locale) if locale.to_ascii_lowercase().starts_with("zh") => Lang::Zh,
        _ => Lang::En,
    }
}

impl Lang {
    /// 指标全名（菜单项 / tooltip 共用）
    pub fn metric_label(self, metric: TrayMetric) -> &'static str {
        match (self, metric) {
            (Lang::Zh, TrayMetric::Total) => "总 Token",
            (Lang::Zh, TrayMetric::Input) => "输入 Token",
            (Lang::Zh, TrayMetric::CacheRead) => "缓存读取 Token",
            (Lang::Zh, TrayMetric::Output) => "输出 Token",
            (Lang::Zh, TrayMetric::CacheRate) => "缓存率",
            (Lang::En, TrayMetric::Total) => "Total Tokens",
            (Lang::En, TrayMetric::Input) => "Input Tokens",
            (Lang::En, TrayMetric::CacheRead) => "Cache Read Tokens",
            (Lang::En, TrayMetric::Output) => "Output Tokens",
            (Lang::En, TrayMetric::CacheRate) => "Cache Rate",
        }
    }

    pub fn show_window(self) -> &'static str {
        match self {
            Lang::Zh => "显示主面板",
            Lang::En => "Show Main Window",
        }
    }

    pub fn hide_window(self) -> &'static str {
        match self {
            Lang::Zh => "隐藏主面板",
            Lang::En => "Hide Main Window",
        }
    }

    pub fn hud_visible(self) -> &'static str {
        match self {
            Lang::Zh => "显示桌面 HUD",
            Lang::En => "Show Desktop HUD",
        }
    }

    pub fn hud_locked(self) -> &'static str {
        match self {
            Lang::Zh => "锁定 HUD (鼠标穿透)",
            Lang::En => "Lock HUD (Click-Through)",
        }
    }

    pub fn hud_mode_label(self, mode: crate::app_settings::HudMode) -> &'static str {
        match (self, mode) {
            (Lang::Zh, crate::app_settings::HudMode::Widget) => "形态: 卡片小组件 (点击切换)",
            (Lang::Zh, crate::app_settings::HudMode::Capsule) => "形态: 极简胶囊 (点击切换)",
            (Lang::En, crate::app_settings::HudMode::Widget) => {
                "Mode: Widget Card (Click to Switch)"
            }
            (Lang::En, crate::app_settings::HudMode::Capsule) => {
                "Mode: Capsule Bar (Click to Switch)"
            }
        }
    }

    pub fn autostart(self) -> &'static str {
        match self {
            Lang::Zh => "开机自动启动",
            Lang::En => "Launch at Startup",
        }
    }

    fn quit(self) -> &'static str {
        match self {
            Lang::Zh => "退出",
            Lang::En => "Quit",
        }
    }
}

/// 单指标显示内容格式化
pub fn display_metric(lang: Lang, metric: TrayMetric, value: i64) -> (Option<String>, String) {
    let value = value.max(0);
    let title_part = if metric == TrayMetric::CacheRate {
        (value > 0).then(|| format!("{value}%"))
    } else {
        (value > 0).then(|| compact_number(value))
    };
    let tooltip_part = if metric == TrayMetric::CacheRate {
        format!("{}: {}%", lang.metric_label(metric), value)
    } else {
        format!("{}: {}", lang.metric_label(metric), full_number(value))
    };
    (title_part, tooltip_part)
}

/// 多指标显示内容（1-2 个指标，用 " • " 隔开）
pub fn display_multi(lang: Lang, metrics: &[TrayMetric], values: &[i64]) -> TrayDisplay {
    let mut titles = Vec::new();
    let mut tooltips = Vec::new();
    for (m, v) in metrics.iter().zip(values.iter()) {
        let (t, tip) = display_metric(lang, *m, *v);
        if let Some(t_str) = t {
            titles.push(t_str);
        }
        tooltips.push(tip);
    }
    let title = if titles.is_empty() {
        None
    } else {
        Some(titles.join(" • "))
    };
    let tooltip = tooltips.join(" • ");
    TrayDisplay { title, tooltip }
}

/// 计算单指标显示内容：兼容原单测
pub fn display(lang: Lang, metric: TrayMetric, value: i64) -> TrayDisplay {
    display_multi(lang, &[metric], &[value])
}

/// 菜单栏紧凑数字：999 / 1.2K / 184K / 1.2M / 12M / 1.2B
pub fn compact_number(n: i64) -> String {
    let n = n.max(0);
    if n < 1_000 {
        return n.to_string();
    }
    if n < 999_500 {
        // 999_500 起让位给 M，避免出现 "1000K"
        return scaled(n as f64, 1_000.0, "K", n < 10_000);
    }
    if n < 999_500_000 {
        return scaled(n as f64, 1_000_000.0, "M", n < 10_000_000);
    }
    scaled(n as f64, 1_000_000_000.0, "B", n < 10_000_000_000)
}

fn scaled(value: f64, divisor: f64, suffix: &str, one_decimal: bool) -> String {
    let scaled = value / divisor;
    if one_decimal {
        let text = format!("{scaled:.1}");
        let text = text.strip_suffix(".0").unwrap_or(&text);
        format!("{text}{suffix}")
    } else {
        format!("{}{suffix}", scaled.round() as i64)
    }
}

/// 完整数字（千分位）：184,321
pub fn full_number(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn metric_id(metric: TrayMetric) -> &'static str {
    match metric {
        TrayMetric::Total => ID_TOTAL,
        TrayMetric::Input => ID_INPUT,
        TrayMetric::CacheRead => ID_CACHE_READ,
        TrayMetric::Output => ID_OUTPUT,
        TrayMetric::CacheRate => ID_CACHE_RATE,
    }
}

fn metric_from_id(id: &str) -> Option<TrayMetric> {
    match id {
        ID_TOTAL => Some(TrayMetric::Total),
        ID_INPUT => Some(TrayMetric::Input),
        ID_CACHE_READ => Some(TrayMetric::CacheRead),
        ID_OUTPUT => Some(TrayMetric::Output),
        ID_CACHE_RATE => Some(TrayMetric::CacheRate),
        _ => None,
    }
}

/// setup 时保存的菜单项句柄与语言：供 `WindowEvent::Focused` 等外部路径
/// （如 single-instance 唤起窗口）同步「显示/隐藏悬浮窗」文案。
static WINDOW_ITEM: OnceLock<MenuItem<Wry>> = OnceLock::new();
static HUD_VISIBLE_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();
static HUD_MODE_ITEM: OnceLock<MenuItem<Wry>> = OnceLock::new();
static HUD_LOCKED_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();
static AUTOSTART_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();
static SESSIONS_ITEMS: OnceLock<Vec<(usize, CheckMenuItem<Wry>)>> = OnceLock::new();
static CAPSULE_OPACITY_ITEMS: OnceLock<Vec<(f64, CheckMenuItem<Wry>)>> = OnceLock::new();
static WIDGET_OPACITY_ITEMS: OnceLock<Vec<(f64, CheckMenuItem<Wry>)>> = OnceLock::new();
static LANG: OnceLock<Lang> = OnceLock::new();
/// 托盘初始化：复用 tauri.conf.json 创建的图标，挂菜单 / 事件 / 刷新线程。
///
/// 返回数据线程用的通知发送端（[`TrayCommand::DataChanged`]）。
pub fn setup(
    app: &AppHandle,
    settings_path: &Path,
    db_path: &Path,
    initial_settings: &AppSettings,
) -> tauri::Result<Sender<TrayCommand>> {
    let tray = app.tray_by_id(TRAY_ID).ok_or_else(|| {
        tauri::Error::AssetNotFound(format!(
            "托盘 `{TRAY_ID}` 未由 tauri.conf.json app.trayIcon 创建"
        ))
    })?;

    let lang = detect_lang();
    let initial_metrics = initial_settings.effective_tray_metrics();
    let metric_items = build_metric_items(app, &initial_metrics, lang)?;
    let hud_visible_item = CheckMenuItem::with_id(
        app,
        ID_HUD_VISIBLE,
        lang.hud_visible(),
        true,
        initial_settings.hud_visible,
        None::<&str>,
    )?;
    let hud_mode_item = MenuItem::with_id(
        app,
        ID_HUD_MODE,
        lang.hud_mode_label(initial_settings.hud_mode),
        initial_settings.hud_visible,
        None::<&str>,
    )?;
    let hud_locked_item = CheckMenuItem::with_id(
        app,
        ID_HUD_LOCKED,
        lang.hud_locked(),
        initial_settings.hud_visible,
        initial_settings.hud_locked,
        None::<&str>,
    )?;
    let mut session_items = Vec::new();
    for (count, label) in [(1, "1 条"), (2, "2 条"), (3, "3 条 (默认)"), (5, "5 条")] {
        let item = CheckMenuItem::with_id(
            app,
            format!("hud-sessions-{count}"),
            label,
            true,
            initial_settings.hud_active_sessions_count == count,
            None::<&str>,
        )?;
        session_items.push((count, item));
    }
    let session_refs: Vec<&dyn IsMenuItem<Wry>> = session_items
        .iter()
        .map(|(_, it)| it as &dyn IsMenuItem<Wry>)
        .collect();
    let sessions_submenu = Submenu::with_items(app, "活跃会话展示数", true, &session_refs)?;

    let mut cap_op_items = Vec::new();
    for (val, label) in [
        (1.0, "100% (不透明)"),
        (0.9, "90%"),
        (0.8, "80%"),
        (0.7, "70%"),
        (0.5, "50%"),
    ] {
        let is_checked = (initial_settings.capsule_opacity - val).abs() < 0.01;
        let item = CheckMenuItem::with_id(
            app,
            format!("hud-capsule-opacity-{val}"),
            label,
            true,
            is_checked,
            None::<&str>,
        )?;
        cap_op_items.push((val, item));
    }
    let cap_op_refs: Vec<&dyn IsMenuItem<Wry>> = cap_op_items
        .iter()
        .map(|(_, it)| it as &dyn IsMenuItem<Wry>)
        .collect();
    let capsule_opacity_submenu = Submenu::with_items(app, "极简胶囊透明度", true, &cap_op_refs)?;

    let mut wid_op_items = Vec::new();
    for (val, label) in [
        (1.0, "100% (不透明)"),
        (0.9, "90%"),
        (0.8, "80%"),
        (0.7, "70%"),
        (0.5, "50%"),
    ] {
        let is_checked = (initial_settings.widget_opacity - val).abs() < 0.01;
        let item = CheckMenuItem::with_id(
            app,
            format!("hud-widget-opacity-{val}"),
            label,
            true,
            is_checked,
            None::<&str>,
        )?;
        wid_op_items.push((val, item));
    }
    let wid_op_refs: Vec<&dyn IsMenuItem<Wry>> = wid_op_items
        .iter()
        .map(|(_, it)| it as &dyn IsMenuItem<Wry>)
        .collect();
    let widget_opacity_submenu = Submenu::with_items(app, "卡片小组件透明度", true, &wid_op_refs)?;

    let autostart_item = CheckMenuItem::with_id(
        app,
        ID_AUTOSTART,
        lang.autostart(),
        true,
        initial_settings.is_autostart_enabled(),
        None::<&str>,
    )?;
    let window_visible = app
        .get_webview_window(WINDOW_LABEL)
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    let window_text = if window_visible {
        lang.hide_window()
    } else {
        lang.show_window()
    };
    let toggle = MenuItem::with_id(app, ID_TOGGLE_WINDOW, window_text, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, lang.quit(), true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;

    let mut items: Vec<&dyn IsMenuItem<Wry>> = metric_items
        .iter()
        .map(|(_, item)| item as &dyn IsMenuItem<Wry>)
        .collect();
    items.push(&sep1);
    items.push(&hud_visible_item);
    items.push(&hud_mode_item);
    items.push(&hud_locked_item);
    items.push(&sessions_submenu);
    items.push(&capsule_opacity_submenu);
    items.push(&widget_opacity_submenu);
    items.push(&sep2);
    items.push(&autostart_item);
    items.push(&toggle);
    items.push(&sep3);
    items.push(&quit);
    tray.set_menu(Some(Menu::with_items(app, &items)?))?;

    let _ = HUD_VISIBLE_ITEM.set(hud_visible_item);
    let _ = HUD_MODE_ITEM.set(hud_mode_item);
    let _ = HUD_LOCKED_ITEM.set(hud_locked_item);
    let _ = SESSIONS_ITEMS.set(session_items);
    let _ = CAPSULE_OPACITY_ITEMS.set(cap_op_items);
    let _ = WIDGET_OPACITY_ITEMS.set(wid_op_items);
    let _ = AUTOSTART_ITEM.set(autostart_item);

    // 文案同步用的全局句柄（set 失败只可能是重复初始化，忽略）
    let _ = WINDOW_ITEM.set(toggle);
    let _ = LANG.set(lang);
    // Windows：左键留给「显示主窗」；macOS 左键保持「弹出菜单」
    tray.set_show_menu_on_left_click(!cfg!(target_os = "windows"))?;

    let click_app = app.clone();
    tray.on_tray_icon_event(move |_tray, event| {
        // 平台差异集中在此：macOS 左键语义是弹菜单，不抢窗口
        if !cfg!(target_os = "windows") {
            return;
        }
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            show_main_window(&click_app);
        }
    });

    let (tx, rx) = mpsc::channel();
    let menu_items = metric_items;
    let settings_path = settings_path.to_path_buf();
    let menu_tx = tx.clone();
    tray.on_menu_event(move |app, event| {
        let id = event.id().as_ref();
        if let Some(metric) = metric_from_id(id) {
            let mut settings = AppSettings::load(&settings_path);
            let mut current = settings.effective_tray_metrics();
            if current.contains(&metric) {
                // 最少选择 1 个
                if current.len() > 1 {
                    current.retain(|m| *m != metric);
                }
            } else {
                // 最多选择 2 个：已选 2 个时替换掉最早选中的
                if current.len() >= 2 {
                    current.remove(0);
                }
                current.push(metric);
            }
            settings.tray_metrics = current.clone();
            settings.tray_metric = current.first().copied().unwrap_or(TrayMetric::Total);
            if let Err(e) = settings.save(&settings_path) {
                eprintln!("[token-watcher] 保存托盘指标设置失败: {e}");
            }
            set_checked(&menu_items, &current);
            let _ = menu_tx.send(TrayCommand::MetricsChanged(current));
        } else if id == ID_HUD_VISIBLE {
            let mut settings = AppSettings::load(&settings_path);
            settings.hud_visible = !settings.hud_visible;
            let _ = settings.save(&settings_path);
            let (x, y) = settings.get_position_for_mode(settings.hud_mode);
            let _ = crate::hud::ensure_hud_window(
                app,
                settings.hud_visible,
                settings.hud_locked,
                settings.hud_mode,
                x,
                y,
            );
            sync_hud_menu_items(app, &settings);
        } else if id == ID_HUD_MODE {
            use tauri::Emitter;
            let mut settings = AppSettings::load(&settings_path);
            let next_mode = match settings.hud_mode {
                crate::app_settings::HudMode::Widget => crate::app_settings::HudMode::Capsule,
                crate::app_settings::HudMode::Capsule => crate::app_settings::HudMode::Widget,
            };
            settings.hud_mode = next_mode;
            let _ = settings.save(&settings_path);
            if let Some(win) = app.get_webview_window(crate::hud::HUD_LABEL) {
                let (target_w, target_h) = crate::hud::hud_size_for_mode(next_mode);
                let _ = win.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
                    target_w, target_h,
                )));
                let (x, y) = settings.get_position_for_mode(next_mode);
                let (safe_x, safe_y) = crate::hud::calculate_safe_position(app, next_mode, x, y);
                let _ = win.set_position(tauri::Position::Logical(tauri::LogicalPosition::new(
                    safe_x as f64,
                    safe_y as f64,
                )));
            }
            sync_hud_menu_items(app, &settings);
            let _ = app.emit("hud-mode-changed", next_mode.as_str());
            let _ = app.emit("hud-updated", ());
        } else if id == ID_HUD_LOCKED {
            let mut settings = AppSettings::load(&settings_path);
            settings.hud_locked = !settings.hud_locked;
            let _ = settings.save(&settings_path);
            if let Some(win) = app.get_webview_window(crate::hud::HUD_LABEL) {
                let _ = crate::hud::apply_hud_locked(&win, settings.hud_locked);
            }
            sync_hud_menu_items(app, &settings);
        } else if id == ID_AUTOSTART {
            use tauri_plugin_autostart::ManagerExt;
            let mut settings = AppSettings::load(&settings_path);
            let current_enabled = settings.is_autostart_enabled();
            let new_enabled = !current_enabled;
            let autolaunch = app.autolaunch();
            if new_enabled {
                let _ = autolaunch.enable();
            } else {
                let _ = autolaunch.disable();
            }
            settings.autostart = Some(new_enabled);
            let _ = settings.save(&settings_path);
            sync_autostart_menu_item(new_enabled);
        } else if id == ID_TOGGLE_WINDOW {
            toggle_main_window(app);
        } else if let Some(cnt_str) = id.strip_prefix("hud-sessions-") {
            if let Ok(count) = cnt_str.parse::<usize>() {
                let mut settings = AppSettings::load(&settings_path);
                settings.hud_active_sessions_count = count.clamp(1, 5);
                let _ = settings.save(&settings_path);
                sync_hud_menu_items(app, &settings);
                let _ = app.emit("hud-settings-changed", ());
                let _ = app.emit("hud-updated", ());
            }
        } else if let Some(op_str) = id.strip_prefix("hud-capsule-opacity-") {
            if let Ok(val) = op_str.parse::<f64>() {
                let mut settings = AppSettings::load(&settings_path);
                settings.capsule_opacity = val.clamp(0.5, 1.0);
                let _ = settings.save(&settings_path);
                sync_hud_menu_items(app, &settings);
                let _ = app.emit("hud-settings-changed", ());
                let _ = app.emit("hud-updated", ());
            }
        } else if let Some(op_str) = id.strip_prefix("hud-widget-opacity-") {
            if let Ok(val) = op_str.parse::<f64>() {
                let mut settings = AppSettings::load(&settings_path);
                settings.widget_opacity = val.clamp(0.5, 1.0);
                let _ = settings.save(&settings_path);
                sync_hud_menu_items(app, &settings);
                let _ = app.emit("hud-settings-changed", ());
                let _ = app.emit("hud-updated", ());
            }
        } else if id == ID_QUIT {
            app.exit(0);
        }
    });

    spawn_refresher(tray, db_path.to_path_buf(), initial_metrics, lang, rx);
    Ok(tx)
}

fn build_metric_items(
    app: &AppHandle,
    current: &[TrayMetric],
    lang: Lang,
) -> tauri::Result<Vec<(TrayMetric, CheckMenuItem<Wry>)>> {
    const METRICS: [TrayMetric; 5] = [
        TrayMetric::Total,
        TrayMetric::Input,
        TrayMetric::CacheRead,
        TrayMetric::Output,
        TrayMetric::CacheRate,
    ];
    METRICS
        .into_iter()
        .map(|metric| {
            let item = CheckMenuItem::with_id(
                app,
                metric_id(metric),
                lang.metric_label(metric),
                true,
                current.contains(&metric),
                None::<&str>,
            )?;
            Ok((metric, item))
        })
        .collect()
}

fn set_checked(items: &[(TrayMetric, CheckMenuItem<Wry>)], current: &[TrayMetric]) {
    for (metric, item) in items {
        let _ = item.set_checked(current.contains(metric));
    }
}
/// 显示并聚焦主窗。**必须 set_focus**：T3/T4 的水合信号依赖
/// `WindowEvent::Focused(true)`（只 show 不 focus 会让 UI 等下一个数据事件）。
fn show_main_window(app: &AppHandle) {
    let _ = crate::hud::show_main_window(app);
}

fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
        return;
    };
    let is_visible = window.is_visible().unwrap_or(false);
    let is_minimized = window.is_minimized().unwrap_or(false);
    if is_visible && !is_minimized {
        let _ = window.hide();
        #[cfg(target_os = "macos")]
        {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
        sync_window_item_label(app);
    } else {
        show_main_window(app);
    }
}

/// 同步「显示/隐藏主面板」文案。窗口也可能被菜单以外的路径显示
/// （single-instance 唤起 → lib.rs 的 Focused 事件），因此单独暴露。
pub fn sync_window_item_label(app: &AppHandle) {
    let (Some(item), Some(lang)) = (WINDOW_ITEM.get(), LANG.get()) else {
        return;
    };
    let is_active = app
        .get_webview_window(WINDOW_LABEL)
        .map(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
        .unwrap_or(false);
    let text = if is_active {
        lang.hide_window()
    } else {
        lang.show_window()
    };
    let _ = item.set_text(text);
}

/// 同步 HUD 菜单项勾选与可用状态
pub fn sync_hud_menu_items(_app: &AppHandle, settings: &AppSettings) {
    if let Some(item) = HUD_VISIBLE_ITEM.get() {
        let _ = item.set_checked(settings.hud_visible);
    }
    if let Some(item) = HUD_MODE_ITEM.get() {
        let lang = LANG.get().copied().unwrap_or(Lang::Zh);
        let _ = item.set_text(lang.hud_mode_label(settings.hud_mode));
        let _ = item.set_enabled(settings.hud_visible);
    }
    if let Some(item) = HUD_LOCKED_ITEM.get() {
        let _ = item.set_checked(settings.hud_locked);
        let _ = item.set_enabled(settings.hud_visible);
    }
    if let Some(items) = SESSIONS_ITEMS.get() {
        for (count, item) in items {
            let _ = item.set_checked(*count == settings.hud_active_sessions_count);
        }
    }
    if let Some(items) = CAPSULE_OPACITY_ITEMS.get() {
        for (val, item) in items {
            let _ = item.set_checked((settings.capsule_opacity - *val).abs() < 0.01);
        }
    }
    if let Some(items) = WIDGET_OPACITY_ITEMS.get() {
        for (val, item) in items {
            let _ = item.set_checked((settings.widget_opacity - *val).abs() < 0.01);
        }
    }
}

/// 同步自启动菜单项勾选状态
pub fn sync_autostart_menu_item(enabled: bool) {
    if let Some(item) = AUTOSTART_ITEM.get() {
        let _ = item.set_checked(enabled);
    }
}

/// 托盘刷新线程：数据变化通知 + 60s 兜底；合并窗口内不重复查库。
fn spawn_refresher(
    tray: TrayIcon<Wry>,
    db_path: PathBuf,
    mut metrics: Vec<TrayMetric>,
    lang: Lang,
    rx: Receiver<TrayCommand>,
) {
    let spawned = thread::Builder::new()
        .name("token-watcher-tray".into())
        .spawn(move || {
            let mut store: Option<Store> = None;
            let mut last = Instant::now();
            let mut first = true;
            let mut pending = false;
            let mut force = false;
            loop {
                let now = Instant::now();
                let elapsed = now.duration_since(last);
                let due_now =
                    force || elapsed >= TICK || (pending && (first || elapsed >= MIN_INTERVAL));
                if due_now {
                    refresh(&tray, &db_path, &metrics, lang, &mut store);
                    last = Instant::now();
                    first = false;
                    pending = false;
                    force = false;
                    continue;
                }
                let due = if pending {
                    last + MIN_INTERVAL
                } else {
                    last + TICK
                };
                match rx.recv_timeout(due.saturating_duration_since(now)) {
                    Ok(TrayCommand::DataChanged) => pending = true,
                    Ok(TrayCommand::MetricChanged(m)) => {
                        metrics = vec![m];
                        force = true;
                    }
                    Ok(TrayCommand::MetricsChanged(ms)) => {
                        metrics = ms;
                        force = true;
                    }
                    Err(RecvTimeoutError::Timeout) => pending = true,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("[token-watcher] 托盘刷新线程启动失败: {e}");
    }
}

fn refresh(
    tray: &TrayIcon<Wry>,
    db_path: &Path,
    metrics: &[TrayMetric],
    lang: Lang,
    store: &mut Option<Store>,
) {
    if store.is_none() {
        match Store::open(db_path) {
            Ok(opened) => *store = Some(opened),
            Err(e) => {
                eprintln!("[token-watcher] 托盘读取数据库失败（稍后重试）: {e}");
                return;
            }
        }
    }
    let Some(db) = store.as_ref() else {
        return;
    };
    let mut values = Vec::new();
    for &m in metrics {
        match agg::tray_metric(db, m) {
            Ok(v) => values.push(v),
            Err(e) => {
                eprintln!("[token-watcher] 托盘聚合查询失败（下次重建连接）: {e}");
                *store = None;
                return;
            }
        }
    }

    let shown = display_multi(lang, metrics, &values);
    if cfg!(target_os = "macos") {
        let _ = tray.set_title(shown.title.as_deref());
    }
    let _ = tray.set_tooltip(Some(&shown.tooltip));
    eprintln!(
        "[token-watcher] 托盘刷新: metrics={metrics:?} values={values:?} title={:?} tooltip={:?}",
        shown.title, shown.tooltip
    );
}
