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
use crate::core::ingest;
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
    pub db_path: PathBuf,
}

impl DataPaths {
    pub fn default_paths() -> Result<Self, CoreError> {
        let home =
            home::home_dir().ok_or_else(|| CoreError::Other("无法定位用户 home 目录".into()))?;
        let claude_dir = home.join(".claude");
        Ok(Self {
            projects_root: claude_dir.join("projects"),
            settings_path: claude_dir.join("settings.json"),
            claude_dir,
            db_path: home.join(".token-watcher").join("db.sqlite3"),
        })
    }
}

/// 数据线程消息：fs 事件、窗口显示、退出
pub enum Message {
    Fs(Event),
    FsError(String),
    WindowShown,
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
}

struct TauriSink {
    app: AppHandle,
    /// T5：托盘刷新内部通道（与 UI emit 并行，窗口隐藏也照常通知）
    tray_tx: Sender<TrayCommand>,
}

impl EventSink for TauriSink {
    fn is_visible(&self) -> bool {
        self.app
            .get_webview_window(WINDOW_LABEL)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(true)
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
    sink: S,
    /// 由 notify 回调持有；Drop 即停止监听
    watcher: Option<RecommendedWatcher>,
    projects_watched: bool,
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
            // ~/.claude 非递归监听始终注册：覆盖 settings.json 尚不存在、
            // 原子写的临时文件、以及 rename 覆盖目标文件的事件路径
            if paths.claude_dir.exists() {
                if let Err(e) = w.watch(&paths.claude_dir, RecursiveMode::NonRecursive) {
                    eprintln!(
                        "[token-watcher] 监听 {} 失败: {e}",
                        paths.claude_dir.display()
                    );
                }
            }
            if paths.settings_path.exists() {
                let _ = w.watch(&paths.settings_path, RecursiveMode::NonRecursive);
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
            sink,
            watcher,
            projects_watched,
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
        if snapshot == SettingsSnapshot::Unreadable {
            return;
        }
        let Some(store) = self.store.as_mut() else {
            return;
        };
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

        if !self.paths.projects_root.exists() {
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
        let mut total = 0u64;
        let result =
            ingest::scan_dir_with_progress(&mut store, &self.paths.projects_root, |d, t| {
                done = d;
                total = t;
                // 首个 (0,total) 与收尾 (total,total) 必发，中间按最小间隔节流
                if d != t && last_emit.elapsed() < PROGRESS_INTERVAL {
                    return;
                }
                last_emit = Instant::now();
                if sink.is_visible() {
                    sink.emit_json("scan-progress", progress_json("scanning", d, t));
                } else {
                    hidden = true;
                }
            });
        self.store = Some(store);
        if hidden {
            self.pending_dirty = true;
        }

        match result {
            Ok(stats) => {
                self.health.root_unreadable = false;
                self.health.skipped_files += stats.files_failed;
                let phase = if total == 0 { "empty" } else { "done" };
                self.push_event("scan-progress", progress_json(phase, done, total));
                self.emit_dirty();
                eprintln!(
                    "[token-watcher] 首扫完成: files={} failed={} records={} bad_lines={} phase={phase}",
                    stats.files_scanned, stats.files_failed, stats.records, stats.bad_lines
                );
            }
            Err(CoreError::Db(e)) => {
                eprintln!("[token-watcher] 首扫数据库错误: {e}");
                self.push_event("scan-progress", progress_json("done", done, total));
                self.enter_readonly();
            }
            Err(e) => {
                eprintln!("[token-watcher] 首扫根目录不可读: {e}");
                self.health.root_unreadable = true;
                self.push_event("scan-progress", progress_json("empty", 0, 0));
                self.emit_dirty();
            }
        }
        self.publish_health_if_changed();
    }

    /// 处理一条消息（Shutdown 由 [`run_loop`] 拦截）
    pub fn handle(&mut self, msg: Message) {
        match msg {
            Message::Fs(event) => self.collect_event(event),
            Message::FsError(e) => eprintln!("[token-watcher] watcher 错误: {e}"),
            Message::WindowShown => self.on_window_shown(),
            Message::Shutdown => {}
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
            let mut parsed_records = 0u64;
            let mut skipped = 0u64;
            let mut db_error = None;
            for path in files {
                match ingest::ingest_file_with_records(&mut store, &path) {
                    Ok((stats, records)) => {
                        parsed_records += stats.records;
                        for record in &records {
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
            if self.is_jsonl_event(path) {
                self.pending_files.insert(path.clone());
                relevant = true;
            }
            if self.is_settings_event(path) {
                self.settings_dirty = true;
                relevant = true;
            }
            if !self.projects_watched && self.is_projects_root(path) && self.try_watch_projects() {
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

    fn is_jsonl_event(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("jsonl") && self.under_projects(path)
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
        let snapshot = match fs::read_to_string(&self.paths.settings_path) {
            Ok(text) => attr::parse_settings(&text),
            // 原子写 rename 的空窗 / 临时不可读：保持上一态
            Err(_) => return Ok(false),
        };
        if snapshot == SettingsSnapshot::Unreadable {
            return Ok(false);
        }
        if self.last_snapshot.as_ref() == Some(&snapshot) {
            return Ok(false);
        }
        let Some(store) = self.store.as_mut() else {
            return Err(());
        };
        let result = store.apply_settings_snapshot(&snapshot, now_ms());
        match result {
            Ok(outcome) => {
                self.last_snapshot = Some(snapshot);
                let changed = matches!(outcome, ApplyOutcome::Changed { .. });
                if changed {
                    eprintln!("[token-watcher] settings.json 变化，归因时间线已更新");
                }
                Ok(changed)
            }
            Err(CoreError::Db(e)) => {
                eprintln!("[token-watcher] settings apply 数据库错误: {e}");
                Err(())
            }
            Err(_) => Ok(false),
        }
    }

    /// 静默全扫（不做进度态）：DB 恢复 / 监听根迟到 / 窗口重现时补课。
    /// 返回是否需要 dashboard-dirty。
    fn quiet_scan(&mut self) -> bool {
        let Some(mut store) = self.store.take() else {
            return false;
        };
        let result = ingest::scan_dir(&mut store, &self.paths.projects_root);
        self.store = Some(store);
        match result {
            Ok(stats) => {
                self.health.root_unreadable = false;
                self.health.skipped_files += stats.files_failed;
                self.publish_health_if_changed();
                stats.records > 0 || stats.truncated_files > 0
            }
            Err(CoreError::Db(e)) => {
                eprintln!("[token-watcher] 补扫数据库错误: {e}");
                self.enter_readonly();
                false
            }
            Err(e) => {
                eprintln!("[token-watcher] 补扫根目录不可读: {e}");
                self.health.root_unreadable = true;
                self.publish_health_if_changed();
                false
            }
        }
    }

    fn on_window_shown(&mut self) {
        // 契约水合语义：重新显示 → 一次 dashboard-dirty，UI 全量重拉
        self.pending_dirty = false;
        if self.sink.is_visible() {
            self.sink.emit_json("dashboard-dirty", Value::Null);
        } else {
            self.pending_dirty = true;
        }
        if let Some(health) = self.pending_health.take() {
            if self.sink.is_visible() {
                self.sink.emit_json("data-health", health.to_json());
            } else {
                self.pending_health = Some(health);
            }
        }
        // 根目录之前不可读 → 趁机重试一次（不引入轮询）
        if self.health.root_unreadable && self.paths.projects_root.exists() && self.quiet_scan() {
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
