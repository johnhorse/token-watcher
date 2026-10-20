//! token-watcher：跨平台 AI token 实时监控桌面小组件
//!
//! 架构与决策见 docs/designs/token-watcher-design-v1.1.md（APPROVED）。
//!
//! T3/T4 app 编排：
//! - 单实例守卫（D15，插件）：第二实例唤起主窗后退出（插件必须最先注册）；
//! - 写者互斥（F5，文件锁）：GUI 是唯一 ingestion owner；tw-ingest 拿不到锁时只读；
//! - 后台数据线程：首扫（进度推送）→ fs 事件驱动增量 → 契约事件推送。
//!
//! T5 托盘：
//! - macOS 菜单栏 title 数字 / Windows 静态图标 + tooltip + 左键弹窗；
//! - 指标可配置（settings.json），数字与 UI 同源（agg::tray_metric）；
//! - 数据线程落库 → 内部 channel 通知托盘刷新（60s 兜底）。

pub mod app_lock;
pub mod app_settings;
pub mod commands;
pub mod core;
pub mod hud;
pub mod proxy;
pub mod tray;
pub mod watch;

use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{Emitter, Manager, RunEvent, WindowEvent};

use app_lock::{AppLock, LockError};
use app_settings::AppSettings;
use watch::{DataHandle, DataPaths, ScanPublicStatus};

