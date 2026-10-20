//! 数据引擎核心：解析 / 存储 / 监听 / 归因 / 聚合
//!
//! 模块随 T1-T7 逐步落地；数据源契约见 docs/data-source/claude-code-jsonl.md，
//! 设计决策（D14-D23）见 docs/designs/token-watcher-design-v1.1.md。

pub mod agg;
pub mod attr;
pub mod ingest;
pub mod parse;
pub mod store;
pub mod pricing;

pub mod quota;
use std::fmt;

/// core 层统一错误：IO / SQLite / 其他
#[derive(Debug)]
pub enum CoreError {
    Io(std::io::Error),
    Db(rusqlite::Error),
    Other(String),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::Io(e) => write!(f, "IO 错误: {e}"),
            CoreError::Db(e) => write!(f, "数据库错误: {e}"),
            CoreError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CoreError::Io(e) => Some(e),
            CoreError::Db(e) => Some(e),
            CoreError::Other(_) => None,
        }
    }
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Io(e)
    }
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        CoreError::Db(e)
    }
}
