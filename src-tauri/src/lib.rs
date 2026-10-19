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
pub mod core;
pub mod tray;
pub mod watch;

use std::sync::mpsc;
use std::sync::Mutex;

use tauri::{Manager, RunEvent, WindowEvent};

use app_lock::{AppLock, LockError};
use app_settings::AppSettings;
use watch::{DataHandle, DataPaths};

struct AppState {
    /// 进程生命周期内持有的写者锁（Drop 时释放）
    _lock: Mutex<Option<AppLock>>,
    data: Mutex<Option<DataHandle>>,
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
            let tray_tx = match tray::setup(
                app.handle(),
                &settings_path,
                &paths.db_path,
                settings.tray_metric,
            ) {
                Ok(tx) => tx,
                Err(e) => {
                    eprintln!("[token-watcher] 托盘初始化失败: {e}");
                    mpsc::channel().0
                }
            };
            let data = watch::spawn(app.handle().clone(), paths, tray_tx);
            app.manage(AppState {
                _lock: Mutex::new(Some(lock)),
                data: Mutex::new(Some(data)),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Tauri 2 没有 visibility 事件：用获得焦点作为「重新显示」信号。
            // 隐藏时的 emit 挂起由数据线程按 is_visible() 自行判断。
            if matches!(event, WindowEvent::Focused(true)) {
                let app_handle = window.app_handle();
                // T5：窗口可能被 single-instance 等菜单外路径显示 → 同步菜单文案
                tray::sync_window_item_label(app_handle);
                if let Some(state) = app_handle.try_state::<AppState>() {
                    if let Ok(guard) = state.data.lock() {
                        if let Some(data) = guard.as_ref() {
                            data.window_shown();
                        }
                    }
                }
            }
        })
        .build(tauri::generate_context!());

    match result {
        Ok(app) => app.run(|app_handle, event| {
            if matches!(event, RunEvent::Exit) {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    if let Ok(mut guard) = state.data.lock() {
                        if let Some(mut data) = guard.take() {
                            data.shutdown();
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
