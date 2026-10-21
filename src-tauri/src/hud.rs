//! 桌面角落 HUD 模式（Desktop Corner HUD）窗口管理与穿透控制
//!
//! 规约：docs/designs/v0.2-hud-mode-spec.md

use tauri::menu::{ContextMenu, Menu};
use tauri::{AppHandle, Manager, Position, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const HUD_LABEL: &str = "hud";
pub const HUD_CAPSULE_WIDTH: f64 = 250.0;
pub const HUD_CAPSULE_HEIGHT: f64 = 34.0;
pub const HUD_WIDGET_WIDTH: f64 = 320.0;
pub const HUD_WIDGET_HEIGHT: f64 = 390.0;

pub const HUD_WIDTH: f64 = HUD_WIDGET_WIDTH;
pub const HUD_HEIGHT: f64 = HUD_WIDGET_HEIGHT;

pub fn hud_size_for_mode(mode: crate::app_settings::HudMode) -> (f64, f64) {
    match mode {
        crate::app_settings::HudMode::Widget => (HUD_WIDGET_WIDTH, HUD_WIDGET_HEIGHT),
        crate::app_settings::HudMode::Capsule => (HUD_CAPSULE_WIDTH, HUD_CAPSULE_HEIGHT),
    }
}

/// 计算 HUD 窗口安全坐标（P1-2 多显示器拔插与越界防护）
pub fn calculate_safe_position(
    app: &AppHandle,
    saved_x: Option<i32>,
    saved_y: Option<i32>,
) -> (i32, i32) {
    let monitors = app.available_monitors().unwrap_or_default();

    if let (Some(x), Some(y)) = (saved_x, saved_y) {
        // 校验目标矩形是否与任一可用屏幕有至少 32px × 32px 的可见交集
        let hud_w = HUD_CAPSULE_WIDTH as i32;
        let hud_h = HUD_CAPSULE_HEIGHT as i32;
        for m in &monitors {
            let scale = m.scale_factor();
            let pos = m.position();
            let size = m.size();

            let m_x = (pos.x as f64 / scale) as i32;
            let m_y = (pos.y as f64 / scale) as i32;
            let m_w = (size.width as f64 / scale) as i32;
            let m_h = (size.height as f64 / scale) as i32;

            let intersect_x1 = x.max(m_x);
            let intersect_y1 = y.max(m_y);
            let intersect_x2 = (x + hud_w).min(m_x + m_w);
            let intersect_y2 = (y + hud_h).min(m_y + m_h);

            if intersect_x2 - intersect_x1 >= 32 && intersect_y2 - intersect_y1 >= 32 {
                return (x, y);
            }
        }
    }

    // 回退到主屏幕或第一个屏幕右上角
    let target_monitor = app
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| monitors.into_iter().next());

    if let Some(m) = target_monitor {
        let scale = m.scale_factor();
        let pos = m.position();
        let size = m.size();

        let m_x = (pos.x as f64 / scale) as i32;
        let m_y = (pos.y as f64 / scale) as i32;
        let m_w = (size.width as f64 / scale) as i32;

        let right_x = m_x + m_w - (HUD_WIDTH as i32) - 24;
        let top_y = m_y + 24;
        (right_x, top_y)
    } else {
        (100, 24)
    }
}

/// 确保 HUD 窗口存在并按要求更新其可见性与锁定状态
pub fn ensure_hud_window(
    app: &AppHandle,
    visible: bool,
    locked: bool,
    mode: crate::app_settings::HudMode,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    let (target_w, target_h) = hud_size_for_mode(mode);
    if let Some(window) = app.get_webview_window(HUD_LABEL) {
        if visible {
            let (target_x, target_y) = calculate_safe_position(app, x, y);
            let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
                target_w, target_h,
            )));
            let _ = window.set_position(Position::Logical(tauri::LogicalPosition::new(
                target_x as f64,
                target_y as f64,
            )));
            window.show().map_err(|e| e.to_string())?;
            apply_hud_locked(&window, locked)?;
        } else {
            window.hide().map_err(|e| e.to_string())?;
        }
        return Ok(());
    }

    if visible {
        let (target_x, target_y) = calculate_safe_position(app, x, y);
        let window =
            WebviewWindowBuilder::new(app, HUD_LABEL, WebviewUrl::App("index.html".into()))
                .title("token-watcher-hud")
                .inner_size(target_w, target_h)
                .resizable(false)
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .shadow(false)
                .position(target_x as f64, target_y as f64)
                .build()
                .map_err(|e| e.to_string())?;

        apply_hud_locked(&window, locked)?;
    }

    Ok(())
}

