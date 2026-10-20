//! T1：SQLite 存储层
//!
//! - 建库/迁移：`PRAGMA user_version` 版本号，后续任务加表按版本递进；
//! - `usage_records`：主键 `record_id`（uuid 单列），UPSERT 后到者胜、绝不累加（D22）；
//! - `file_checkpoints`：每文件 `(byte_offset, mtime_ms, size)` 三元组；
//! - 写入按批事务提交（默认 500 条/事务）；
//! - 「今日」按本地时区 00:00 起算：时间戳存 UTC epoch 毫秒，边界换算交给
//!   SQLite 的 `localtime`/`utc` 修饰符（DST 安全，不引第三方时区库）。
//!
//! T2 扩展（schema v2）：`profiles` + `profile_timeline` 订阅档案时间线；
//! `usage_records.profile_id` 在写入时物化归因（NULL = Unknown），查询侧直接
//! GROUP BY，无需 join。

use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags};

use super::attr::{self, ApplyOutcome, Interval, Profile, ProfileId, SettingsSnapshot, Timeline};
use super::parse::UsageRecord;
use super::CoreError;

/// 当前 schema 版本。加表/改表时递增并在 migrate 里追加版本迁移分支。
pub const SCHEMA_VERSION: i64 = 5;

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

/// v2（T2）：订阅档案与归因时间线。
/// 档案身份 = (endpoint, credential_fingerprint)；指纹为 SHA-256 前 8 位 hex，
/// 鉴权原文永不入库。时间线区间半开 `[start_ms, end_ms)`，NULL end = 仍在使用。
/// 存量 usage_records 的 profile_id 迁移后为 NULL（Unknown）。
const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS profiles (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    endpoint               TEXT NOT NULL,
    credential_fingerprint TEXT,
    display_name           TEXT NOT NULL,
    created_at             INTEGER NOT NULL,
    -- R2-2 手动额度（v0.1 纯手动口径）：NULL = 未配置，UI 不显示配额条
    quota_manual_total     INTEGER,
    quota_manual_used      INTEGER,
    quota_reset_date       TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_profiles_identity
    ON profiles(endpoint, COALESCE(credential_fingerprint, ''));

CREATE TABLE IF NOT EXISTS profile_timeline (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id INTEGER NOT NULL REFERENCES profiles(id),
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_profile_timeline_start ON profile_timeline(start_ms);

ALTER TABLE usage_records ADD COLUMN profile_id INTEGER;
CREATE INDEX IF NOT EXISTS idx_usage_records_profile ON usage_records(profile_id);
"#;

/// 后到者胜：字段整体取新值，绝不累加（D22）。
/// `profile_id` 例外：已有非 NULL 值优先保留——手动修正（T6 回填）不会被
/// 后续重扫/重放覆盖；只有从未归因（NULL）的记录才写入本次计算结果。
const UPSERT_SQL: &str = r#"
INSERT INTO usage_records (
    record_id, session_id, timestamp_ms, input_tokens, output_tokens,
    cache_read_tokens, cache_creation_tokens, model, entrypoint,
    is_sidechain, cwd, cc_version, profile_id, attribution_source
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
ON CONFLICT(record_id) DO UPDATE SET
    session_id            = excluded.session_id,
    timestamp_ms          = excluded.timestamp_ms,
    input_tokens          = excluded.input_tokens,
    output_tokens         = excluded.output_tokens,
    cache_read_tokens     = excluded.cache_read_tokens,
    cache_creation_tokens = excluded.cache_creation_tokens,
    model                 = excluded.model,
    entrypoint            = excluded.entrypoint,
    is_sidechain          = excluded.is_sidechain,
    cwd                   = excluded.cwd,
    cc_version            = excluded.cc_version,
    profile_id            = CASE
        WHEN usage_records.attribution_source = 'manual' THEN usage_records.profile_id
        WHEN excluded.attribution_source = 'proxy' THEN excluded.profile_id
        ELSE COALESCE(usage_records.profile_id, excluded.profile_id)
    END,
    attribution_source    = CASE
        WHEN usage_records.attribution_source = 'manual' THEN 'manual'
        WHEN excluded.attribution_source = 'proxy' THEN 'proxy'
        ELSE usage_records.attribution_source
    END
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

    /// 只读连接（Phase 3 command 查询用）：不跑迁移、不建目录，
    /// 避免与数据线程的初始化竞态；查询失败按 fetch 失败呈现。
    pub fn open_readonly(path: &Path) -> Result<Self, CoreError> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { conn })
    }

    /// 编辑档案显示名（T6 手动修正 UI 入口）。返回是否命中档案。
    pub fn update_profile_display(
        &mut self,
        profile_id: i64,
        display_name: &str,
    ) -> Result<bool, CoreError> {
        let changed = self.conn.execute(
            "UPDATE profiles SET display_name = ?2 WHERE id = ?1",
            params![profile_id, display_name],
        )?;
        Ok(changed > 0)
    }

    /// 确保档案存在。如果已存在 (endpoint, fingerprint) 则直接返回其 id；
    /// 否则插入新档案并返回新分配的 id。
    pub fn ensure_profile(
        &mut self,
        endpoint: &str,
        fingerprint: Option<&str>,
    ) -> Result<ProfileId, CoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM profiles
             WHERE endpoint = ?1 AND COALESCE(credential_fingerprint, '') = COALESCE(?2, '')",
        )?;
        let mut rows = stmt.query(params![endpoint, fingerprint])?;
        if let Some(row) = rows.next()? {
            return Ok(row.get(0)?);
        }
        drop(rows);
        drop(stmt);

        let display_name = attr::default_display_name(endpoint);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO profiles (endpoint, credential_fingerprint, display_name, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![endpoint, fingerprint, display_name, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 记录代理嗅探到的归因映射
    pub fn record_proxy_attribution(
        &mut self,
        message_id: &str,
        profile_id: ProfileId,
        upstream_url: Option<&str>,
    ) -> Result<(), CoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO proxy_attributions (message_id, profile_id, upstream_url, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(message_id) DO UPDATE SET
                 profile_id = excluded.profile_id,
                 upstream_url = excluded.upstream_url",
            params![message_id, profile_id, upstream_url, now],
        )?;
        Ok(())
    }

    /// 查询某条消息是否有代理精确归因记录
    pub fn get_proxy_attribution(&self, message_id: &str) -> Result<Option<ProfileId>, CoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT profile_id FROM proxy_attributions WHERE message_id = ?1")?;
        let mut rows = stmt.query(params![message_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// 晚到对账：当代理归因到达后，纠正已落库但非 manual 状态的 usage_records
    pub fn reconcile_usage_attribution(
        &mut self,
        message_id: &str,
        profile_id: ProfileId,
    ) -> Result<usize, CoreError> {
        let changed = self.conn.execute(
            "UPDATE usage_records
             SET profile_id = ?2, attribution_source = 'proxy'
             WHERE record_id = ?1 AND attribution_source != 'manual'",
            params![message_id, profile_id],
        )?;
        Ok(changed)
    }

    /// 查询指定记录的归因 profile_id 与来源（'proxy' / 'timeline' / 'manual'），测试与诊断用
    pub fn record_attribution_details(
        &self,
        record_id: &str,
    ) -> Result<Option<(Option<ProfileId>, String)>, CoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT profile_id, attribution_source FROM usage_records WHERE record_id = ?1",
        )?;
        let mut rows = stmt.query(params![record_id])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    /// 模拟用户手动修正记录归因（标记为 'manual'），测试用
    pub fn mark_record_manual(
        &mut self,
        record_id: &str,
        profile_id: Option<ProfileId>,
    ) -> Result<(), CoreError> {
        self.conn.execute(
            "UPDATE usage_records SET profile_id = ?1, attribution_source = 'manual' WHERE record_id = ?2",
            params![profile_id, record_id],
        )?;
        Ok(())
    }

    fn init(conn: Connection) -> Result<Self, CoreError> {
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL：读写分离，UI 读不被 ingest 写阻塞（T3 起会用到）
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
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
            tx.execute_batch("PRAGMA user_version = 1;")?;
            tx.commit()?;
        }
        if version < 2 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V2)?;
            tx.execute_batch("PRAGMA user_version = 2;")?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = self.conn.transaction()?;
            // D22 修订：行级 uuid 身份按 content block 放大计量。主键换
            // message.id 身份；旧表整体保留为 usage_records_legacy（已删
            // 源文件的孤儿历史不销毁，但不进任何聚合）。清空 checkpoints
            // 强制全量重扫（UPSERT 幂等）。旧表索引随 RENAME 保留原名，
            // 需先 DROP 才能为新表重建同名索引。
            tx.execute_batch(
                "DROP INDEX IF EXISTS idx_usage_records_timestamp;
                 DROP INDEX IF EXISTS idx_usage_records_session;
                 DROP INDEX IF EXISTS idx_usage_records_profile;
                 ALTER TABLE usage_records RENAME TO usage_records_legacy;
                 CREATE TABLE usage_records (
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
                     cc_version            TEXT,
                     profile_id            INTEGER REFERENCES profiles(id)
                 );
                 CREATE INDEX idx_usage_records_timestamp ON usage_records(timestamp_ms);
                 CREATE INDEX idx_usage_records_session ON usage_records(session_id);
                 CREATE INDEX idx_usage_records_profile ON usage_records(profile_id);
                 DELETE FROM file_checkpoints;
                 PRAGMA user_version = 3;",
            )?;
            tx.commit()?;
        }
        if version < 4 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS proxy_attributions (
                     message_id   TEXT PRIMARY KEY NOT NULL,
                     profile_id   INTEGER NOT NULL REFERENCES profiles(id),
                     upstream_url TEXT,
                     created_at   INTEGER NOT NULL
                 );
                 ALTER TABLE usage_records ADD COLUMN attribution_source TEXT NOT NULL DEFAULT 'timeline';
                 CREATE INDEX IF NOT EXISTS idx_usage_records_attr_source ON usage_records(attribution_source);
                 PRAGMA user_version = 4;",
            )?;
            tx.commit()?;
        }
        if version < 5 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "ALTER TABLE profiles ADD COLUMN quota_type TEXT NOT NULL DEFAULT 'manual';
                 ALTER TABLE profiles ADD COLUMN quota_unit TEXT NOT NULL DEFAULT 'usd';
                 ALTER TABLE profiles ADD COLUMN quota_api_url TEXT;
                 ALTER TABLE profiles ADD COLUMN quota_api_key TEXT;
                 ALTER TABLE profiles ADD COLUMN quota_total REAL;
                 ALTER TABLE profiles ADD COLUMN quota_used REAL;
                 ALTER TABLE profiles ADD COLUMN quota_updated_at INTEGER;
                 ALTER TABLE profiles ADD COLUMN quota_sync_status TEXT;
                 ALTER TABLE profiles ADD COLUMN quota_sync_error TEXT;

                 UPDATE profiles
                 SET quota_total = quota_manual_total,
                     quota_used = quota_manual_used,
                     quota_unit = 'tokens'
                 WHERE quota_manual_total IS NOT NULL AND quota_total IS NULL;

                 PRAGMA user_version = 5;",
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// T7：聚合层只读访问连接。聚合 SQL 留在 agg 模块内，
    /// 避免存储层混入展示形状（Dashboard 查询不向 store.rs 扩散）。
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 批量 UPSERT 用量记录。同 record_id 后到者胜，绝不累加。
    /// 每 [`UPSERT_BATCH`] 条一个事务，避免长事务锁库与崩溃丢整批。
    /// 落库前按当前时间线物化归因（T2）：`profile_id` 为 NULL 即 Unknown。
    pub fn upsert_records(&mut self, records: &[UsageRecord]) -> Result<(), CoreError> {
        // 时间线很小，每批加载一次内存查询即可；避免逐条 SQL 查询
        let timeline = self.load_timeline()?;
        let codex_profile_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM profiles
                 WHERE endpoint LIKE '%openai.com%'
                    OR endpoint LIKE '%15721%'
                    OR display_name LIKE '%OpenAI%'
                    OR display_name LIKE '%Codex%'
                 ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();
        for chunk in records.chunks(UPSERT_BATCH) {
            let tx = self.conn.transaction()?;
            {
                let mut stmt = tx.prepare_cached(UPSERT_SQL)?;
                let mut proxy_stmt = tx.prepare_cached(
                    "SELECT profile_id FROM proxy_attributions WHERE message_id = ?1",
                )?;
                for r in chunk {
                    let proxy_id: Option<i64> = proxy_stmt
                        .query_row(params![&r.record_id], |row| row.get(0))
                        .ok();
                    let is_codex = r.record_id.starts_with("codex:");
                    let (profile_id, attr_source) = if let Some(pid) = proxy_id {
                        (Some(pid), "proxy")
                    } else if is_codex {
                        (codex_profile_id, "config")
                    } else {
                        (attr::attribute_at(&timeline, r.timestamp_ms), "timeline")
                    };
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
                        profile_id,
                        attr_source,
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

    // -----------------------------------------------------------------------
    // T2：订阅档案时间线 + 归因物化
    // -----------------------------------------------------------------------

    /// 所有档案，按 id 升序（T6 手动改名 UI 使用）
    pub fn profiles(&self) -> Result<Vec<Profile>, CoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, endpoint, credential_fingerprint, display_name
             FROM profiles ORDER BY id",
        )?;
        let profiles = stmt
            .query_map([], |row| {
                Ok(Profile {
                    id: row.get(0)?,
                    endpoint: row.get(1)?,
                    credential_fingerprint: row.get(2)?,
                    display_name: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(profiles)
    }

    /// 所有时间线区间，按 `start_ms` 升序
    pub fn timeline_intervals(&self) -> Result<Vec<Interval>, CoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, profile_id, start_ms, end_ms
             FROM profile_timeline ORDER BY start_ms, id",
        )?;
        let intervals = stmt
            .query_map([], |row| {
                Ok(Interval {
                    id: Some(row.get(0)?),
                    profile_id: row.get(1)?,
                    start_ms: row.get(2)?,
                    end_ms: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(intervals)
    }

    /// 从数据库重建内存时间线（契约决策在 attr 层，store 只负责持久化）
    pub fn load_timeline(&self) -> Result<Timeline, CoreError> {
        Ok(Timeline::new(self.profiles()?, self.timeline_intervals()?))
    }

    /// 应用一次 settings 快照并持久化时间线变化。
    /// 纯决策逻辑在 [`attr::apply_snapshot`]；这里只落库 diff。
    pub fn apply_settings_snapshot(
        &mut self,
        snapshot: &SettingsSnapshot,
        at_ms: i64,
    ) -> Result<ApplyOutcome, CoreError> {
        let mut timeline = self.load_timeline()?;
        let outcome = attr::apply_snapshot(&mut timeline, snapshot, at_ms);
        if let ApplyOutcome::Changed {
            created_profile,
            closed_interval,
            opened_interval,
        } = &outcome
        {
            let tx = self.conn.transaction()?;
            if let Some(profile) = created_profile {
                tx.execute(
                    "INSERT INTO profiles
                         (id, endpoint, credential_fingerprint, display_name, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        profile.id,
                        profile.endpoint,
                        profile.credential_fingerprint,
                        profile.display_name,
                        at_ms
                    ],
                )?;
            }
            if let Some((id, end_ms)) = closed_interval
                .as_ref()
                .and_then(|iv| iv.id.map(|id| (id, iv.end_ms)))
            {
                tx.execute(
                    "UPDATE profile_timeline SET end_ms = ?1 WHERE id = ?2",
                    params![end_ms, id],
                )?;
            }
            if let Some(interval) = opened_interval {
                tx.execute(
                    "INSERT INTO profile_timeline (profile_id, start_ms, end_ms)
                     VALUES (?1, ?2, NULL)",
                    params![interval.profile_id, interval.start_ms],
                )?;
            }
            tx.commit()?;
        }
        Ok(outcome)
    }

    /// 归因查询：`ts_ms` 时刻属于哪个档案。
    /// 内存实现：时间线很小，加载后复用 [`attr::attribute_at`]，
    /// 保证写入物化与查询侧共用同一套宽限期语义。
    pub fn attribute_at(&self, ts_ms: i64) -> Result<Option<ProfileId>, CoreError> {
        Ok(attr::attribute_at(&self.load_timeline()?, ts_ms))
    }

    /// 手动修正（T6 接线）：把 `[start_ms, end_ms)` 内所有 usage 的归因
    /// 改写为 `profile_id`（None = Unknown）。返回受影响行数。
    /// 手动值在后续重扫时被 UPSERT 保留（见 `UPSERT_SQL`）。
    pub fn reassign_profile_range(
        &mut self,
        profile_id: Option<ProfileId>,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<u64, CoreError> {
        let affected = self.conn.execute(
            "UPDATE usage_records SET profile_id = ?1
             WHERE timestamp_ms >= ?2 AND timestamp_ms < ?3",
            params![profile_id, start_ms, end_ms],
        )?;
        Ok(affected as u64)
    }

    /// 手动额度配置（R2-2，T5/T6 接线）：整体覆盖三列，`None` = 清除该项。
    /// `quota_manual_used` 只存用户输入，不从统计累加；`quota_reset_date`
    /// 为 ISO 日期字符串，仅用于倒计时展示，到期不自动清零。
    /// 返回是否命中档案（false = `profile_id` 不存在）。
    pub fn update_profile_quota(
        &mut self,
        profile_id: ProfileId,
        total: Option<i64>,
        used: Option<i64>,
        reset_date: Option<&str>,
    ) -> Result<bool, CoreError> {
        let affected = self.conn.execute(
            "UPDATE profiles
             SET quota_manual_total = ?1,
                 quota_manual_used = ?2,
                 quota_reset_date = ?3,
                 quota_total = ?1,
                 quota_used = ?2,
                 quota_type = 'manual',
                 quota_unit = 'tokens'
             WHERE id = ?4",
            params![total, used, reset_date, profile_id],
        )?;
        Ok(affected > 0)
    }
    /// 更新档案配额配置（v0.2 自动配额单一真源写入口）
    #[allow(clippy::too_many_arguments)]
    pub fn update_profile_quota_config(
        &mut self,
        profile_id: ProfileId,
        quota_type: &str,
        quota_unit: &str,
        quota_api_url: Option<&str>,
        quota_api_key: Option<&str>,
        manual_total: Option<f64>,
        manual_used: Option<f64>,
        reset_date: Option<&str>,
    ) -> Result<bool, CoreError> {
        let quota_type_clean = if quota_type == "api" { "api" } else { "manual" };
        let unit_clean = if quota_unit == "tokens" || quota_unit == "cny" {
            quota_unit
        } else {
            "usd"
        };

        let total_val = if quota_type_clean == "manual" {
            manual_total
        } else {
            None
        };
        let used_val = if quota_type_clean == "manual" {
            manual_used
        } else {
            None
        };
        let manual_total_int = manual_total.map(|v| v as i64);
        let manual_used_int = manual_used.map(|v| v as i64);

        let affected = self.conn.execute(
            "UPDATE profiles
             SET quota_type = ?1,
                 quota_unit = ?2,
                 quota_api_url = ?3,
                 quota_api_key = COALESCE(?4, quota_api_key),
                 quota_total = CASE WHEN ?1 = 'manual' THEN ?5 ELSE quota_total END,
                 quota_used = CASE WHEN ?1 = 'manual' THEN ?6 ELSE quota_used END,
                 quota_reset_date = ?7,
                 quota_manual_total = ?8,
                 quota_manual_used = ?9,
                 quota_sync_status = CASE WHEN ?1 = 'manual' THEN NULL ELSE quota_sync_status END,
                 quota_sync_error = CASE WHEN ?1 = 'manual' THEN NULL ELSE quota_sync_error END
             WHERE id = ?10",
            params![
                quota_type_clean,
                unit_clean,
                quota_api_url,
                quota_api_key,
                total_val,
                used_val,
                reset_date,
                manual_total_int,
                manual_used_int,
                profile_id,
            ],
        )?;
        Ok(affected > 0)
    }

    /// 保存后台或手动 API 配额拉取结果（带在途写入保护：仅当 quota_type = 'api' 时更新）
    #[allow(clippy::too_many_arguments)]
    pub fn save_profile_quota_sync_result(
        &mut self,
        profile_id: ProfileId,
        total: Option<f64>,
        used: Option<f64>,
        reset_date: Option<&str>,
        sync_status: &str,
        sync_error: Option<&str>,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        let affected = if sync_status == "ok" {
            self.conn.execute(
                "UPDATE profiles
                 SET quota_total = ?1,
                     quota_used = ?2,
                     quota_reset_date = COALESCE(?3, quota_reset_date),
                     quota_sync_status = ?4,
                     quota_sync_error = NULL,
                     quota_updated_at = ?5
                 WHERE id = ?6 AND quota_type = 'api'",
                params![total, used, reset_date, sync_status, now_ms, profile_id],
            )?
        } else {
            self.conn.execute(
                "UPDATE profiles
                 SET quota_sync_status = ?1,
                     quota_sync_error = ?2,
                     quota_updated_at = ?3
                 WHERE id = ?4 AND quota_type = 'api'",
                params![sync_status, sync_error, now_ms, profile_id],
            )?
        };
        Ok(affected > 0)
    }

    /// 列出所有启用了 API 自动同步的档案 ID
    pub fn list_api_quota_profiles(&self) -> Result<Vec<ProfileId>, CoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM profiles WHERE quota_type = 'api'",
        )?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 每条用量记录的归因（record_id → profile_id），按时间排序；测试/诊断用。
    pub fn record_profiles(&self) -> Result<Vec<(String, Option<ProfileId>)>, CoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT record_id, profile_id FROM usage_records ORDER BY timestamp_ms, record_id",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
