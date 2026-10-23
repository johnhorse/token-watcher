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

/// 主题外观模式
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeMode {
    #[default]
    MeterFace,
    Slate,
    Mint,
    #[serde(other)]
    Unknown,
}

impl ThemeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MeterFace | Self::Unknown => "meter-face",
            Self::Slate => "slate",
            Self::Mint => "mint",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s {
            "slate" => Self::Slate,
            "mint" => Self::Mint,
            _ => Self::MeterFace,
        }
    }
}

/// HUD 形态模式：卡片小组件 vs 极简胶囊条
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HudMode {
    #[default]
    Widget,
    Capsule,
}

impl HudMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Widget => "widget",
            Self::Capsule => "capsule",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s {
            "capsule" => Self::Capsule,
            _ => Self::Widget,
        }
    }
}

pub fn default_capsule_opacity() -> f64 {
    0.9
}

pub fn default_widget_opacity() -> f64 {
    0.95
}

pub fn default_hud_active_sessions_count() -> usize {
    3
}

/// app 自身设置（未来字段一律给 serde 默认值，保证旧文件可读）
pub fn default_hud_metrics() -> Vec<String> {
    vec![
        "current_rate".to_string(),
        "session_total".to_string(),
        "cache_rate".to_string(),
    ]
}

pub fn default_tray_metrics() -> Vec<TrayMetric> {
    vec![TrayMetric::Total]
}
/// app 自身设置（未来字段一律给 serde 默认值，保证旧文件可读）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// 托盘显示指标（兼容旧字段单选）
    pub tray_metric: TrayMetric,
    /// 状态栏显示指标列表（多选 1-2 项，默认 [Total]）
    pub tray_metrics: Vec<TrayMetric>,
    /// 飞行面板选中的指标列表（默认 [当前速率, 会话总 Token, 缓存率]，最少 2 项）
    pub hud_metrics: Vec<String>,
    /// 桌面角落 HUD 是否开启（默认 false）
    pub hud_visible: bool,
    /// 桌面角落 HUD 是否锁定并穿透鼠标（默认 false）
    pub hud_locked: bool,
    /// 桌面角落 HUD 记忆 X 坐标（兼容旧版字段，逻辑像素，None 表示未保存或使用默认）
    pub hud_x: Option<i32>,
    /// 桌面角落 HUD 记忆 Y 坐标（兼容旧版字段，逻辑像素，None 表示未保存或使用默认）
    pub hud_y: Option<i32>,
    /// 桌面卡片小组件记忆 X 坐标（逻辑像素，None 表示未保存或使用默认）
    pub widget_x: Option<i32>,
    /// 桌面卡片小组件记忆 Y 坐标（逻辑像素，None 表示未保存或使用默认）
    pub widget_y: Option<i32>,
    /// 极简胶囊条记忆 X 坐标（逻辑像素，None 表示未保存或使用默认）
    pub capsule_x: Option<i32>,
    /// 极简胶囊条记忆 Y 坐标（逻辑像素，None 表示未保存或使用默认）
    pub capsule_y: Option<i32>,
    /// 主题模式（默认 MeterFace 经典工控仪表）
    pub theme: ThemeMode,
    /// 开机自启动设置（None 表示首次运行未初始化，默认行为视为 true）
    pub autostart: Option<bool>,
    /// HUD 形态（默认 Widget 卡片小组件）
    pub hud_mode: HudMode,
    /// 极简胶囊透明度（0.5 ~ 1.0，默认 0.9）
    #[serde(default = "default_capsule_opacity")]
    pub capsule_opacity: f64,
    /// 卡片小组件透明度（0.5 ~ 1.0，默认 0.95）
    #[serde(default = "default_widget_opacity")]
    pub widget_opacity: f64,
    /// HUD 激活会话最多展示条数（1/2/3/5，默认 3）
    #[serde(default = "default_hud_active_sessions_count")]
    pub hud_active_sessions_count: usize,
}
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            tray_metric: TrayMetric::Total,
            tray_metrics: default_tray_metrics(),
            hud_metrics: default_hud_metrics(),
            hud_visible: false,
            hud_locked: false,
            hud_x: None,
            hud_y: None,
            widget_x: None,
            widget_y: None,
            capsule_x: None,
            capsule_y: None,
            theme: ThemeMode::MeterFace,
            autostart: None,
            hud_mode: HudMode::Widget,
            capsule_opacity: default_capsule_opacity(),
            widget_opacity: default_widget_opacity(),
            hud_active_sessions_count: default_hud_active_sessions_count(),
        }
    }
}

