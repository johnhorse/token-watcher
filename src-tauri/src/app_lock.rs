//! T4：写者互斥锁（单一 ingestion owner，F5）
//!
//! GUI 与 tw-ingest 双 bin 并存，但任一时刻只允许一个进程写库
//! （ingest、settings apply、档案分配都在这把锁之内）。单实例插件（D15）
//! 只负责窗口体验；本锁负责数据安全，两者并存。
//!
//! 实现：`~/.token-watcher/owner.lock` 上的排他文件锁，使用
//! `std::fs::File::try_lock`（Rust 1.89+ 稳定），不引入额外依赖。
//! 锁随文件句柄关闭自动释放；进程崩溃时由内核释放，不会留下死锁。

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 默认锁文件路径：`~/.token-watcher/owner.lock`
pub fn default_lock_path() -> io::Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| io::Error::other("无法定位用户 home 目录"))?;
    Ok(home.join(".token-watcher").join("owner.lock"))
}

#[derive(Debug)]
pub enum LockError {
    /// 已被其他进程/句柄持有
    Busy,
    Io(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Busy => write!(f, "写者锁已被占用"),
            LockError::Io(e) => write!(f, "写者锁 IO 错误: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// 持有的写者锁。Drop 时释放。
pub struct AppLock {
    file: File,
    path: PathBuf,
}

impl AppLock {
    /// 尝试获取写者锁。拿不到（已被持有）返回 [`LockError::Busy`]。
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir).map_err(LockError::Io)?;
            }
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LockError::Busy),
            Err(TryLockError::Error(e)) => return Err(LockError::Io(e)),
        }
        // 写入持有者 pid 作为诊断信息（拿不到锁的进程可读它定位冲突来源）
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = writeln!(file, "pid={}", std::process::id());
        let _ = file.flush();
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// 获取默认路径（`~/.token-watcher/owner.lock`）的写者锁
    pub fn acquire_default() -> Result<Self, LockError> {
        let path = default_lock_path().map_err(LockError::Io)?;
        Self::acquire(&path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AppLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// 读取锁文件里记录的持有者 pid（拿不到锁时用于提示文案）。
pub fn read_owner_pid(path: &Path) -> Option<u32> {
    let mut text = String::new();
    File::open(path).ok()?.read_to_string(&mut text).ok()?;
    text.trim().strip_prefix("pid=")?.parse::<u32>().ok()
}
