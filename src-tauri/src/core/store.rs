//! T1：SQLite 存储层
//!
//! - 建库/迁移：`PRAGMA user_version` 版本号，后续任务加表按版本递进；
//! - `usage_records`：主键 `record_id`（uuid 单列），UPSERT REPLACE 语义、绝不累加（D22）；
//! - `file_checkpoints`：每文件 `(byte_offset, mtime_ms, size)` 三元组；
//! - 写入按批事务提交（默认 500 条/事务）；
//! - 「今日」按本地时区 00:00 起算：时间戳存 UTC epoch 毫秒，边界换算交给
//!   SQLite 的 `localtime`/`utc` 修饰符（DST 安全，不引第三方时区库）。

use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection};

use super::parse::UsageRecord;
use super::CoreError;

/// 当前 schema 版本。加表/改表时递增并在 migrate 里追加版本迁移分支。
pub const SCHEMA_VERSION: i64 = 1;

/// 每条 SQL 事务提交的用量记录条数
pub const UPSERT_BATCH: usize = 500;

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS usage_records (
    record_id             TEXT PRIMARY KEY NOT NULL,
    session_id            TEXT NOT NULL,
    timestamp_ms          INTEGER NOT NULL,
    input_tokens          INTEGER NOT NULL,
    output_tokens         INTEGER NOT NULL,
    cache_read_tokens     INTEGER NOT NULL,
    cache_creation_tokens INTEGER NOT NULL,
    model                 TEXT NOT NULL,
    entrypoint            TEXT NOT NULL,
    is_sidechain          INTEGER NOT NULL,
    cwd                   TEXT,
    cc_version            TEXT
);
CREATE INDEX IF NOT EXISTS idx_usage_records_timestamp ON usage_records(timestamp_ms);
CREATE INDEX IF NOT EXISTS idx_usage_records_session ON usage_records(session_id);

CREATE TABLE IF NOT EXISTS file_checkpoints (
    file_path   TEXT PRIMARY KEY NOT NULL,
    byte_offset INTEGER NOT NULL,
    mtime_ms    INTEGER NOT NULL,
    size        INTEGER NOT NULL
);
"#;

const UPSERT_SQL: &str = r#"
INSERT OR REPLACE INTO usage_records (
    record_id, session_id, timestamp_ms, input_tokens, output_tokens,
    cache_read_tokens, cache_creation_tokens, model, entrypoint,
    is_sidechain, cwd, cc_version
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
"#;

const TOTALS_SQL: &str = r#"
SELECT COALESCE(SUM(input_tokens), 0),
       COALESCE(SUM(output_tokens), 0),
       COALESCE(SUM(cache_read_tokens), 0),
       COALESCE(SUM(cache_creation_tokens), 0)
FROM usage_records
WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2
"#;

/// 本地日边界换算：把「本地日历日 00:00」换算为 UTC epoch 毫秒。
/// `'utc'` 修饰符 = 入参按本地时间解释、转成 UTC；次日先按日历加一天再换算，
/// 避免在夏令时切换日用 +24h 造成的偏移误差。
const LOCAL_DAY_BOUNDS_SQL: &str = r#"
SELECT CAST(strftime('%s', ?1 || ' 00:00:00', 'utc') AS INTEGER) * 1000,
       CAST(strftime('%s', date(?1, '+1 day') || ' 00:00:00', 'utc') AS INTEGER) * 1000
"#;

/// 每文件增量读取的恢复点
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    /// 指向最后一个完整行末尾（不完整尾行不计入）
    pub byte_offset: i64,
    pub mtime_ms: i64,
    pub size: i64,
}

/// 四项 token 的聚合结果
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
}

