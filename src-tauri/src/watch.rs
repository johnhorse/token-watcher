//! T3/T4：app 层数据编排（watcher + 首扫 + 增量 ingest + 契约事件）
//!
//! ## 启动序（D21）
//!
//! [`DataEngine::new`] 先注册 fs watcher，再由 [`DataEngine::initial_scan`] 枚举首扫。
//! 注册与枚举之间到达的事件不会丢：事件路径与枚举路径最终都走同一套
//! checkpoint 续读（[`ingest`]），重叠窗口内同一文件可能被看两次，但两次都从
//! byte checkpoint 之后读取、uuid UPSERT 去重（D22），结果幂等一致。
//!
//! ## 线程模型
//!
//! 单个数据线程独占 `Store`（rusqlite `Connection` 非 Sync）；notify 回调只往
//! channel 投递事件。窗口可见性只在「发事件前」查询 `is_visible()`——隐藏时挂起
//! emit（置 pending），watcher 继续 ingest 保持数据新鲜；重新显示时发一次
//! `dashboard-dirty` 触发 UI 全量水合（契约水合语义）。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::core::attr::{self, ApplyOutcome, SettingsSnapshot};
use crate::core::ingest::{self, ScanStats};
use crate::core::store::Store;
use crate::core::CoreError;
use crate::tray::TrayCommand;

/// 增量事件聚合窗口（OQ4：与 5 分钟宽限/激活阈值一并按体感调）
pub const DEBOUNCE: Duration = Duration::from_millis(500);
/// DB 降级后的重试间隔（仅故障态生效，正常路径无轮询）
const DB_RETRY: Duration = Duration::from_secs(10);
/// 首扫进度事件最小间隔（文件多时避免 IPC 洪泛）
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

pub const WINDOW_LABEL: &str = "main";

/// 数据源/库路径（测试可注入临时目录）
#[derive(Debug, Clone)]
pub struct DataPaths {
    pub projects_root: PathBuf,
    pub settings_path: PathBuf,
    pub claude_dir: PathBuf,
    pub codex_sessions_root: PathBuf,
    pub codex_config_path: PathBuf,
    pub codex_dir: PathBuf,
    pub cline_tasks_root: PathBuf,
    pub roo_tasks_root: PathBuf,
    pub aider_analytics_path: PathBuf,
    pub opencode_db_path: PathBuf,
    pub continue_sessions_root: PathBuf,
    pub omp_sessions_root: PathBuf,
    pub antigravity_conversations_root: PathBuf,
    pub zcode_root: PathBuf,
    pub qoder_root: PathBuf,
    pub workbuddy_root: PathBuf,
    pub db_path: PathBuf,
}

impl DataPaths {
    pub fn default_paths() -> Result<Self, CoreError> {
        let home =
            home::home_dir().ok_or_else(|| CoreError::Other("无法定位用户 home 目录".into()))?;
        let claude_dir = home.join(".claude");
        let codex_dir = home.join(".codex");

        #[cfg(target_os = "macos")]
        let global_storage = home.join("Library/Application Support/Code/User/globalStorage");
        #[cfg(target_os = "windows")]
        let global_storage = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Code/User/globalStorage");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let global_storage = home.join(".config/Code/User/globalStorage");

        let cline_tasks_root = global_storage.join("saoudrizwan.claude-dev").join("tasks");
        let roo_tasks_root = global_storage
            .join("rooveterinaryinc.roo-cline")
            .join("tasks");

        let aider_analytics_path = if home.join(".aider/analytics.jsonl").exists() {
            home.join(".aider/analytics.jsonl")
        } else {
            home.join(".aider.analytics.jsonl")
        };

        #[cfg(target_os = "macos")]
        let opencode_db_path = home.join(".local/share/opencode/opencode.db");
        #[cfg(target_os = "windows")]
        let opencode_db_path = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("opencode/opencode.db");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let opencode_db_path = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("opencode/opencode.db");

        let continue_sessions_root = home.join(".continue/sessions");
        let omp_sessions_root = home.join(".omp/agent/sessions");

        Ok(Self {
            projects_root: claude_dir.join("projects"),
            settings_path: claude_dir.join("settings.json"),
            claude_dir,
            codex_sessions_root: codex_dir.join("sessions"),
            codex_config_path: codex_dir.join("config.toml"),
            codex_dir,
            cline_tasks_root,
            roo_tasks_root,
            aider_analytics_path,
            opencode_db_path,
            continue_sessions_root,
            omp_sessions_root,
            antigravity_conversations_root: home.join(".gemini/antigravity/conversations"),
            zcode_root: home.join(".zcode"),
            qoder_root: home.join(".qoder"),
            workbuddy_root: home.join(".codebuddy"),
            db_path: home.join(".token-watcher").join("db.sqlite3"),
        })
    }

    pub fn for_test(temp: &Path) -> Self {
        let claude_dir = temp.join("claude");
        let codex_dir = temp.join("codex");
        Self {
            projects_root: claude_dir.join("projects"),
            settings_path: claude_dir.join("settings.json"),
            claude_dir,
            codex_sessions_root: codex_dir.join("sessions"),
            codex_config_path: codex_dir.join("config.toml"),
            codex_dir,
            cline_tasks_root: temp.join("cline_tasks"),
            roo_tasks_root: temp.join("roo_tasks"),
            aider_analytics_path: temp.join("aider.jsonl"),
            opencode_db_path: temp.join("opencode.db"),
            continue_sessions_root: temp.join("continue_sessions"),
            omp_sessions_root: temp.join("omp_sessions"),
            antigravity_conversations_root: temp.join("antigravity_conversations"),
            zcode_root: temp.join("zcode"),
            qoder_root: temp.join("qoder"),
            workbuddy_root: temp.join("workbuddy"),
            db_path: temp.join("db.sqlite3"),
        }
    }
}

/// 数据线程消息：fs 事件、窗口显示、退出
pub enum Message {
    Fs(Event),
    FsError(String),
    WindowShown,
    ProxyAttribution {
        message_id: String,
        profile_id: Option<i64>,
        endpoint: String,
        fingerprint: Option<String>,
    },
    Shutdown,
}

/// 契约事件出口（测试用 mock 替换 Tauri 实现）
pub trait EventSink: Send + 'static {
    /// 主窗口是否可见（隐藏时挂起 emit）
    fn is_visible(&self) -> bool;
    fn emit_json(&self, event: &str, payload: Value);
    /// T5：数据落库后的内部通知（驱动托盘刷新）。
    /// 默认 no-op：测试 MockSink 无需关心托盘。
    fn data_changed(&self) {}
    /// 引擎扫描状态变更内部通知（同步 AppState 中的 status 快照，无视窗口可见性）。
    fn status_changed(&self, _phase: &str, _done: u64, _total: u64, _percent: f64) {}
    /// 窗口重新显示时触发状态回放补发。
    fn window_shown(&self) {}
}

struct TauriSink {
    app: AppHandle,
    /// T5：托盘刷新内部通道（与 UI emit 并行，窗口隐藏也照常通知）
    tray_tx: Sender<TrayCommand>,
}

impl EventSink for TauriSink {
    fn is_visible(&self) -> bool {
        let main_win = self.app.get_webview_window(WINDOW_LABEL);
        let hud_win = self.app.get_webview_window(crate::hud::HUD_LABEL);
        if main_win.is_none() && hud_win.is_none() {
            return true;
        }
        let main_visible = main_win.and_then(|w| w.is_visible().ok()).unwrap_or(false);
        let hud_visible = hud_win.and_then(|w| w.is_visible().ok()).unwrap_or(false);
        main_visible || hud_visible
    }

