//! token-watcher：跨平台 AI token 实时监控桌面小组件
//!
//! 架构与决策见 docs/designs/token-watcher-design-v1.1.md（APPROVED）。

pub mod core;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
