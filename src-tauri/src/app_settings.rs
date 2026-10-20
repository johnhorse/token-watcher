//! T5 app 自身设置（与 Claude 的 settings.json 无关）
//!
//! 位置：`~/.token-watcher/settings.json`（由调用方从 db_path 所在目录推导）。
//! 结构可扩展；损坏/未知值时回退默认并尽力重建，绝不阻塞启动。

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::agg::TrayMetric;

/// 磁盘上的设置文件名
pub const FILE_NAME: &str = "settings.json";

/// app 自身设置（未来字段一律给 serde 默认值，保证旧文件可读）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// 托盘显示指标（四选一，默认总 Token）
    pub tray_metric: TrayMetric,
    /// 桌面角落 HUD 是否开启（默认 false）
    pub hud_visible: bool,
    /// 桌面角落 HUD 是否锁定并穿透鼠标（默认 false）
    pub hud_locked: bool,
    /// 桌面角落 HUD 记忆 X 坐标（逻辑像素，None 表示未保存或使用默认）
    pub hud_x: Option<i32>,
    /// 桌面角落 HUD 记忆 Y 坐标（逻辑像素，None 表示未保存或使用默认）
    pub hud_y: Option<i32>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            tray_metric: TrayMetric::Total,
            hud_visible: false,
            hud_locked: false,
            hud_x: None,
            hud_y: None,
        }
    }
}

impl AppSettings {
    /// 读取设置：文件不存在/不可读 → 默认值；内容损坏 → 默认值并重建文件。
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(settings) => settings,
                Err(e) => {
                    eprintln!("[token-watcher] 设置文件损坏，回退默认并重建: {e}");
                    let settings = Self::default();
                    if let Err(e) = settings.save(path) {
                        eprintln!("[token-watcher] 重建设置文件失败: {e}");
                    }
                    settings
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// 原子写（tmp + rename）：避免半写文件被下一次启动读到
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)?;
            }
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }
}

/// 从数据库路径推导设置文件路径（同一数据目录）
pub fn path_for_db(db_path: &Path) -> std::path::PathBuf {
    db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join(FILE_NAME)
}
