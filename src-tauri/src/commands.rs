//! Phase 3：契约 command 接线（docs/contracts/ui-data-contract.md）。
//!
//! 查询走只读连接（不与数据线程的迁移/写入竞态）；写操作（档案编辑/
//! 手动修正）在首扫完成后才放行，低频、SQLite 层串行化，写完发
//! `dashboard-dirty` 让 UI 防抖重拉。

use tauri::{AppHandle, Emitter, Manager, State};

use crate::core::agg::{self, Dashboard, Range, TokenTotals};
use crate::core::store::Store;
use crate::AppState;

/// `get_scan_status` 响应：进度 + data-health 快照（契约合并语义）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ScanStatusResponse {
    pub phase: String,
    pub files_done: u64,
    pub files_total: u64,
    pub percent: f64,
    pub root_unreadable: bool,
    pub skipped_files: u64,
    pub db_readonly: bool,
}

fn parse_range(range: &str) -> Result<Range, String> {
    match range {
        "today" => Ok(Range::Today),
        "week" => Ok(Range::Week),
        "month" => Ok(Range::Month),
        "all" => Ok(Range::All),
        other => Err(format!("未知 range: {other}")),
    }
}

fn empty_dashboard(range: Range, engine: Option<String>) -> Dashboard {
    Dashboard {
        range,
        engine,
        totals: TokenTotals::default(),
        cost: crate::core::pricing::TokenCost::default(),
        pulses: Vec::new(),
        models: Vec::new(),
        profiles: Vec::new(),
        sessions: Vec::new(),
        available_engines: Vec::new(),
    }
}

