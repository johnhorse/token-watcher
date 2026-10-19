//! Phase 3：契约 command 接线（docs/contracts/ui-data-contract.md）。
//!
//! 查询走只读连接（不与数据线程的迁移/写入竞态）；写操作（档案编辑/
//! 手动修正）在首扫完成后才放行，低频、SQLite 层串行化，写完发
//! `dashboard-dirty` 让 UI 防抖重拉。

use tauri::{AppHandle, Emitter, State};

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

fn empty_dashboard(range: Range) -> Dashboard {
    Dashboard {
        range,
        totals: TokenTotals::default(),
        pulses: Vec::new(),
        models: Vec::new(),
        profiles: Vec::new(),
        sessions: Vec::new(),
    }
}

#[tauri::command]
pub fn get_dashboard(state: State<'_, AppState>, range: String) -> Result<Dashboard, String> {
    let range = parse_range(&range)?;
    let snap = state.status.lock().map_err(|e| e.to_string())?.clone();
    // 首扫未完成：empty 返回空数据（UI 空态），scanning 返回空数据
    // （UI 状态机本就停在骨架，这里兜底防 fetch 失败横幅误报）
    if snap.phase != "done" && snap.phase != "empty" {
        return Ok(empty_dashboard(range));
    }
    let mut guard = state.store.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        let store = Store::open_readonly(&state.db_path).map_err(|e| e.to_string())?;
        *guard = Some(store);
    }
    let store = guard.as_ref().expect("上面刚放入");
    agg::dashboard(store, range).map_err(|e| e.to_string())
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