    fn emit_json(&self, event: &str, payload: Value) {
        let _ = self.app.emit(event, &payload);
        // Phase 3：同步状态快照（get_scan_status 的数据源）
        if matches!(event, "scan-progress" | "data-health") {
            if let Some(state) = self.app.try_state::<crate::AppState>() {
                if let Ok(mut snap) = state.status.lock() {
                    match event {
                        "scan-progress" => {
                            snap.phase = payload
                                .get("phase")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            snap.files_done = payload
                                .get("files_done")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            snap.files_total = payload
                                .get("files_total")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            snap.percent = payload
                                .get("percent")
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0);
                        }
                        _ => {
                            snap.health.root_unreadable = payload
                                .get("root_unreadable")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            snap.health.skipped_files = payload
                                .get("skipped_files")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            snap.health.db_readonly = payload
                                .get("db_readonly")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                        }
                    }
                }
            }
        }
    }
    fn status_changed(&self, phase: &str, done: u64, total: u64, percent: f64) {
        if let Some(state) = self.app.try_state::<crate::AppState>() {
            if let Ok(mut snap) = state.status.lock() {
                snap.phase = phase.to_string();
                snap.files_done = done;
                snap.files_total = total;
                snap.percent = percent;
            }
        }
    }

    fn window_shown(&self) {
        let snap_opt = if let Some(state) = self.app.try_state::<crate::AppState>() {
            state
                .status
                .lock()
                .ok()
                .map(|s| (s.phase.clone(), s.files_done, s.files_total))
        } else {
            None
        };
        if let Some((phase, done, total)) = snap_opt {
            if !phase.is_empty() && self.is_visible() {
                let _ = self
                    .app
                    .emit("scan-progress", &progress_json(&phase, done, total));
            }
        }
    }

    fn data_changed(&self) {
        let _ = self.tray_tx.send(TrayCommand::DataChanged);
    }
}

/// 引擎公开状态快照（get_scan_status 数据源）。
/// 由 TauriSink 在发 scan-progress / data-health 事件时同步更新，
/// 兜住「事件早于前端监听就绪」的窗口。
#[derive(Debug, Clone, Default)]
pub struct ScanPublicStatus {
    /// "" = 启动前；scanning / done / empty（与 scan-progress 事件一致）
    pub phase: String,
    pub files_done: u64,
    pub files_total: u64,
    pub percent: f64,
    pub health: DataHealth,
}

/// D17 三档错误态（仅状态变化时发）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DataHealth {
    pub root_unreadable: bool,
    pub skipped_files: u64,
    pub db_readonly: bool,
}

impl DataHealth {
    fn to_json(self) -> Value {
        json!({
            "root_unreadable": self.root_unreadable,
            "skipped_files": self.skipped_files,
            "db_readonly": self.db_readonly,
        })
    }
}

fn progress_json(phase: &str, done: u64, total: u64) -> Value {
    let percent = if total == 0 {
        0.0
    } else {
        (done as f64 * 1000.0 / total as f64).round() / 10.0
    };
    json!({
        "phase": phase,
        "files_done": done,
        "files_total": total,
        "percent": percent,
    })
}