#[tauri::command]
pub fn get_dashboard(
    state: State<'_, AppState>,
    range: String,
    engine: Option<String>,
) -> Result<Dashboard, String> {
    let range = parse_range(&range)?;
    let snap = state.status.lock().map_err(|e| e.to_string())?.clone();
    // 首扫未完成：empty 返回空数据（UI 空态），scanning 返回空数据
    // （UI 状态机本就停在骨架，这里兜底防 fetch 失败横幅误报）
    // 仅在明确处于首扫且有剩余文件未完成时兜底返回空数据
    if snap.phase == "scanning" && snap.files_total > 0 && snap.files_done < snap.files_total {
        return Ok(empty_dashboard(range, engine));
    }
    let mut guard = state.store.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        let store = Store::open_readonly(&state.db_path).map_err(|e| e.to_string())?;
        *guard = Some(store);
    }
    let store = guard.as_ref().expect("上面刚放入");
    let price_table = state.pricing.table();
    let price_guard = price_table.read().unwrap();
    agg::dashboard_with_prices_and_engine(store, range, Some(&price_guard), engine.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_session_flight_recorder(
    state: State<'_, AppState>,
    session_id: String,
    limit: Option<usize>,
) -> Result<Option<agg::SessionFlightRecorder>, String> {
    let snap = state.status.lock().map_err(|e| e.to_string())?.clone();
    if snap.phase == "scanning" && snap.files_total > 0 && snap.files_done < snap.files_total {
        return Ok(None);
    }
    let mut guard = state.store.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        let store = Store::open_readonly(&state.db_path).map_err(|e| e.to_string())?;
        *guard = Some(store);
    }
    let store = guard.as_ref().expect("上面刚放入");
    let price_table = state.pricing.table();
    let price_guard = price_table.read().unwrap();
    let now = agg::now_ms();
    agg::session_flight_recorder(store, Some(&price_guard), &session_id, limit, now)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn get_scan_status(state: State<'_, AppState>) -> ScanStatusResponse {
    let snap = state.status.lock().map(|s| s.clone()).unwrap_or_default();
    ScanStatusResponse {
        phase: snap.phase,
        files_done: snap.files_done,
        files_total: snap.files_total,
        percent: snap.percent,
        root_unreadable: snap.health.root_unreadable,
        skipped_files: snap.health.skipped_files,
        db_readonly: snap.health.db_readonly,
    }
}

/// 首扫完成后开写连接执行低频写操作（档案编辑/手动修正）。
/// 首扫期间的迁移由数据线程独占完成，这里等它结束再开库。
fn with_write_store<T>(
    state: &State<'_, AppState>,
    f: impl FnOnce(&mut Store) -> Result<T, String>,
) -> Result<T, String> {
    let snap = state.status.lock().map_err(|e| e.to_string())?.clone();
    if snap.phase != "done" && snap.phase != "empty" {
        return Err("首扫尚未完成".into());
    }
    let mut store = Store::open(&state.db_path).map_err(|e| e.to_string())?;
    f(&mut store)
}

fn emit_dirty(app: &AppHandle) {
    // 写操作改变聚合结果：通知 UI 防抖重拉（dashboard-dirty 无 payload）
    let _ = app.emit("dashboard-dirty", ());
}

#[tauri::command]
pub fn update_profile_display(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_id: i64,
    display_name: String,
) -> Result<(), String> {
    with_write_store(&state, |store| {
        store
            .update_profile_display(profile_id, &display_name)
            .map_err(|e| e.to_string())
    })?;
    emit_dirty(&app);
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn update_profile_quota(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_id: i64,
    total: Option<i64>,
    used: Option<i64>,
    reset_date: Option<String>,
) -> Result<(), String> {
    with_write_store(&state, |store| {
        store
            .update_profile_quota(profile_id, total, used, reset_date.as_deref())
            .map_err(|e| e.to_string())
    })?;
    emit_dirty(&app);
    Ok(())
}
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn update_profile_quota_config(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_id: i64,
    quota_type: String,
    unit: String,
    api_url: Option<String>,
    api_key: Option<String>,
    manual_total: Option<f64>,
    manual_used: Option<f64>,
    reset_date: Option<String>,
) -> Result<(), String> {
    with_write_store(&state, |store| {
        store
            .update_profile_quota_config(
                profile_id,
                &quota_type,
                &unit,
                api_url.as_deref(),
                api_key.as_deref(),
                manual_total,
                manual_used,
                reset_date.as_deref(),
            )
            .map_err(|e| e.to_string())
    })?;
    emit_dirty(&app);
    Ok(())
}

#[tauri::command]
pub async fn sync_profile_quota(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_id: i64,
) -> Result<agg::QuotaInfo, String> {
    let syncer = state.quota_syncer.clone();
    let result = syncer.sync_profile(profile_id).await;
    emit_dirty(&app);
    result
}

#[tauri::command]
pub fn reassign_usage(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_id: Option<i64>,
    start_ms: i64,
    end_ms: i64,
) -> Result<(), String> {
    with_write_store(&state, |store| {
        store
            .reassign_profile_range(profile_id, start_ms, end_ms)
            .map_err(|e| e.to_string())?;
        Ok(())
    })?;
    emit_dirty(&app);
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProxyStatusResponse {
    pub running: bool,
    pub port: u16,
    pub upstream: String,
    pub is_injected: bool,
    pub export_cmd: String,
}

#[tauri::command]
pub fn get_proxy_status(state: State<'_, AppState>) -> ProxyStatusResponse {
    let guard = state.proxy.lock().ok();
    if let Some(proxy_state) = guard.as_ref().and_then(|g| g.as_ref()) {
        let is_injected = crate::proxy::get_injected_marker_path()
            .map(|p| p.exists())
            .unwrap_or(false);
        let upstream = proxy_state.resolve_upstream(None);
        ProxyStatusResponse {
            running: true,
            port: proxy_state.bound_port,
            upstream,
            is_injected,
            export_cmd: format!(
                "export ANTHROPIC_BASE_URL=http://127.0.0.1:{}",
                proxy_state.bound_port
            ),
        }
    } else {
        ProxyStatusResponse {
            running: false,
            port: crate::proxy::DEFAULT_PORT,
            upstream: "https://api.anthropic.com".into(),
            is_injected: false,
            export_cmd: format!(
                "export ANTHROPIC_BASE_URL=http://127.0.0.1:{}",
                crate::proxy::DEFAULT_PORT
            ),
        }
    }
}

#[tauri::command]
pub fn inject_proxy_settings(state: State<'_, AppState>) -> Result<ProxyStatusResponse, String> {
    let guard = state.proxy.lock().map_err(|e| e.to_string())?;
    let proxy_state = guard
        .as_ref()
        .ok_or_else(|| "代理服务尚未运行".to_string())?;
    crate::proxy::inject_settings_proxy(proxy_state.bound_port, &proxy_state.claude_settings_path)?;
    let upstream = proxy_state.resolve_upstream(None);
    Ok(ProxyStatusResponse {
        running: true,
        port: proxy_state.bound_port,
        upstream,
        is_injected: true,
        export_cmd: format!(
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:{}",
            proxy_state.bound_port
        ),
    })
}

#[tauri::command]
pub fn restore_proxy_settings(state: State<'_, AppState>) -> Result<ProxyStatusResponse, String> {
    let guard = state.proxy.lock().map_err(|e| e.to_string())?;
    let proxy_state = guard
        .as_ref()
        .ok_or_else(|| "代理服务尚未运行".to_string())?;
    crate::proxy::restore_settings_proxy(&proxy_state.claude_settings_path)?;
    let upstream = proxy_state.resolve_upstream(None);
    Ok(ProxyStatusResponse {
        running: true,
        port: proxy_state.bound_port,
        upstream,
        is_injected: false,
        export_cmd: format!(
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:{}",
            proxy_state.bound_port
        ),
    })
}

#[tauri::command]
pub fn get_hud_summary(state: State<'_, AppState>) -> Result<crate::core::agg::HudSummary, String> {
    let settings_path = crate::app_settings::path_for_db(&state.db_path);
    let settings = crate::app_settings::AppSettings::load(&settings_path);
    let mut guard = state.store.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        let store = Store::open_readonly(&state.db_path).map_err(|e| e.to_string())?;
        *guard = Some(store);
    }
    let store = guard.as_ref().expect("上面刚放入");
    crate::core::agg::hud_summary(
        store,
        settings.hud_locked,
        &settings.effective_hud_metrics(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_hud_visible(
    app: AppHandle,
    state: State<'_, AppState>,
    visible: bool,
) -> Result<(), String> {
    let settings_path = crate::app_settings::path_for_db(&state.db_path);
    let mut settings = crate::app_settings::AppSettings::load(&settings_path);
    settings.hud_visible = visible;
    settings.save(&settings_path).map_err(|e| e.to_string())?;
    crate::hud::ensure_hud_window(
        &app,
        visible,
        settings.hud_locked,
        settings.hud_x,
        settings.hud_y,
    )?;
    crate::tray::sync_hud_menu_items(&app, &settings);
    Ok(())
}

#[tauri::command]
pub fn set_hud_locked(
    app: AppHandle,
    state: State<'_, AppState>,
    locked: bool,
) -> Result<(), String> {
    let settings_path = crate::app_settings::path_for_db(&state.db_path);
    let mut settings = crate::app_settings::AppSettings::load(&settings_path);
    settings.hud_locked = locked;
    settings.save(&settings_path).map_err(|e| e.to_string())?;
    if let Some(win) = app.get_webview_window(crate::hud::HUD_LABEL) {
        crate::hud::apply_hud_locked(&win, locked)?;
    }
    crate::tray::sync_hud_menu_items(&app, &settings);
    Ok(())
}

#[tauri::command]
pub fn save_hud_position(state: State<'_, AppState>, x: i32, y: i32) -> Result<(), String> {
    let settings_path = crate::app_settings::path_for_db(&state.db_path);
    let mut settings = crate::app_settings::AppSettings::load(&settings_path);
    settings.hud_x = Some(x);
    settings.hud_y = Some(y);
    settings.save(&settings_path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn move_hud_by(app: AppHandle, dx: f64, dy: f64) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(crate::hud::HUD_LABEL) {
        if let Ok(pos) = window.outer_position() {
            if let Ok(scale) = window.scale_factor() {
                let new_x = pos.x + (dx * scale).round() as i32;
                let new_y = pos.y + (dy * scale).round() as i32;
                let _ = window.set_position(tauri::Position::Physical(
                    tauri::PhysicalPosition::new(new_x, new_y),
                ));
            }
        }
    }
    Ok(())
}
#[tauri::command]
pub fn resize_hud(app: AppHandle, width: f64) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(crate::hud::HUD_LABEL) {
        let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
            width,
            crate::hud::HUD_HEIGHT,
        )));
    }
    Ok(())
}

#[tauri::command]
pub fn show_hud_context_menu(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let settings_path = crate::app_settings::path_for_db(&state.db_path);
    crate::hud::show_hud_context_menu(&app, &settings_path)
}

#[tauri::command]
pub fn toggle_hud_metric(
    app: AppHandle,
    state: State<'_, AppState>,
    metric: String,
) -> Result<Vec<String>, String> {
    let settings_path = crate::app_settings::path_for_db(&state.db_path);
    let mut settings = crate::app_settings::AppSettings::load(&settings_path);
    let mut current = settings.effective_hud_metrics();
    if current.contains(&metric) {
        if current.len() > 2 {
            current.retain(|m| m != &metric);
        }
    } else {
        current.push(metric);
    }
    settings.hud_metrics = current.clone();
    settings.save(&settings_path).map_err(|e| e.to_string())?;
    let _ = app.emit("hud-updated", ());
    Ok(current)
}

#[tauri::command]
pub fn save_current_hud_position(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(crate::hud::HUD_LABEL) {
        if let Ok(pos) = window.outer_position() {
            if let Ok(scale) = window.scale_factor() {
                let logical_x = (pos.x as f64 / scale).round() as i32;
                let logical_y = (pos.y as f64 / scale).round() as i32;
                let settings_path = crate::app_settings::path_for_db(&state.db_path);
                let mut settings = crate::app_settings::AppSettings::load(&settings_path);
                settings.hud_x = Some(logical_x);
                settings.hud_y = Some(logical_y);
                settings.save(&settings_path).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn show_main_window(app: AppHandle) -> Result<(), String> {
    crate::hud::show_main_window(&app)
}
#[tauri::command]
pub fn log_js_error(msg: String) {
    eprintln!("[WEBVIEW_JS_ERROR] {}", msg);
}

#[tauri::command]
pub fn get_theme(state: State<'_, AppState>) -> Result<String, String> {
    let settings_path = crate::app_settings::path_for_db(&state.db_path);
    let settings = crate::app_settings::AppSettings::load(&settings_path);
    Ok(settings.theme.as_str().to_string())
}

#[tauri::command]
pub fn set_theme(app: AppHandle, state: State<'_, AppState>, theme: String) -> Result<(), String> {
    let settings_path = crate::app_settings::path_for_db(&state.db_path);
    let mut settings = crate::app_settings::AppSettings::load(&settings_path);
    let mode = crate::app_settings::ThemeMode::from_str_loose(&theme);
    settings.theme = mode;
    settings.save(&settings_path).map_err(|e| e.to_string())?;
    let _ = app.emit("theme-changed", mode.as_str());
    Ok(())
}
