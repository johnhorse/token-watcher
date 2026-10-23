//! v0.7.2 P1：手写滚动日志器（零依赖）
//!
//! - 目录 `~/.token-watcher/log/`，文件 `token-watcher.log`；
//! - 单文件上限与保留份数可配（app_settings 的 `log_max_bytes` / `log_keep`，
//!   默认 2MB × 2）；
//! - 任何 IO 失败静默降级为 no-op：日志绝不阻塞或拖垮数据路径；
//! - 只记录路径与计数；调用方不得写入消息内容、token 明细或密钥。

use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 默认单文件上限：约 2MB
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// 默认保留份数（含当前文件）
pub const DEFAULT_KEEP: usize = 2;
/// 日志文件名
pub const FILE_NAME: &str = "token-watcher.log";

pub struct RollingLog {
    dir: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    /// 当前文件已写字节数（滚动判定用，不每行做 stat）
    written: u64,
}

impl RollingLog {
    /// 打开（必要时创建）日志目录；失败不报错，后续写入自动降级 no-op
    pub fn new(dir: PathBuf, max_bytes: u64, keep: usize) -> Self {
        let _ = fs::create_dir_all(&dir);
        Self {
            dir,
            max_bytes: max_bytes.max(1),
            keep: keep.max(1),
            file: None,
            written: 0,
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    fn rotated(&self, index: usize) -> PathBuf {
        self.dir.join(format!("{FILE_NAME}.{index}"))
    }

    fn open(&mut self) {
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path())
        {
            Ok(file) => {
                self.written = file.metadata().map(|meta| meta.len()).unwrap_or(0);
                self.file = Some(file);
            }
            Err(_) => self.file = None,
        }
    }

    /// 滚动：current → .1 → .2 …，超出保留份数的归档删除
    fn rotate(&mut self) {
        self.file = None;
        if self.keep <= 1 {
            let _ = fs::remove_file(self.path());
        } else {
            let _ = fs::remove_file(self.rotated(self.keep - 1));
            for index in (1..self.keep - 1).rev() {
                let _ = fs::rename(self.rotated(index), self.rotated(index + 1));
            }
            let _ = fs::rename(self.path(), self.rotated(1));
        }
        self.written = 0;
    }

    /// 追加一行；任何失败静默忽略（no-op），绝不 panic
    pub fn info(&mut self, message: &str) {
        let line = format!("[{}] {}\n", timestamp_utc(), message);
        let len = line.len() as u64;
        if self.written > 0 && self.written + len > self.max_bytes {
            self.rotate();
        }
        if self.file.is_none() {
            self.open();
        }
        if let Some(file) = self.file.as_mut() {
            if file
                .write_all(line.as_bytes())
                .and_then(|_| file.flush())
                .is_ok()
            {
                self.written += len;
            } else {
                self.file = None;
            }
        }
    }
}

static GLOBAL: OnceLock<Mutex<RollingLog>> = OnceLock::new();

/// 进程级初始化（重复调用以首次为准）
pub fn init(dir: PathBuf, max_bytes: u64, keep: usize) {
    let _ = GLOBAL.set(Mutex::new(RollingLog::new(dir, max_bytes, keep)));
}

pub fn enabled() -> bool {
    GLOBAL.get().is_some()
}

/// 进程级写日志；未初始化或写入失败时静默 no-op
pub fn info(message: impl Display) {
    if let Some(log) = GLOBAL.get() {
        if let Ok(mut log) = log.lock() {
            log.info(&message.to_string());
        }
    }
}

fn timestamp_utc() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        now.subsec_millis()
    )
}

/// 天数（1970-01-01 起）→ 公历年月日（Howard Hinnant civil_from_days）
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
