//! 桌面角落 HUD 模式（Desktop Corner HUD）窗口管理与穿透控制
//!
//! 规约：docs/designs/v0.2-hud-mode-spec.md

use tauri::{AppHandle, Manager, Position, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const HUD_LABEL: &str = "hud";
pub const HUD_WIDTH: f64 = 184.0;
pub const HUD_HEIGHT: f64 = 34.0;

/// 计算 HUD 窗口安全坐标（P1-2 多显示器拔插与越界防护）
pub fn calculate_safe_position(
    app: &AppHandle,
    saved_x: Option<i32>,
    saved_y: Option<i32>,
) -> (i32, i32) {
    let monitors = app.available_monitors().unwrap_or_default();

    if let (Some(x), Some(y)) = (saved_x, saved_y) {
        // 校验目标矩形是否与任一可用屏幕有至少 32px × 32px 的可见交集
        let hud_w = HUD_WIDTH as i32;
        let hud_h = HUD_HEIGHT as i32;

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
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(HUD_LABEL) {
        if visible {
            let (target_x, target_y) = calculate_safe_position(app, x, y);
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
                .inner_size(HUD_WIDTH, HUD_HEIGHT)
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