impl AppSettings {
    /// 读取设置：文件不存在/不可读 → 默认值；内容损坏 → 默认值并重建文件。
    pub fn effective_tray_metrics(&self) -> Vec<TrayMetric> {
        if self.tray_metrics.is_empty() {
            vec![self.tray_metric]
        } else {
            self.tray_metrics.clone()
        }
    }

    pub fn effective_hud_metrics(&self) -> Vec<String> {
        if self.hud_metrics.len() < 2 {
            default_hud_metrics()
        } else {
            self.hud_metrics.clone()
        }
    }

    /// 是否启用开机自启：None 时遵循首次启动默认启用 (true)
    pub fn is_autostart_enabled(&self) -> bool {
        self.autostart.unwrap_or(true)
    }

    /// 获取指定形态对应的记忆坐标（向后兼容：小组件若未独立设置坐标，回退旧版单一 hud_x/hud_y；胶囊形态保持独立）
    pub fn get_position_for_mode(&self, mode: HudMode) -> (Option<i32>, Option<i32>) {
        match mode {
            HudMode::Widget => (self.widget_x.or(self.hud_x), self.widget_y.or(self.hud_y)),
            HudMode::Capsule => (self.capsule_x, self.capsule_y),
        }
    }
    /// 设置指定形态的记忆坐标，并同步更新 hud_x/hud_y 以保证向前兼容
    pub fn set_position_for_mode(&mut self, mode: HudMode, x: i32, y: i32) {
        match mode {
            HudMode::Widget => {
                self.widget_x = Some(x);
                self.widget_y = Some(y);
            }
            HudMode::Capsule => {
                self.capsule_x = Some(x);
                self.capsule_y = Some(y);
            }
        }
        self.hud_x = Some(x);
        self.hud_y = Some(y);
    }
    /// 读取设置：文件不存在/不可读 → 默认值；内容损坏 → 默认值并重建文件。
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(mut settings) => {
                    if settings.tray_metrics.is_empty() {
                        settings.tray_metrics = vec![settings.tray_metric];
                    }
                    if settings.hud_metrics.len() < 2 {
                        settings.hud_metrics = default_hud_metrics();
                    }
                    settings.capsule_opacity = settings.capsule_opacity.clamp(0.5, 1.0);
                    settings.widget_opacity = settings.widget_opacity.clamp(0.5, 1.0);
                    settings.hud_active_sessions_count =
                        settings.hud_active_sessions_count.clamp(1, 5);
                    settings
                }
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

/// 开机自启系统同步动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartAction {
    /// 触发启用自启
    Enable,
    /// 触发禁用自启
    Disable,
    /// 无需操作（状态已同步或查询失败保护）
    Noop,
}

/// 判定开机自启同步动作：仅在系统实际状态与期望设置不一致时触发变更，
/// 避免每次应用启动重复写入 LaunchAgent plist 引发 macOS 系统反复弹出“已添加后台项目”通知。
pub fn determine_autostart_action(
    desired: bool,
    is_enabled_res: Result<bool, String>,
) -> AutostartAction {
    match is_enabled_res {
        Ok(is_enabled) => {
            if desired && !is_enabled {
                AutostartAction::Enable
            } else if !desired && is_enabled {
                AutostartAction::Disable
            } else {
                AutostartAction::Noop
            }
        }
        Err(e) => {
            eprintln!("[token-watcher] 检查系统开机自启状态失败，维持现状: {e}");
            AutostartAction::Noop
        }
    }
}