impl Totals {
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// 打开（必要时创建）数据库文件并完成迁移
    pub fn open(path: &Path) -> Result<Self, CoreError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        Self::init(Connection::open(path)?)
    }

    /// 内存库（测试用）
    pub fn open_in_memory() -> Result<Self, CoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, CoreError> {
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL：读写分离，UI 读不被 ingest 写阻塞（T3 起会用到）
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), CoreError> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(CoreError::Other(format!(
                "数据库 schema 版本 {version} 高于程序支持的 {SCHEMA_VERSION}"
            )));
        }
        if version < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V1)?;
            tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
            tx.commit()?;
        }
        Ok(())
    }

    /// 批量 UPSERT 用量记录。同 record_id 后到者胜（REPLACE），绝不累加。
    /// 每 [`UPSERT_BATCH`] 条一个事务，避免长事务锁库与崩溃丢整批。
    pub fn upsert_records(&mut self, records: &[UsageRecord]) -> Result<(), CoreError> {
        for chunk in records.chunks(UPSERT_BATCH) {
            let tx = self.conn.transaction()?;
            {
                let mut stmt = tx.prepare_cached(UPSERT_SQL)?;
                for r in chunk {
                    stmt.execute(params![
                        r.record_id,
                        r.session_id,
                        r.timestamp_ms,
                        r.input_tokens,
                        r.output_tokens,
                        r.cache_read_tokens,
                        r.cache_creation_tokens,
                        r.model,
                        r.entrypoint,
                        r.is_sidechain as i64,
                        r.cwd,
                        r.cc_version,
                    ])?;
                }
            }
            tx.commit()?;
        }
        Ok(())
    }

    pub fn get_checkpoint(&self, file_path: &str) -> Result<Option<Checkpoint>, CoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT byte_offset, mtime_ms, size FROM file_checkpoints WHERE file_path = ?1",
        )?;
        let mut rows = stmt.query([file_path])?;
        match rows.next()? {
            Some(row) => Ok(Some(Checkpoint {
                byte_offset: row.get(0)?,
                mtime_ms: row.get(1)?,
                size: row.get(2)?,
            })),
            None => Ok(None),
        }
    }

    pub fn set_checkpoint(
        &mut self,
        file_path: &str,
        checkpoint: Checkpoint,
    ) -> Result<(), CoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO file_checkpoints (file_path, byte_offset, mtime_ms, size)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                file_path,
                checkpoint.byte_offset,
                checkpoint.mtime_ms,
                checkpoint.size
            ],
        )?;
        Ok(())
    }

    /// 统计 `[start_ms, end_ms)` 区间的四项 token 之和
    pub fn totals_between(&self, start_ms: i64, end_ms: i64) -> Result<Totals, CoreError> {
        let mut stmt = self.conn.prepare_cached(TOTALS_SQL)?;
        let totals = stmt.query_row(params![start_ms, end_ms], |row| {
            Ok(Totals {
                input: row.get(0)?,
                output: row.get(1)?,
                cache_read: row.get(2)?,
                cache_creation: row.get(3)?,
            })
        })?;
        Ok(totals)
    }

    /// 本地日历日 `YYYY-MM-DD` 的 UTC 毫秒区间 `[start, end)`
    pub fn local_day_bounds(&self, local_date: &str) -> Result<(i64, i64), CoreError> {
        let bounds = self
            .conn
            .query_row(LOCAL_DAY_BOUNDS_SQL, [local_date], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
        Ok(bounds)
    }

    /// 当前本地日历日 `YYYY-MM-DD`
    pub fn local_today(&self) -> Result<String, CoreError> {
        let today = self
            .conn
            .query_row("SELECT date('now', 'localtime')", [], |row| row.get(0))?;
        Ok(today)
    }

    /// 指定本地日历日的四项 token 之和
    pub fn totals_local_day(&self, local_date: &str) -> Result<Totals, CoreError> {
        let (start_ms, end_ms) = self.local_day_bounds(local_date)?;
        self.totals_between(start_ms, end_ms)
    }

    /// 本地「今日」四项 token 之和（今天 = 本地 00:00 起）
    pub fn today_totals(&self) -> Result<Totals, CoreError> {
        let today = self.local_today()?;
        self.totals_local_day(&today)
    }

    /// 库中记录数（测试/诊断用）
    pub fn count_records(&self) -> Result<i64, CoreError> {
        let count = self
            .conn
            .query_row("SELECT COUNT(*) FROM usage_records", [], |row| row.get(0))?;
        Ok(count)
    }
}