pub struct AppState {
    /// 进程生命周期内持有的写者锁（Drop 时释放）
    _lock: Mutex<Option<AppLock>>,
    data: Mutex<Option<DataHandle>>,
    /// 引擎状态快照（get_scan_status 数据源，TauriSink 同步更新）
    pub status: Mutex<ScanPublicStatus>,
    /// command 查询用只读连接（lazy，首扫完成后才建立）
    pub store: Mutex<Option<core::store::Store>>,
    /// 只读连接的目标路径
    pub db_path: std::path::PathBuf,
    /// 本地反向代理状态
    pub proxy: Mutex<Option<std::sync::Arc<proxy::ProxyState>>>,
    /// 成本估算价格表管理器
    pub pricing: std::sync::Arc<core::pricing::PriceManager>,
    /// 自动配额同步器
    pub quota_syncer: std::sync::Arc<core::quota::QuotaSyncer>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = match DataPaths::default_paths() {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("[token-watcher] 无法定位数据目录: {e}");
            std::process::exit(1);
        }
    };
    let lock_path = match app_lock::default_lock_path() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("[token-watcher] 无法定位锁文件: {e}");
            std::process::exit(1);
        }
    };

    let result = tauri::Builder::default()
        // D15：必须第一个注册；第二实例在此唤起主窗并退出，不会走到 setup
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window(watch::WINDOW_LABEL) {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .setup(move |app| {
            // 写者锁（F5）：single-instance 已拦住同 app 的第二实例，
            // 这里拦的是 tw-ingest 等其他写进程。拿不到 → 提示并退出。
            let lock = match AppLock::acquire(&lock_path) {
                Ok(lock) => lock,
                Err(LockError::Busy) => {
                    show_owner_conflict(app_lock::read_owner_pid(&lock_path));
                    std::process::exit(0);
                }
                Err(LockError::Io(e)) => {
                    eprintln!("[token-watcher] 无法获取写者锁: {e}");
                    std::process::exit(1);
                }
            };
            // T5：托盘（复用 tauri.conf.json 创建的图标，挂菜单/事件/刷新线程）。
            // 托盘失败不阻塞数据管道：用断开通道降级，窗口仍可用。
            let settings_path = app_settings::path_for_db(&paths.db_path);
            let settings = AppSettings::load(&settings_path);
            let tray_tx = match tray::setup(app.handle(), &settings_path, &paths.db_path, &settings)
            {
                Ok(tx) => tx,
                Err(e) => {
                    eprintln!("[token-watcher] 托盘初始化失败: {e}");
                    mpsc::channel().0
                }
            };
            let db_path = paths.db_path.clone();
            let claude_settings_path = paths.settings_path.clone();
            let data = watch::spawn(app.handle().clone(), paths, tray_tx);
            let proxy_state = match tauri::async_runtime::block_on(proxy::start_proxy_server(
                data.sender(),
                claude_settings_path.clone(),
                proxy::DEFAULT_PORT,
            )) {
                Ok((st, _join)) => {
                    eprintln!(
                        "[token-watcher] 本地代理已在 127.0.0.1:{} 启动",
                        st.bound_port
                    );
                    Some(st)
                }
                Err(e) => {
                    eprintln!("[token-watcher] 本地代理启动失败: {e}");
                    None
                }
            };
            if settings.hud_visible {
                if let Err(e) = hud::ensure_hud_window(
                    app.handle(),
                    true,
                    settings.hud_locked,
                    settings.hud_x,
                    settings.hud_y,
                ) {
                    eprintln!("[token-watcher] HUD 窗口恢复失败: {e}");
                }
            }
            let data_dir = db_path.parent().unwrap_or(std::path::Path::new("."));
            let pricing = std::sync::Arc::new(core::pricing::PriceManager::new(data_dir));
            pricing.clone().spawn_background_refresher(Some(app.handle().clone()));
            let quota_syncer = std::sync::Arc::new(core::quota::QuotaSyncer::new(
                db_path.clone(),
                claude_settings_path.clone(),
            ));
            // 启动后台 QuotaSyncer 定时轮询（每 10 分钟轮询一次所有 API 档案，等待首扫完成）
            {
                let syncer = quota_syncer.clone();
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    // 等待首扫完成
                    loop {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        if let Some(state) = app_handle.try_state::<AppState>() {
                            if let Ok(guard) = state.status.lock() {
                                if guard.phase == "done" || guard.phase == "empty" {
                                    break;
                                }
                            }
                        }
                    }
                    // 首扫完成后首次拉取
                    syncer.sync_all_active_profiles().await;
                    let _ = app_handle.emit("dashboard-dirty", ());

                    // 定时 10 分钟轮询
                    loop {
                        tokio::time::sleep(Duration::from_secs(600)).await;
                        syncer.sync_all_active_profiles().await;
                        let _ = app_handle.emit("dashboard-dirty", ());
                    }
                });
            }
            app.manage(AppState {
                _lock: Mutex::new(Some(lock)),
                data: Mutex::new(Some(data)),
                status: Mutex::new(ScanPublicStatus::default()),
                store: Mutex::new(None),
                db_path,
                proxy: Mutex::new(proxy_state),
                pricing,
                quota_syncer,
            });
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::Focused(true) => {
                let app_handle = window.app_handle();
                tray::sync_window_item_label(app_handle);
                if let Some(state) = app_handle.try_state::<AppState>() {
                    if let Ok(guard) = state.data.lock() {
                        if let Some(data) = guard.as_ref() {
                            data.window_shown();
                        }
                    }
                }
            }
            WindowEvent::CloseRequested { api, .. } if window.label() == watch::WINDOW_LABEL => {
                api.prevent_close();
                let _ = window.hide();
                tray::sync_window_item_label(window.app_handle());
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_dashboard,
            commands::get_scan_status,
            commands::update_profile_display,
            commands::update_profile_quota,
            commands::reassign_usage,
            commands::get_proxy_status,
            commands::inject_proxy_settings,
            commands::restore_proxy_settings,
            commands::get_hud_summary,
            commands::set_hud_visible,
            commands::set_hud_locked,
            commands::save_hud_position,
            commands::show_main_window,
            commands::update_profile_quota_config,
            commands::sync_profile_quota
        ])
        .build(tauri::generate_context!());

    match result {
        Ok(app) => app.run(|app_handle, event| {
            if matches!(event, RunEvent::Exit) {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    if let Ok(guard) = state.proxy.lock() {
                        if let Some(proxy_st) = guard.as_ref() {
                            let _ = proxy::restore_settings_proxy(&proxy_st.claude_settings_path);
                        }
                    }
                    if let Ok(mut guard) = state.data.lock() {
                        if let Some(mut data) = guard.take() {
                            data.detach();
                        }
                    }
                    if let Ok(mut guard) = state._lock.lock() {
                        guard.take();
                    }
                }
            }
        }),
        Err(e) => {
            eprintln!("[token-watcher] 启动失败: {e}");
            std::process::exit(1);
        }
    }
}

/// 写者锁冲突提示（正常双开会被 single-instance 插件拦在前面，走不到这里）。
fn show_owner_conflict(pid: Option<u32>) {
    let message = match pid {
        Some(pid) => format!("token-watcher 已有另一个实例在运行（pid {pid}），本次启动将退出。"),
        None => "token-watcher 已有另一个实例在运行（写者锁被占用），本次启动将退出。".to_string(),
    };
    eprintln!("[token-watcher] {message}");

    #[cfg(target_os = "macos")]
    {
        let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display alert \"token-watcher\" message \"{escaped}\" as critical");
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .status();
    }

    #[cfg(target_os = "windows")]
    {
        let escaped = message.replace('\'', "''");
        let script = format!(
            "Add-Type -AssemblyName PresentationFramework; \
             [System.Windows.MessageBox]::Show('{escaped}', 'token-watcher')"
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status();
    }
}