/// 设置鼠标穿透状态（P1-4 加固：解除锁定后强制刷新焦点与置顶，促使 DWM 重排）
pub fn apply_hud_locked(window: &WebviewWindow, locked: bool) -> Result<(), String> {
    window
        .set_ignore_cursor_events(locked)
        .map_err(|e| e.to_string())?;

    if !locked {
        let _ = window.set_focus();
        let _ = window.set_always_on_top(true);
    }

    Ok(())
}

/// 唤醒并聚焦主窗口
pub fn show_main_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(crate::watch::WINDOW_LABEL) {
        #[cfg(target_os = "macos")]
        {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        }
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        crate::tray::sync_window_item_label(app);
        if let Some(state) = app.try_state::<crate::AppState>() {
            if let Ok(guard) = state.data.lock() {
                if let Some(data) = guard.as_ref() {
                    data.window_shown();
                }
            }
        }
        Ok(())
    } else {
        Err("主窗口不存在".to_string())
    }
}

/// 弹出 HUD 原生右键菜单（配置 6 项指标与锁定开关）
pub fn show_hud_context_menu(
    app: &AppHandle,
    settings_path: &std::path::Path,
) -> Result<(), String> {
    let Some(window) = app.get_webview_window(HUD_LABEL) else {
        return Err("HUD 窗口不存在".to_string());
    };
    let settings = crate::app_settings::AppSettings::load(settings_path);
    let active_metrics = settings.effective_hud_metrics();

    use tauri::menu::{CheckMenuItem, IsMenuItem, PredefinedMenuItem};
    let metric_defs = [
        ("current_rate", "当前速率"),
        ("session_total", "激活会话总 Token"),
        ("current_model", "当前模型"),
        ("cache_rate", "缓存率"),
        ("input_tokens", "输入 Token 数"),
        ("output_tokens", "输出 Token 数"),
    ];

    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = Vec::new();
    for (id, label) in metric_defs {
        let is_checked = active_metrics.contains(&id.to_string());
        let item = CheckMenuItem::with_id(
            app,
            format!("hud-metric-{id}"),
            label,
            true,
            is_checked,
            None::<&str>,
        )
        .map_err(|e| e.to_string())?;
        items.push(Box::new(item));
    }

    let sep = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    items.push(Box::new(sep));

    let mode_text = match settings.hud_mode {
        crate::app_settings::HudMode::Widget => "切换为极简胶囊",
        crate::app_settings::HudMode::Capsule => "切换为卡片小组件",
    };
    let mode_item =
        tauri::menu::MenuItem::with_id(app, "hud-toggle-mode", mode_text, true, None::<&str>)
            .map_err(|e| e.to_string())?;
    items.push(Box::new(mode_item));

    let lock_text = if settings.hud_locked {
        "解锁 HUD"
    } else {
        "锁定 HUD (鼠标穿透)"
    };
    let lock_item =
        tauri::menu::MenuItem::with_id(app, "hud-toggle-lock", lock_text, true, None::<&str>)
            .map_err(|e| e.to_string())?;
    items.push(Box::new(lock_item));
    let item_refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|b| b.as_ref()).collect();
    let menu = Menu::with_items(app, &item_refs).map_err(|e| e.to_string())?;

    menu.popup(window.as_ref().window().clone())
        .map_err(|e| e.to_string())?;
    Ok(())
}