fn pulse_json(record: &crate::core::parse::UsageRecord) -> Value {
    json!({
        "t_ms": record.timestamp_ms,
        "session_id": record.session_id,
        "input": record.input_tokens,
        "output": record.output_tokens,
        "cache_read": record.cache_read_tokens,
        "cache_creation": record.cache_creation_tokens,
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// 数据编排引擎：独占 Store + watcher + 去抖状态
pub struct DataEngine<S: EventSink> {
    paths: DataPaths,
    projects_root_canonical: Option<PathBuf>,
    claude_dir_canonical: Option<PathBuf>,
    codex_sessions_root_canonical: Option<PathBuf>,
    codex_dir_canonical: Option<PathBuf>,
    cline_tasks_root_canonical: Option<PathBuf>,
    roo_tasks_root_canonical: Option<PathBuf>,
    omp_sessions_root_canonical: Option<PathBuf>,
    continue_sessions_root_canonical: Option<PathBuf>,
    sink: S,
    /// 由 notify 回调持有；Drop 即停止监听
    watcher: Option<RecommendedWatcher>,
    projects_watched: bool,
    codex_sessions_watched: bool,
    cline_tasks_watched: bool,
    roo_tasks_watched: bool,
    aider_watched: bool,
    continue_sessions_watched: bool,
    omp_sessions_watched: bool,
    opencode_watched: bool,
    store: Option<Store>,
    /// 去抖批次中待 ingest 的 JSONL 文件
    pending_files: HashSet<PathBuf>,
    settings_dirty: bool,
    /// 上次成功应用/确认一致的 settings 快照（坏 JSON 不入此字段）
    last_snapshot: Option<SettingsSnapshot>,
    health: DataHealth,
    last_health_sent: Option<DataHealth>,
    /// 窗口隐藏期间发生的变化：重新可见时补发 dashboard-dirty
    pending_dirty: bool,
    /// 窗口隐藏期间错过的 data-health（只保留最新态）
    pending_health: Option<DataHealth>,
    debounce_deadline: Option<Instant>,
    db_retry_at: Option<Instant>,
    /// 监听根被创建/恢复后需要补一次静默全扫
    needs_catchup: bool,
}

impl<S: EventSink> DataEngine<S> {
    /// 注册 watcher（D21 第一步）并打开数据库。失败不致命：
    /// watcher 注册失败只影响实时性，DB 失败进入只读降级并在 [`Self::on_tick`] 重试。
    pub fn new(paths: DataPaths, sink: S, tx: Sender<Message>) -> Self {
        let projects_root_canonical = fs::canonicalize(&paths.projects_root).ok();
        let claude_dir_canonical = fs::canonicalize(&paths.claude_dir).ok();
        let codex_sessions_root_canonical = fs::canonicalize(&paths.codex_sessions_root).ok();
        let codex_dir_canonical = fs::canonicalize(&paths.codex_dir).ok();
        let cline_tasks_root_canonical = fs::canonicalize(&paths.cline_tasks_root).ok();
        let roo_tasks_root_canonical = fs::canonicalize(&paths.roo_tasks_root).ok();
        let omp_sessions_root_canonical = fs::canonicalize(&paths.omp_sessions_root).ok();
        let continue_sessions_root_canonical = fs::canonicalize(&paths.continue_sessions_root).ok();

        let mut watcher =
            match notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
                let msg = match res {
                    Ok(event) => Message::Fs(event),
                    Err(e) => Message::FsError(e.to_string()),
                };
                let _ = tx.send(msg);
            }) {
                Ok(w) => Some(w),
                Err(e) => {
                    eprintln!("[token-watcher] watcher 初始化失败: {e}");
                    None
                }
            };

        let mut projects_watched = false;
        let mut codex_sessions_watched = false;
        let mut cline_tasks_watched = false;
        let mut roo_tasks_watched = false;
        let mut aider_watched = false;
        let mut continue_sessions_watched = false;
        let mut omp_sessions_watched = false;
        let mut opencode_watched = false;
        if let Some(w) = watcher.as_mut() {
            if paths.projects_root.exists() {
                match w.watch(&paths.projects_root, RecursiveMode::Recursive) {
                    Ok(()) => projects_watched = true,
                    Err(e) => eprintln!(
                        "[token-watcher] 监听 {} 失败: {e}",
                        paths.projects_root.display()
                    ),
                }
            }
            if paths.claude_dir.exists() {
                let _ = w.watch(&paths.claude_dir, RecursiveMode::NonRecursive);
            }
            if paths.settings_path.exists() {
                let _ = w.watch(&paths.settings_path, RecursiveMode::NonRecursive);
            }

            if paths.codex_sessions_root.exists() {
                match w.watch(&paths.codex_sessions_root, RecursiveMode::Recursive) {
                    Ok(()) => codex_sessions_watched = true,
                    Err(e) => eprintln!(
                        "[token-watcher] 监听 {} 失败: {e}",
                        paths.codex_sessions_root.display()
                    ),
                }
            }
            if paths.codex_dir.exists() {
                let _ = w.watch(&paths.codex_dir, RecursiveMode::NonRecursive);
            }
            if paths.codex_config_path.exists() {
                let _ = w.watch(&paths.codex_config_path, RecursiveMode::NonRecursive);
            }

            if paths.cline_tasks_root.exists() {
                match w.watch(&paths.cline_tasks_root, RecursiveMode::Recursive) {
                    Ok(()) => cline_tasks_watched = true,
                    Err(e) => eprintln!(
                        "[token-watcher] 监听 {} 失败: {e}",
                        paths.cline_tasks_root.display()
                    ),
                }
            }
            if paths.roo_tasks_root.exists() {
                match w.watch(&paths.roo_tasks_root, RecursiveMode::Recursive) {
                    Ok(()) => roo_tasks_watched = true,
                    Err(e) => eprintln!(
                        "[token-watcher] 监听 {} 失败: {e}",
                        paths.roo_tasks_root.display()
                    ),
                }
            }
            if paths.aider_analytics_path.exists() {
                match w.watch(&paths.aider_analytics_path, RecursiveMode::NonRecursive) {
                    Ok(()) => aider_watched = true,
                    Err(e) => eprintln!(
                        "[token-watcher] 监听 {} 失败: {e}",
                        paths.aider_analytics_path.display()
                    ),
                }
            }
            if paths.continue_sessions_root.exists() {
                match w.watch(&paths.continue_sessions_root, RecursiveMode::Recursive) {
                    Ok(()) => continue_sessions_watched = true,
                    Err(e) => eprintln!(
                        "[token-watcher] 监听 {} 失败: {e}",
                        paths.continue_sessions_root.display()
                    ),
                }
            }
            if paths.omp_sessions_root.exists() {
                match w.watch(&paths.omp_sessions_root, RecursiveMode::Recursive) {
                    Ok(()) => omp_sessions_watched = true,
                    Err(e) => eprintln!(
                        "[token-watcher] 监听 {} 失败: {e}",
                        paths.omp_sessions_root.display()
                    ),
                }
            }
            if paths.opencode_db_path.exists() {
                let target = paths
                    .opencode_db_path
                    .parent()
                    .unwrap_or(&paths.opencode_db_path);
                match w.watch(target, RecursiveMode::NonRecursive) {
                    Ok(()) => opencode_watched = true,
                    Err(e) => eprintln!("[token-watcher] 监听 {} 失败: {e}", target.display()),
                }
            }
            if paths.antigravity_conversations_root.exists() {
                let _ = w.watch(&paths.antigravity_conversations_root, RecursiveMode::Recursive);
            }
            if paths.zcode_root.exists() {
                let _ = w.watch(&paths.zcode_root, RecursiveMode::Recursive);
            }
            if paths.qoder_root.exists() {
                let _ = w.watch(&paths.qoder_root, RecursiveMode::Recursive);
            }
            if paths.workbuddy_root.exists() {
                let _ = w.watch(&paths.workbuddy_root, RecursiveMode::Recursive);
            }
        }

        let (store, health) = match Store::open(&paths.db_path) {
            Ok(store) => (Some(store), DataHealth::default()),
            Err(e) => {
                eprintln!("[token-watcher] 数据库打开失败，进入只读降级: {e}");
                (
                    None,
                    DataHealth {
                        db_readonly: true,
                        ..Default::default()
                    },
                )
            }
        };
        let db_retry_at = if store.is_none() {
            Some(Instant::now() + DB_RETRY)
        } else {
            None
        };

        let mut engine = Self {
            paths,
            projects_root_canonical,
            claude_dir_canonical,
            codex_sessions_root_canonical,
            codex_dir_canonical,
            cline_tasks_root_canonical,
            roo_tasks_root_canonical,
            omp_sessions_root_canonical,
            continue_sessions_root_canonical,
            sink,
            watcher,
            projects_watched,
            codex_sessions_watched,
            cline_tasks_watched,
            roo_tasks_watched,
            aider_watched,
            continue_sessions_watched,
            omp_sessions_watched,
            opencode_watched,
            store,
            pending_files: HashSet::new(),
            settings_dirty: false,
            last_snapshot: None,
            health,
            last_health_sent: None,
            pending_dirty: false,
            pending_health: None,
            debounce_deadline: None,
            db_retry_at,
            needs_catchup: false,
        };
        engine.apply_initial_settings();
        engine
    }
    /// 检查指定文件是否处于去抖待处理队列中（测试与诊断用）
    pub fn pending_files_contains(&self, path: &Path) -> bool {
        self.pending_files.contains(path)
    }

    /// 启动时先把当前 settings 应用到时间线（在首扫之前），
    /// 保证首扫期间新落库的记录能按当下的档案归因。
    fn apply_initial_settings(&mut self) {
        let snapshot = if self.paths.settings_path.exists() {
            match fs::read_to_string(&self.paths.settings_path) {
                Ok(text) => attr::parse_settings(&text),
                Err(_) => SettingsSnapshot::Unreadable,
            }
        } else {
            // 文件不存在 = 该时刻无归因信息
            SettingsSnapshot::NoAttribution
        };
        if snapshot != SettingsSnapshot::Unreadable {
            if let Some(store) = self.store.as_mut() {
                let result = store.apply_settings_snapshot(&snapshot, now_ms());
                match result {
                    Ok(_) => self.last_snapshot = Some(snapshot),
                    Err(CoreError::Db(e)) => {
                        eprintln!("[token-watcher] 启动 settings 应用失败: {e}");
                        self.enter_readonly();
                    }
                    Err(_) => {}
                }
            }
        }

        if self.paths.codex_config_path.exists() {
            if let Ok(text) = fs::read_to_string(&self.paths.codex_config_path) {
                if let SettingsSnapshot::Profile {
                    endpoint,
                    fingerprint,
                } = attr::parse_codex_config(&text)
                {
                    if let Some(store) = self.store.as_mut() {
                        let _ = store.ensure_profile(&endpoint, fingerprint.as_deref());
                    }
                }
            }
        }
    }

    /// 首扫（D16 进度态）：scanning → done/empty → dashboard-dirty。
    /// watcher 已在此前注册（D21），重叠窗口由 checkpoint 续读覆盖。
    pub fn initial_scan(&mut self) {
        if self.health.db_readonly {
            // 库不可写也结束骨架态：UI 由 data-health 展示只读横幅
            self.push_event("scan-progress", progress_json("done", 0, 0));
            self.publish_health_if_changed();
            return;
        }
        self.push_event("scan-progress", progress_json("scanning", 0, 0));

        let roots: Vec<PathBuf> = [
            &self.paths.projects_root,
            &self.paths.codex_sessions_root,
            &self.paths.cline_tasks_root,
            &self.paths.roo_tasks_root,
            &self.paths.continue_sessions_root,
            &self.paths.omp_sessions_root,
            &self.paths.antigravity_conversations_root,
            &self.paths.zcode_root,
            &self.paths.qoder_root,
            &self.paths.workbuddy_root,
        ]
        .into_iter()
        .filter(|p| p.exists())
        .cloned()
        .collect();

        let aider_file = if self.paths.aider_analytics_path.is_file() {
            Some(self.paths.aider_analytics_path.clone())
        } else {
            None
        };
        let opencode_file = if self.paths.opencode_db_path.is_file() {
            Some(self.paths.opencode_db_path.clone())
        } else {
            None
        };

        if roots.is_empty() && aider_file.is_none() && opencode_file.is_none() {
            self.health.root_unreadable = false;
            self.push_event("scan-progress", progress_json("empty", 0, 0));
            self.emit_dirty();
            self.publish_health_if_changed();
            return;
        }
        let Some(mut store) = self.store.take() else {
            return;
        };
        let sink = &self.sink;
        let mut hidden = false;
        let mut last_emit = Instant::now() - PROGRESS_INTERVAL;
        let mut done = 0u64;

        let mut all_files = Vec::new();
        let mut dirs_failed = 0u64;
        let mut any_root_unreadable = false;
        for root in &roots {
            if let Err(e) = fs::read_dir(root) {
                eprintln!("[token-watcher] 首扫根目录不可读 {}: {e}", root.display());
                any_root_unreadable = true;
                continue;
            }
            ingest::collect_source_files(root, &mut all_files, &mut dirs_failed);
        }
        if let Some(f) = aider_file {
            all_files.push(f);
        }
        if let Some(f) = opencode_file {
            all_files.push(f);
        }
        self.health.root_unreadable = any_root_unreadable;
        all_files.sort();
        let total = all_files.len() as u64;
        if sink.is_visible() {
            sink.emit_json("scan-progress", progress_json("scanning", 0, total));
        }

        let mut stats = ScanStats {
            dirs_failed,
            ..Default::default()
        };

        for (index, path) in all_files.into_iter().enumerate() {
            stats.files_scanned += 1;
            match ingest::ingest_file(&mut store, &path) {
                Ok(file_stats) => {
                    stats.records += file_stats.records;
                    stats.bad_lines += file_stats.bad_lines;
                    if file_stats.truncated {
                        stats.truncated_files += 1;
                    }
                }
                Err(e @ CoreError::Db(_)) => {
                    eprintln!("[token-watcher] 首扫数据库错误: {e}");
                    self.store = Some(store);
                    self.push_event("scan-progress", progress_json("done", done, total));
                    self.enter_readonly();
                    return;
                }
                Err(_) => stats.files_failed += 1,
            }
            done = index as u64 + 1;
            if (done == total || last_emit.elapsed() >= PROGRESS_INTERVAL) && sink.is_visible() {
                last_emit = Instant::now();
                sink.emit_json("scan-progress", progress_json("scanning", done, total));
            } else if !sink.is_visible() {
                hidden = true;
            }
        }
        self.store = Some(store);
        if hidden {
            self.pending_dirty = true;
        }

        if !any_root_unreadable {
            self.health.root_unreadable = false;
        }
        self.health.skipped_files += stats.files_failed;
        let phase = if total == 0 { "empty" } else { "done" };
        self.push_event("scan-progress", progress_json(phase, done, total));
        self.emit_dirty();
        eprintln!(
            "[token-watcher] 首扫完成: files={} failed={} records={} bad_lines={} phase={phase}",
            stats.files_scanned, stats.files_failed, stats.records, stats.bad_lines
        );
        self.publish_health_if_changed();
    }

    /// 处理一条消息（Shutdown 由 [`run_loop`] 拦截）
    pub fn handle(&mut self, msg: Message) {
        match msg {
            Message::Fs(event) => self.collect_event(event),
            Message::FsError(e) => eprintln!("[token-watcher] watcher 错误: {e}"),
            Message::WindowShown => self.on_window_shown(),
            Message::ProxyAttribution {
                message_id,
                profile_id,
                endpoint,
                fingerprint,
            } => self.on_proxy_attribution(message_id, profile_id, endpoint, fingerprint),
            Message::Shutdown => {}
        }
    }

    fn on_proxy_attribution(
        &mut self,
        message_id: String,
        profile_id: Option<i64>,
        endpoint: String,
        fingerprint: Option<String>,
    ) {
        let Some(store) = self.store.as_mut() else {
            return;
        };
        let target_profile_id = match profile_id {
            Some(pid) => pid,
            None => match store.ensure_profile(&endpoint, fingerprint.as_deref()) {
                Ok(pid) => pid,
                Err(e) => {
                    eprintln!("[token-watcher] 代理归因 ensure_profile 失败: {e}");
                    return;
                }
            },
        };
        if let Err(e) =
            store.record_proxy_attribution(&message_id, target_profile_id, Some(&endpoint))
        {
            eprintln!("[token-watcher] 记录代理归因失败: {e}");
            return;
        }
        match store.reconcile_usage_attribution(&message_id, target_profile_id) {
            Ok(changed) if changed > 0 => {
                self.sink.data_changed();
                self.emit_dirty();
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[token-watcher] 代理归因晚到对账失败: {e}");
            }
        }
    }

    /// 下一次需要醒来的时刻：去抖截止 / DB 重试。None → 可阻塞等消息。
    pub fn next_deadline(&self) -> Option<Instant> {
        let debounce =
            if self.pending_files.is_empty() && !self.settings_dirty && !self.needs_catchup {
                None
            } else {
                // 有待处理批次却没有截止时刻（防御）→ 立即处理，绝不无限阻塞
                Some(self.debounce_deadline.unwrap_or_else(Instant::now))
            };
        let retry = if self.store.is_none() {
            self.db_retry_at
        } else {
            None
        };
        match (debounce, retry) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// 到点后的推进：去抖批次落库、DB 降级重试
    pub fn on_tick(&mut self) {
        let now = Instant::now();
        if self.debounce_deadline.is_some_and(|d| now >= d) {
            self.flush();
        }
        if self.store.is_none() {
            if let Some(at) = self.db_retry_at {
                if now >= at {
                    self.try_reopen_store();
                }
            }
        }
    }

    /// 强制处理当前去抖批次（测试直接调用；生产由 on_tick 在窗口到期后调用）
    pub fn flush(&mut self) {
        self.debounce_deadline = None;
        if self.settings_dirty || !self.pending_files.is_empty() || self.needs_catchup {
            let mut changed = false;

            // settings 先于文件：同批次里切换后的记录应按新档案归因
            if self.settings_dirty {
                self.settings_dirty = false;
                match self.apply_settings_from_disk() {
                    Ok(timeline_changed) => changed |= timeline_changed,
                    Err(()) => {
                        self.enter_readonly();
                        return;
                    }
                }
            }
            if self.needs_catchup {
                self.needs_catchup = false;
                changed |= self.quiet_scan();
            }

            let files: Vec<PathBuf> = self.pending_files.drain().collect();
            if files.is_empty() {
                if changed {
                    self.emit_dirty();
                }
                return;
            }

            let Some(mut store) = self.store.take() else {
                return;
            };
            let mut pulses = Vec::new();
            let mut touched_sessions = std::collections::HashSet::new();
            let mut parsed_records = 0u64;
            let mut skipped = 0u64;
            let mut db_error = None;
            for path in files {
                match ingest::ingest_file_with_records(&mut store, &path) {
                    Ok((stats, records)) => {
                        parsed_records += stats.records;
                        for record in &records {
                            touched_sessions.insert(record.session_id.clone());
                            pulses.push(pulse_json(record));
                        }
                    }
                    Err(e @ CoreError::Db(_)) => {
                        db_error = Some(e);
                        break;
                    }
                    Err(_) => {
                        // 原子 rename / 删除竞态导致的瞬时消失不计入 D17 跳过数
                        if path.exists() {
                            skipped += 1;
                        }
                    }
                }
            }
            self.store = Some(store);
            if let Some(e) = db_error {
                eprintln!("[token-watcher] 增量 ingest 数据库错误: {e}");
                self.enter_readonly();
                return;
            }
            if skipped > 0 {
                self.health.skipped_files += skipped;
                self.publish_health_if_changed();
            }
            if parsed_records > 0 {
                changed = true;
            }
            // usage-pulse：本次解析出的 Record 行逐条发（重复导入会重发，
            // UI 幂等累计可接受——契约的到达脉冲语义）
            for pulse in pulses {
                self.push_event("usage-pulse", pulse);
            }
            for sid in touched_sessions {
                self.push_event("session-flight-updated", json!({ "session_id": sid }));
            }
            if changed {
                self.emit_dirty();
            }
        }
    }

    fn collect_event(&mut self, event: Event) {
        // 删除源文件不删历史（SC4）；访问事件无意义
        if matches!(event.kind, EventKind::Access(_) | EventKind::Remove(_)) {
            return;
        }
        let mut relevant = false;
        for path in &event.paths {
            if self.is_source_event(path) {
                if path == &self.paths.opencode_db_path
                    || path.file_name().and_then(|n| n.to_str()) == Some("opencode.db")
                    || path.file_name().and_then(|n| n.to_str()) == Some("opencode.db-wal")
                {
                    self.pending_files
                        .insert(self.paths.opencode_db_path.clone());
                } else if self.under_antigravity(path) && path.to_string_lossy().ends_with(".db-wal") {
                    let db_str = path.to_string_lossy().trim_end_matches("-wal").to_string();
                    let db_path = std::path::PathBuf::from(db_str);
                    if db_path.is_file() {
                        self.pending_files.insert(db_path);
                    }
                } else {
                    self.pending_files.insert(path.clone());
                }
                relevant = true;
            } else if path.is_dir() {
                if self.collect_dir_sources(path) {
                    relevant = true;
                }
            } else if self.under_omp(path) {
                // omp 专有文件联动：
                // 1) 锁文件 .<session>.jsonl.lock.os 联动对应 <session>.jsonl
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if file_name.ends_with(".lock.os") && file_name.contains(".jsonl") {
                        let stripped = file_name
                            .trim_start_matches('.')
                            .trim_end_matches(".lock.os");
                        if stripped.ends_with(".jsonl") {
                            if let Some(parent) = path.parent() {
                                let jsonl = parent.join(stripped);
                                if jsonl.is_file() {
                                    self.pending_files.insert(jsonl);
                                    relevant = true;
                                }
                            }
                        }
                    }
                }
                // 2) 工具日志子目录 <session>/<tool>.log 联动对应 <session>.jsonl
                if let Some(parent) = path.parent() {
                    let candidate = parent.with_extension("jsonl");
                    if candidate.is_file() && self.under_omp(&candidate) {
                        self.pending_files.insert(candidate);
                        relevant = true;
                    }
                }
            }
            if self.is_settings_event(path) || self.is_codex_config_event(path) {
                self.settings_dirty = true;
                relevant = true;
            }
            if !self.projects_watched && self.is_projects_root(path) && self.try_watch_projects() {
                self.needs_catchup = true;
                relevant = true;
            }
            if !self.codex_sessions_watched
                && self.is_codex_sessions_root(path)
                && self.try_watch_codex_sessions()
            {
                self.needs_catchup = true;
                relevant = true;
            }
            if !self.cline_tasks_watched
                && self.is_cline_tasks_root(path)
                && self.try_watch_cline_tasks()
            {
                self.needs_catchup = true;
                relevant = true;
            }
            if !self.roo_tasks_watched && self.is_roo_tasks_root(path) && self.try_watch_roo_tasks()
            {
                self.needs_catchup = true;
                relevant = true;
            }
            if !self.aider_watched
                && path == &self.paths.aider_analytics_path
                && self.try_watch_aider()
            {
                self.needs_catchup = true;
                relevant = true;
            }
            if !self.continue_sessions_watched
                && self.is_continue_sessions_root(path)
                && self.try_watch_continue_sessions()
            {
                self.needs_catchup = true;
                relevant = true;
            }
            if !self.omp_sessions_watched
                && self.is_omp_sessions_root(path)
                && self.try_watch_omp_sessions()
            {
                self.needs_catchup = true;
                relevant = true;
            }
            if !self.opencode_watched && self.is_opencode_event(path) && self.try_watch_opencode() {
                self.needs_catchup = true;
                relevant = true;
            }
        }
        if relevant {
            self.debounce_deadline = Some(Instant::now() + DEBOUNCE);
        }
    }

    fn under_projects(&self, path: &Path) -> bool {
        if path.starts_with(&self.paths.projects_root) {
            return true;
        }
        match (&self.projects_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => false,
        }
    }

    fn under_codex(&self, path: &Path) -> bool {
        if path.starts_with(&self.paths.codex_sessions_root) {
            return true;
        }
        match (&self.codex_sessions_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => false,
        }
    }

    fn under_cline(&self, path: &Path) -> bool {
        if path.starts_with(&self.paths.cline_tasks_root) {
            return true;
        }
        match (&self.cline_tasks_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => false,
        }
    }

    fn under_roo(&self, path: &Path) -> bool {
        if path.starts_with(&self.paths.roo_tasks_root) {
            return true;
        }
        match (&self.roo_tasks_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => false,
        }
    }

    fn is_cline_tasks_root(&self, path: &Path) -> bool {
        if path == self.paths.cline_tasks_root {
            return true;
        }
        match (&self.cline_tasks_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical == *root,
            _ => false,
        }
    }
    fn is_roo_tasks_root(&self, path: &Path) -> bool {
        if path == self.paths.roo_tasks_root {
            return true;
        }
        match (&self.roo_tasks_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical == *root,
            _ => false,
        }
    }

    fn under_omp(&self, path: &Path) -> bool {
        if path.starts_with(&self.paths.omp_sessions_root) {
            return true;
        }
        match (&self.omp_sessions_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => false,
        }
    }

    fn under_continue(&self, path: &Path) -> bool {
        if path.starts_with(&self.paths.continue_sessions_root) {
            return true;
        }
        match (
            &self.continue_sessions_root_canonical,
            fs::canonicalize(path),
        ) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => false,
        }
    }
    fn under_zcode(&self, path: &Path) -> bool {
        match (
            fs::canonicalize(&self.paths.zcode_root).ok(),
            fs::canonicalize(path),
        ) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => path.starts_with(&self.paths.zcode_root),
        }
    }

    fn under_qoder(&self, path: &Path) -> bool {
        match (
            fs::canonicalize(&self.paths.qoder_root).ok(),
            fs::canonicalize(path),
        ) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => path.starts_with(&self.paths.qoder_root),
        }
    }

    fn under_workbuddy(&self, path: &Path) -> bool {
        match (
            fs::canonicalize(&self.paths.workbuddy_root).ok(),
            fs::canonicalize(path),
        ) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => path.starts_with(&self.paths.workbuddy_root),
        }
    }

    fn under_antigravity(&self, path: &Path) -> bool {
        match (
            fs::canonicalize(&self.paths.antigravity_conversations_root).ok(),
            fs::canonicalize(path),
        ) {
            (Some(root), Ok(canonical)) => canonical.starts_with(root),
            _ => path.starts_with(&self.paths.antigravity_conversations_root),
        }
    }

    fn is_source_event(&self, path: &Path) -> bool {
        let is_jsonl = path.extension().and_then(|e| e.to_str()) == Some("jsonl");
        let is_ui_messages = path.file_name().and_then(|n| n.to_str()) == Some("ui_messages.json");
        let is_continue =
            path.extension().and_then(|e| e.to_str()) == Some("json") && self.under_continue(path);
        let is_opencode = path == self.paths.opencode_db_path
            || path.file_name().and_then(|n| n.to_str()) == Some("opencode.db")
            || path.file_name().and_then(|n| n.to_str()) == Some("opencode.db-wal");
        let is_antigravity = self.under_antigravity(path)
            && (path.extension().and_then(|e| e.to_str()) == Some("db")
                || path.to_string_lossy().ends_with(".db-wal"));

        (is_jsonl
            && (self.under_projects(path)
                || self.under_codex(path)
                || self.under_omp(path)
                || self.under_zcode(path)
                || self.under_qoder(path)
                || self.under_workbuddy(path)
                || path == self.paths.aider_analytics_path))
            || (is_ui_messages && (self.under_cline(path) || self.under_roo(path)))
            || is_continue
            || is_opencode
            || is_antigravity
    }

    fn is_projects_root(&self, path: &Path) -> bool {
        if path == self.paths.projects_root {
            return true;
        }
        match (&self.projects_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical == *root,
            _ => false,
        }
    }

    fn is_codex_sessions_root(&self, path: &Path) -> bool {
        if path == self.paths.codex_sessions_root {
            return true;
        }
        match (&self.codex_sessions_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical == *root,
            _ => false,
        }
    }
    fn is_continue_sessions_root(&self, path: &Path) -> bool {
        if path == self.paths.continue_sessions_root {
            return true;
        }
        match (
            &self.continue_sessions_root_canonical,
            fs::canonicalize(path),
        ) {
            (Some(root), Ok(canonical)) => canonical == *root,
            _ => false,
        }
    }

    fn is_omp_sessions_root(&self, path: &Path) -> bool {
        if path == self.paths.omp_sessions_root {
            return true;
        }
        match (&self.omp_sessions_root_canonical, fs::canonicalize(path)) {
            (Some(root), Ok(canonical)) => canonical == *root,
            _ => false,
        }
    }

    fn is_opencode_event(&self, path: &Path) -> bool {
        if path == self.paths.opencode_db_path {
            return true;
        }
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if file_name == "opencode.db" || file_name == "opencode.db-wal" {
                return true;
            }
        }
        false
    }

    /// 目录级变更事件处理（应对 macOS FSEvents 批量写聚合）：
    /// 自动提取该目录下对应引擎的有效源文件加入待读取队列。
    fn collect_dir_sources(&mut self, path: &Path) -> bool {
        let mut found = false;
        if self.under_codex(path) && !self.is_codex_sessions_root(path) {
            let before = self.pending_files.len();
            collect_jsonl_recursive(path, 3, &mut self.pending_files);
            found = self.pending_files.len() > before;
        } else if (self.under_projects(path) && !self.is_projects_root(path))
            || self.under_omp(path)
        {
            let before = self.pending_files.len();
            collect_jsonl_recursive(path, 2, &mut self.pending_files);
            found = self.pending_files.len() > before;
        } else if (self.under_cline(path) || self.under_roo(path))
            && !self.is_cline_tasks_root(path)
            && !self.is_roo_tasks_root(path)
        {
            let ui_msg = path.join("ui_messages.json");
            if ui_msg.is_file() {
                self.pending_files.insert(ui_msg);
                found = true;
            } else if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join("ui_messages.json");
                    if candidate.is_file() {
                        self.pending_files.insert(candidate);
                        found = true;
                    }
                }
            }
        } else if self.under_continue(path) {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("json") {
                        self.pending_files.insert(p);
                        found = true;
                    }
                }
            }
        } else if self.under_zcode(path) || self.under_qoder(path) || self.under_workbuddy(path) {
            let before = self.pending_files.len();
            collect_jsonl_recursive(path, 3, &mut self.pending_files);
            found = self.pending_files.len() > before;
        } else if self.under_antigravity(path) {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("db") {
                        self.pending_files.insert(p);
                        found = true;
                    }
                }
            }
        }
        found
    }

    /// settings 相关事件：目标文件本身，或 ~/.claude 下名字含 "settings" 的直接子项
    /// （原子写临时文件 / rename 源）。
    fn is_settings_event(&self, path: &Path) -> bool {
        if path == self.paths.settings_path {
            return true;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        if !name.contains("settings") {
            return false;
        }
        if path.parent() == Some(self.paths.claude_dir.as_path()) {
            return true;
        }
        match (&self.claude_dir_canonical, path.parent()) {
            (Some(root), Some(parent)) => fs::canonicalize(parent)
                .map(|p| p == *root)
                .unwrap_or(false),
            _ => false,
        }
    }

    fn is_codex_config_event(&self, path: &Path) -> bool {
        if path == self.paths.codex_config_path {
            return true;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        if name != "config.toml" {
            return false;
        }
        if path.parent() == Some(self.paths.codex_dir.as_path()) {
            return true;
        }
        match (&self.codex_dir_canonical, path.parent()) {
            (Some(root), Some(parent)) => fs::canonicalize(parent)
                .map(|p| p == *root)
                .unwrap_or(false),
            _ => false,
        }
    }

    fn try_watch_codex_sessions(&mut self) -> bool {
        if self.codex_sessions_watched {
            return true;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.watch(&self.paths.codex_sessions_root, RecursiveMode::Recursive) {
            Ok(()) => {
                self.codex_sessions_watched = true;
                eprintln!("[token-watcher] 检测到 codex/sessions 目录，已开始监听");
                true
            }
            Err(e) => {
                eprintln!("[token-watcher] 补注册 codex/sessions 监听失败: {e}");
                false
            }
        }
    }
    fn try_watch_cline_tasks(&mut self) -> bool {
        if self.cline_tasks_watched {
            return true;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.watch(&self.paths.cline_tasks_root, RecursiveMode::Recursive) {
            Ok(()) => {
                self.cline_tasks_watched = true;
                eprintln!("[token-watcher] 检测到 cline/tasks 目录，已开始监听");
                true
            }
            Err(_) => false,
        }
    }

    fn try_watch_roo_tasks(&mut self) -> bool {
        if self.roo_tasks_watched {
            return true;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.watch(&self.paths.roo_tasks_root, RecursiveMode::Recursive) {
            Ok(()) => {
                self.roo_tasks_watched = true;
                eprintln!("[token-watcher] 检测到 roo/tasks 目录，已开始监听");
                true
            }
            Err(_) => false,
        }
    }

    fn try_watch_aider(&mut self) -> bool {
        if self.aider_watched {
            return true;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.watch(
            &self.paths.aider_analytics_path,
            RecursiveMode::NonRecursive,
        ) {
            Ok(()) => {
                self.aider_watched = true;
                eprintln!("[token-watcher] 检测到 aider analytics 文件，已开始监听");
                true
            }
            Err(_) => false,
        }
    }

    fn try_watch_continue_sessions(&mut self) -> bool {
        if self.continue_sessions_watched {
            return true;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.watch(&self.paths.continue_sessions_root, RecursiveMode::Recursive) {
            Ok(()) => {
                self.continue_sessions_watched = true;
                eprintln!("[token-watcher] 检测到 continue/sessions 目录，已开始监听");
                true
            }
            Err(_) => false,
        }
    }

    fn try_watch_omp_sessions(&mut self) -> bool {
        if self.omp_sessions_watched {
            return true;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.watch(&self.paths.omp_sessions_root, RecursiveMode::Recursive) {
            Ok(()) => {
                self.omp_sessions_watched = true;
                eprintln!("[token-watcher] 检测到 omp/agent/sessions 目录，已开始监听");
                true
            }
            Err(_) => false,
        }
    }

    fn try_watch_opencode(&mut self) -> bool {
        if self.opencode_watched {
            return true;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        let target = self
            .paths
            .opencode_db_path
            .parent()
            .unwrap_or(&self.paths.opencode_db_path);
        match watcher.watch(target, RecursiveMode::NonRecursive) {
            Ok(()) => {
                self.opencode_watched = true;
                eprintln!("[token-watcher] 检测到 opencode.db 目录，已开始监听");
                true
            }
            Err(_) => false,
        }
    }

    fn try_watch_projects(&mut self) -> bool {
        if self.projects_watched {
            return true;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.watch(&self.paths.projects_root, RecursiveMode::Recursive) {
            Ok(()) => {
                self.projects_watched = true;
                eprintln!("[token-watcher] 检测到 projects 目录，已开始监听");
                true
            }
            Err(e) => {
                eprintln!("[token-watcher] 补注册 projects 监听失败: {e}");
                false
            }
        }
    }

    /// 读取并应用 settings.json。瞬时可读失败/坏 JSON 一律保持上一态（契约）。
    /// 返回时间线是否发生变化。
    fn apply_settings_from_disk(&mut self) -> Result<bool, ()> {
        let mut changed = false;
        let snapshot = match fs::read_to_string(&self.paths.settings_path) {
            Ok(text) => attr::parse_settings(&text),
            Err(_) => SettingsSnapshot::NoAttribution,
        };
        if snapshot != SettingsSnapshot::Unreadable
            && self.last_snapshot.as_ref() != Some(&snapshot)
        {
            if let Some(store) = self.store.as_mut() {
                match store.apply_settings_snapshot(&snapshot, now_ms()) {
                    Ok(outcome) => {
                        self.last_snapshot = Some(snapshot);
                        if matches!(outcome, ApplyOutcome::Changed { .. }) {
                            eprintln!("[token-watcher] settings.json 变化，归因时间线已更新");
                            changed = true;
                        }
                    }
                    Err(CoreError::Db(e)) => {
                        eprintln!("[token-watcher] settings apply 数据库错误: {e}");
                        return Err(());
                    }
                    Err(_) => {}
                }
            }
        }

        if self.paths.codex_config_path.exists() {
            if let Ok(text) = fs::read_to_string(&self.paths.codex_config_path) {
                if let SettingsSnapshot::Profile {
                    endpoint,
                    fingerprint,
                } = attr::parse_codex_config(&text)
                {
                    if let Some(store) = self.store.as_mut() {
                        let _ = store.ensure_profile(&endpoint, fingerprint.as_deref());
                    }
                }
            }
        }
        Ok(changed)
    }

    /// 静默全扫（不做进度态）：DB 恢复 / 监听根迟到 / 窗口重现时补课。
    /// 返回是否需要 dashboard-dirty。
    fn quiet_scan(&mut self) -> bool {
        let Some(mut store) = self.store.take() else {
            return false;
        };
        let roots: Vec<PathBuf> = [
            &self.paths.projects_root,
            &self.paths.codex_sessions_root,
            &self.paths.cline_tasks_root,
            &self.paths.roo_tasks_root,
            &self.paths.continue_sessions_root,
            &self.paths.omp_sessions_root,
            &self.paths.antigravity_conversations_root,
            &self.paths.zcode_root,
            &self.paths.qoder_root,
            &self.paths.workbuddy_root,
        ]
        .into_iter()
        .filter(|p| p.exists())
        .cloned()
        .collect();
        let mut changed = false;
        for root in roots {
            match ingest::scan_dir(&mut store, &root) {
                Ok(stats) => {
                    self.health.root_unreadable = false;
                    self.health.skipped_files += stats.files_failed;
                    if stats.records > 0 || stats.truncated_files > 0 {
                        changed = true;
                    }
                }
                Err(CoreError::Db(e)) => {
                    eprintln!("[token-watcher] 补扫数据库错误: {e}");
                    self.store = Some(store);
                    self.enter_readonly();
                    return false;
                }
                Err(e) => {
                    eprintln!("[token-watcher] 补扫根目录不可读: {e}");
                    self.health.root_unreadable = true;
                }
            }
        }
        if self.paths.aider_analytics_path.is_file() {
            match ingest::ingest_file(&mut store, &self.paths.aider_analytics_path) {
                Ok(stats) => {
                    if stats.records > 0 || stats.truncated {
                        changed = true;
                    }
                }
                Err(CoreError::Db(e)) => {
                    eprintln!("[token-watcher] 补扫 aider 数据库错误: {e}");
                    self.store = Some(store);
                    self.enter_readonly();
                    return false;
                }
                Err(_) => {}
            }
        }
        if self.paths.opencode_db_path.is_file() {
            match ingest::ingest_file(&mut store, &self.paths.opencode_db_path) {
                Ok(stats) => {
                    if stats.records > 0 || stats.truncated {
                        changed = true;
                    }
                }
                Err(CoreError::Db(e)) => {
                    eprintln!("[token-watcher] 补扫 opencode 数据库错误: {e}");
                    self.store = Some(store);
                    self.enter_readonly();
                    return false;
                }
                Err(_) => {}
            }
        }
        self.store = Some(store);
        self.publish_health_if_changed();
        changed
    }

    fn on_window_shown(&mut self) {
        // 契约水合语义：重新显示 → 一次 dashboard-dirty，UI 全量重拉
        self.pending_dirty = false;
        if self.sink.is_visible() {
            self.sink.emit_json("dashboard-dirty", Value::Null);
        } else {
            self.pending_dirty = true;
        }
        self.sink.window_shown();
        if let Some(health) = self.pending_health.take() {
            if self.sink.is_visible() {
                self.sink.emit_json("data-health", health.to_json());
            } else {
                self.pending_health = Some(health);
            }
        }
        // 根目录之前不可读 → 趁机重试一次（不引入轮询）
        // 重新探测启动时可能未安装/不存在的目录（解决 F5 盲区）
        if !self.projects_watched && self.paths.projects_root.exists() && self.try_watch_projects()
        {
            self.needs_catchup = true;
        }
        if !self.codex_sessions_watched
            && self.paths.codex_sessions_root.exists()
            && self.try_watch_codex_sessions()
        {
            self.needs_catchup = true;
        }
        if !self.cline_tasks_watched
            && self.paths.cline_tasks_root.exists()
            && self.try_watch_cline_tasks()
        {
            self.needs_catchup = true;
        }
        if !self.roo_tasks_watched
            && self.paths.roo_tasks_root.exists()
            && self.try_watch_roo_tasks()
        {
            self.needs_catchup = true;
        }
        if !self.aider_watched && self.paths.aider_analytics_path.exists() && self.try_watch_aider()
        {
            self.needs_catchup = true;
        }
        if !self.continue_sessions_watched
            && self.paths.continue_sessions_root.exists()
            && self.try_watch_continue_sessions()
        {
            self.needs_catchup = true;
        }
        if !self.omp_sessions_watched
            && self.paths.omp_sessions_root.exists()
            && self.try_watch_omp_sessions()
        {
            self.needs_catchup = true;
        }
        if !self.opencode_watched
            && self.paths.opencode_db_path.exists()
            && self.try_watch_opencode()
        {
            self.needs_catchup = true;
        }

        // 根目录之前不可读或有待补扫 → 趁机重试一次（不引入轮询）
        let root_exists = self.paths.projects_root.exists()
            || self.paths.codex_sessions_root.exists()
            || self.paths.cline_tasks_root.exists()
            || self.paths.roo_tasks_root.exists()
            || self.paths.continue_sessions_root.exists()
            || self.paths.omp_sessions_root.exists()
            || self.paths.aider_analytics_path.exists()
            || self.paths.opencode_db_path.exists();
        if (self.health.root_unreadable || self.needs_catchup) && root_exists && self.quiet_scan() {
            self.needs_catchup = false;
            self.emit_dirty();
        }
    }

    fn try_reopen_store(&mut self) {
        self.db_retry_at = None;
        match Store::open(&self.paths.db_path) {
            Ok(store) => {
                self.store = Some(store);
                self.health.db_readonly = false;
                eprintln!("[token-watcher] 数据库恢复，补扫断档期间的变更");
                self.publish_health_if_changed();
                let mut changed = false;
                // 断档期间 settings.json 可能已经变化：恢复后补一次快照应用
                match self.apply_settings_from_disk() {
                    Ok(timeline_changed) => changed |= timeline_changed,
                    Err(()) => {
                        self.enter_readonly();
                        return;
                    }
                }
                if self.quiet_scan() {
                    changed = true;
                }
                if changed {
                    self.emit_dirty();
                }
            }
            Err(e) => {
                eprintln!("[token-watcher] 数据库仍不可用: {e}");
                self.db_retry_at = Some(Instant::now() + DB_RETRY);
            }
        }
    }

    fn enter_readonly(&mut self) {
        self.store = None;
        self.pending_files.clear();
        self.settings_dirty = false;
        if !self.health.db_readonly {
            eprintln!("[token-watcher] 数据库不可写，进入只读降级（每 10s 重试）");
        }
        self.health.db_readonly = true;
        self.db_retry_at = Some(Instant::now() + DB_RETRY);
        self.publish_health_if_changed();
    }

    fn publish_health_if_changed(&mut self) {
        let health = self.health;
        if self.last_health_sent == Some(health) {
            return;
        }
        self.last_health_sent = Some(health);
        if self.sink.is_visible() {
            self.sink.emit_json("data-health", health.to_json());
        } else {
            self.pending_health = Some(health);
        }
    }

    fn emit_dirty(&mut self) {
        // T5：落库 → 托盘刷新（不受窗口可见性影响，隐藏时也更新菜单栏数字）
        self.sink.data_changed();
        if self.sink.is_visible() {
            self.pending_dirty = false;
            self.sink.emit_json("dashboard-dirty", Value::Null);
        } else {
            self.pending_dirty = true;
        }
    }

    /// 发一个契约事件；窗口隐藏时挂起（后续任意事件恢复时补 dashboard-dirty）
    fn push_event(&mut self, event: &str, payload: Value) {
        if event == "scan-progress" {
            let phase = payload.get("phase").and_then(Value::as_str).unwrap_or("");
            let done = payload
                .get("files_done")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let total = payload
                .get("files_total")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let percent = payload
                .get("percent")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            self.sink.status_changed(phase, done, total, percent);
        }
        if self.sink.is_visible() {
            if self.pending_dirty {
                self.pending_dirty = false;
                self.sink.emit_json("dashboard-dirty", Value::Null);
            }
            self.sink.emit_json(event, payload);
        } else {
            self.pending_dirty = true;
        }
    }
}

/// 数据线程主循环：消息驱动 + 去抖/DB 重试超时
pub fn run_loop<S: EventSink>(mut engine: DataEngine<S>, rx: Receiver<Message>) {
    loop {
        match engine.next_deadline() {
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(Message::Shutdown) => break,
                    Ok(msg) => engine.handle(msg),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match rx.recv() {
                Ok(Message::Shutdown) => break,
                Ok(msg) => engine.handle(msg),
                Err(_) => break,
            },
        }
        engine.on_tick();
    }
}

/// 后台数据线程句柄
pub struct DataHandle {
    tx: Sender<Message>,
    join: Option<thread::JoinHandle<()>>,
}

impl DataHandle {
    /// 窗口重新显示：触发一次 dashboard-dirty（契约水合语义）
    pub fn window_shown(&self) {
        let _ = self.tx.send(Message::WindowShown);
    }

    pub fn sender(&self) -> Sender<Message> {
        self.tx.clone()
    }

    /// 干净退出：停 watcher、结束数据线程
    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Message::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// 秒退（Phase 3）：发 Shutdown 后不 join 数据线程（首扫大历史时
    /// join 最坏 ~18s，退出不该等它）。进程退出由 OS 回收线程；已提交
    /// 事务由 WAL 保证，未推进的 checkpoint 下次重读走 UPSERT 幂等。
    pub fn detach(&mut self) {
        let _ = self.tx.send(Message::Shutdown);
        if let Some(join) = self.join.take() {
            std::mem::forget(join);
        }
    }
}

impl Drop for DataHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Shutdown);
    }
}

/// 启动后台数据线程（Tauri 接线用）
///
/// `tray_tx`：T5 托盘刷新通知（数据落库后发送 [`TrayCommand::DataChanged`]）
pub fn spawn(app: AppHandle, paths: DataPaths, tray_tx: Sender<TrayCommand>) -> DataHandle {
    let (tx, rx) = mpsc::channel();
    let sink = TauriSink { app, tray_tx };
    let engine_tx = tx.clone();
    let join = thread::Builder::new()
        .name("token-watcher-data".into())
        .spawn(move || {
            let mut engine = DataEngine::new(paths, sink, engine_tx);
            engine.initial_scan();
            run_loop(engine, rx);
            eprintln!("[token-watcher] 数据线程退出");
        })
        .map_err(|e| eprintln!("[token-watcher] 数据线程启动失败: {e}"))
        .ok();
    DataHandle { tx, join }
}

/// 递归收集指定目录层级内的所有 .jsonl 文件
fn collect_jsonl_recursive(
    dir: &Path,
    max_depth: usize,
    out: &mut std::collections::HashSet<PathBuf>,
) {
    if max_depth == 0 {
        return;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    out.insert(p);
                }
            } else if p.is_dir() && max_depth > 1 {
                collect_jsonl_recursive(&p, max_depth - 1, out);
            }
        }
    }
}
