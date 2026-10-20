//! T2 集成测试：settings 快照 → 档案时间线 → usage 归因物化
//!
//! 覆盖验收场景：
//! 1. 装前历史（timeline 空）→ 全部 Unknown
//! 2. 单次切换：切换前旧档案；切换时刻起 300s 宽限内 Unknown；宽限后新档案
//! 3. A→B→A 交替：两个档案、三段区间、两段宽限 Unknown
//! 4. 同 endpoint 换指纹 = 新档案（推断局限的诚实呈现）
//! 5. env 缺 BASE_URL / 坏 JSON：坏 JSON 保持上一态；无 BASE_URL 进入 Unknown
//! 6. 指纹不入库：全库 dump 不含鉴权原文
//! 7. 回填函数：区间内改写、区间外不动、半开边界、重放不覆盖手动修正
//! 8. ingest 路径物化：fixture JSONL + fixture 时间线 → 逐条 profile_id 正确

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::types::ValueRef;
use serde_json::json;
use token_watcher_lib::core::attr::{
    fingerprint, parse_settings, ApplyOutcome, SettingsSnapshot, GRACE_MS,
};
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::parse::{parse_line, ParsedLine};
use token_watcher_lib::core::store::Store;

/// 自建临时目录（沿用 T1 做法，避免新增依赖）
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tw-t2-{}-{nanos}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const T0: i64 = 1_700_000_000_000;
const HOUR: i64 = 3_600_000;
const MINUTE: i64 = 60_000;

/// epoch 毫秒 → ISO-8601 UTC 字符串（借 SQLite 格式化，与 T1 相同）
fn iso_utc(ms: i64) -> String {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1 / 1000.0, 'unixepoch')",
        [ms],
        |row| row.get(0),
    )
    .unwrap()
}

fn settings_json(base_url: Option<&str>, token: Option<&str>) -> String {
    let mut env = serde_json::Map::new();
    if let Some(url) = base_url {
        env.insert("ANTHROPIC_BASE_URL".to_string(), json!(url));
    }
    if let Some(token) = token {
        env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), json!(token));
    }
    json!({ "env": env }).to_string()
}

/// 应用一次 settings 快照
fn apply(
    store: &mut Store,
    base_url: Option<&str>,
    token: Option<&str>,
    at_ms: i64,
) -> ApplyOutcome {
    store
        .apply_settings_snapshot(&parse_settings(&settings_json(base_url, token)), at_ms)
        .unwrap()
}

/// 构造一行 assistant 用量记录
fn usage_line(uuid: &str, ts_ms: i64, input: i64, output: i64) -> String {
    json!({
        "type": "assistant",
        "uuid": uuid,
        "session_id": format!("session-{uuid}"),
        "sessionId": format!("session-{uuid}"),
        "timestamp": iso_utc(ts_ms),
        "message": {
            "model": "glm-5.3",
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0
            }
        }
    })
    .to_string()
}

fn ingest_lines(store: &mut Store, dir: &TempDir, name: &str, lines: &[String]) {
    let path = dir.path().join(name);
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    ingest::ingest_file(store, &path).unwrap();
}

fn profile_of(store: &Store, record_id: &str) -> Option<i64> {
    store
        .record_profiles()
        .unwrap()
        .into_iter()
        .find(|(id, _)| id == record_id)
        .unwrap_or_else(|| panic!("记录 {record_id} 不存在"))
        .1
}

// 1. 装前历史：时间线为空 → 全部 Unknown
#[test]
fn pre_install_history_is_unknown() {
    let mut store = Store::open_in_memory().unwrap();
    let dir = TempDir::new();
    ingest_lines(
        &mut store,
        &dir,
        "history.jsonl",
        &[
            usage_line("pre-1", T0 - 2 * HOUR, 10, 1),
            usage_line("pre-2", T0 - HOUR, 20, 2),
        ],
    );

    assert_eq!(store.count_records().unwrap(), 2);
    assert!(store.profiles().unwrap().is_empty());
    assert!(store.timeline_intervals().unwrap().is_empty());
    for (record_id, profile_id) in store.record_profiles().unwrap() {
        assert_eq!(profile_id, None, "{record_id} 应归 Unknown");
    }
    assert_eq!(store.attribute_at(T0 - HOUR).unwrap(), None);
}

// 2. 单次切换：旧档案 → 300s 宽限 Unknown → 新档案
#[test]
fn single_switch_has_grace_period() {
    let mut store = Store::open_in_memory().unwrap();
    let switch_at = T0 + HOUR;

    let first = apply(
        &mut store,
        Some("https://api.deepseek.com"),
        Some("sk-a"),
        T0,
    );
    assert!(matches!(
        first,
        ApplyOutcome::Changed {
            created_profile: Some(_),
            closed_interval: None,
            opened_interval: Some(_),
        }
    ));
    let a_id = store.profiles().unwrap()[0].id;

    let second = apply(
        &mut store,
        Some("https://openrouter.ai/api"),
        Some("sk-b"),
        switch_at,
    );
    match second {
        ApplyOutcome::Changed {
            created_profile,
            closed_interval,
            opened_interval,
        } => {
            assert!(created_profile.is_some());
            assert_eq!(closed_interval.unwrap().end_ms, Some(switch_at));
            assert_eq!(opened_interval.unwrap().start_ms, switch_at);
        }
        other => panic!("预期切换事件，得到 {other:?}"),
    }
    let b_id = store.profiles().unwrap()[1].id;
    assert_ne!(a_id, b_id);
}

// 2（续）. 时间点断言集中在一个用例里
#[test]
fn single_switch_attribution_boundaries() {
    let mut store = Store::open_in_memory().unwrap();
    let switch_at = T0 + HOUR;
    apply(
        &mut store,
        Some("https://api.deepseek.com"),
        Some("sk-a"),
        T0,
    );
    let a_id = store.profiles().unwrap()[0].id;
    apply(
        &mut store,
        Some("https://openrouter.ai/api"),
        Some("sk-b"),
        switch_at,
    );
    let b_id = store.profiles().unwrap()[1].id;

    let dir = TempDir::new();
    ingest_lines(
        &mut store,
        &dir,
        "switch.jsonl",
        &[
            usage_line("a-period", T0 + 10 * MINUTE, 1, 0),
            usage_line("just-before", switch_at - 1, 2, 0),
            usage_line("at-switch", switch_at, 3, 0),
            usage_line("grace-last-ms", switch_at + GRACE_MS - 1, 4, 0),
            usage_line("grace-over", switch_at + GRACE_MS, 5, 0),
        ],
    );

    assert_eq!(profile_of(&store, "a-period"), Some(a_id));
    assert_eq!(profile_of(&store, "just-before"), Some(a_id));
    assert_eq!(profile_of(&store, "at-switch"), None, "切换瞬间归 Unknown");
    assert_eq!(
        profile_of(&store, "grace-last-ms"),
        None,
        "宽限期最后一毫秒仍 Unknown"
    );
    assert_eq!(
        profile_of(&store, "grace-over"),
        Some(b_id),
        "宽限期结束归新档案"
    );

    // store 查询接口与写入物化语义一致
    assert_eq!(store.attribute_at(switch_at + GRACE_MS - 1).unwrap(), None);
    assert_eq!(
        store.attribute_at(switch_at + GRACE_MS).unwrap(),
        Some(b_id)
    );
}

// 3. A→B→A：两个档案、三段区间、两段宽限 Unknown，A 复用同一持久 ID
#[test]
fn alternating_profiles_create_separate_intervals() {
    let mut store = Store::open_in_memory().unwrap();
    let t1 = T0 + HOUR;
    let t2 = T0 + 2 * HOUR;

    apply(&mut store, Some("https://a.example.com"), Some("sk-a"), T0);
    apply(&mut store, Some("https://b.example.com"), Some("sk-b"), t1);
    apply(&mut store, Some("https://a.example.com"), Some("sk-a"), t2);

    let profiles = store.profiles().unwrap();
    assert_eq!(profiles.len(), 2, "A 去而复返不新建档案");
    let a_id = profiles[0].id;
    let b_id = profiles[1].id;

    let intervals = store.timeline_intervals().unwrap();
    assert_eq!(intervals.len(), 3);
    assert_eq!(
        (
            intervals[0].profile_id,
            intervals[0].start_ms,
            intervals[0].end_ms
        ),
        (a_id, T0, Some(t1))
    );
    assert_eq!(
        (
            intervals[1].profile_id,
            intervals[1].start_ms,
            intervals[1].end_ms
        ),
        (b_id, t1, Some(t2))
    );
    assert_eq!(
        (
            intervals[2].profile_id,
            intervals[2].start_ms,
            intervals[2].end_ms
        ),
        (a_id, t2, None)
    );

    let dir = TempDir::new();
    ingest_lines(
        &mut store,
        &dir,
        "alternating.jsonl",
        &[
            usage_line("a1", T0 + 10 * MINUTE, 1, 0),
            usage_line("b-grace", t1 + MINUTE, 2, 0),
            usage_line("b1", t1 + 10 * MINUTE, 3, 0),
            usage_line("a2-grace", t2 + MINUTE, 4, 0),
            usage_line("a2", t2 + 10 * MINUTE, 5, 0),
        ],
    );

    assert_eq!(profile_of(&store, "a1"), Some(a_id));
    assert_eq!(profile_of(&store, "b-grace"), None);
    assert_eq!(profile_of(&store, "b1"), Some(b_id));
    assert_eq!(profile_of(&store, "a2-grace"), None);
    assert_eq!(profile_of(&store, "a2"), Some(a_id));
}

// 4. 同 endpoint 不同指纹 = 新档案（无法区分换 key 与同址双订阅，诚实拆档）
#[test]
fn same_endpoint_different_fingerprint_is_a_new_profile() {
    let mut store = Store::open_in_memory().unwrap();
    let endpoint = "https://api.deepseek.com";
    let t1 = T0 + HOUR;
    apply(&mut store, Some(endpoint), Some("sk-first"), T0);
    apply(&mut store, Some(endpoint), Some("sk-second"), t1);

    let profiles = store.profiles().unwrap();
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].endpoint, profiles[1].endpoint);
    assert_eq!(
        profiles[0].credential_fingerprint.as_deref(),
        Some(fingerprint("sk-first").as_str())
    );
    assert_eq!(
        profiles[1].credential_fingerprint.as_deref(),
        Some(fingerprint("sk-second").as_str())
    );
    assert_ne!(profiles[0].id, profiles[1].id);
    // 指纹是 8 位 hex
    for p in &profiles {
        let fp = p.credential_fingerprint.as_deref().unwrap();
        assert_eq!(fp.len(), 8);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    let dir = TempDir::new();
    ingest_lines(
        &mut store,
        &dir,
        "fingerprint.jsonl",
        &[
            usage_line("first", T0 + 10 * MINUTE, 1, 0),
            usage_line("second", t1 + 10 * MINUTE, 2, 0),
        ],
    );
    assert_eq!(profile_of(&store, "first"), Some(profiles[0].id));
    assert_eq!(profile_of(&store, "second"), Some(profiles[1].id));
}

// 5. 坏 JSON 保持上一态；有效 JSON 无 BASE_URL = 该时刻无归因信息（Unknown）
#[test]
fn unreadable_and_no_attribution_snapshots() {
    let mut store = Store::open_in_memory().unwrap();

    // (a) 时间线为空：两种快照都不产生档案/区间
    let bad = parse_settings("{ not json");
    assert_eq!(bad, SettingsSnapshot::Unreadable);
    assert_eq!(
        store.apply_settings_snapshot(&bad, T0).unwrap(),
        ApplyOutcome::Unreadable
    );
    assert!(store.timeline_intervals().unwrap().is_empty());

    let no_attr = parse_settings(&settings_json(None, Some("sk-x")));
    assert_eq!(no_attr, SettingsSnapshot::NoAttribution);
    assert_eq!(
        store.apply_settings_snapshot(&no_attr, T0 + 1).unwrap(),
        ApplyOutcome::NoChange,
        "无区间可关：时间线不变"
    );
    assert!(store.profiles().unwrap().is_empty());

    // (b) 有开放区间：坏 JSON 不得关闭区间（保持上一个已知态）
    let open_at = T0 + HOUR;
    apply(
        &mut store,
        Some("https://a.example.com"),
        Some("sk-a"),
        open_at,
    );
    let before = store.timeline_intervals().unwrap();
    assert_eq!(
        store
            .apply_settings_snapshot(&parse_settings("{ broken"), open_at + 1_000)
            .unwrap(),
        ApplyOutcome::Unreadable
    );
    assert_eq!(
        store.timeline_intervals().unwrap(),
        before,
        "坏 JSON 不产生时间线事件"
    );

    // (c) 有效 JSON 但缺 BASE_URL：该时刻起无归因信息 → 关闭旧区间
    let close_at = open_at + 2 * HOUR;
    match store.apply_settings_snapshot(&no_attr, close_at).unwrap() {
        ApplyOutcome::Changed {
            created_profile: None,
            closed_interval: Some(interval),
            opened_interval: None,
        } => assert_eq!(interval.end_ms, Some(close_at)),
        other => panic!("预期关闭区间，得到 {other:?}"),
    }

    let dir = TempDir::new();
    ingest_lines(
        &mut store,
        &dir,
        "no-attribution.jsonl",
        &[
            usage_line("still-a", open_at + 10 * MINUTE, 1, 0),
            usage_line("unknown-after-close", close_at + 1_000, 2, 0),
        ],
    );
    assert_eq!(
        profile_of(&store, "still-a"),
        Some(store.profiles().unwrap()[0].id)
    );
    assert_eq!(profile_of(&store, "unknown-after-close"), None);
}

// 6. CRITICAL：鉴权原文永不落库，只能看到 8 位指纹
#[test]
fn credential_secret_is_never_persisted() {
    const SECRET: &str = "sk-super-secret-token-do-not-persist-0xC0FFEE";
    let dir = TempDir::new();
    let db_path = dir.path().join("attr.sqlite3");

    {
        let mut store = Store::open(&db_path).unwrap();
        apply(
            &mut store,
            Some("https://api.deepseek.com"),
            Some(SECRET),
            T0,
        );
        let ingest_dir = TempDir::new();
        ingest_lines(
            &mut store,
            &ingest_dir,
            "one.jsonl",
            &[usage_line("r1", T0 + 10 * MINUTE, 1, 0)],
        );
        assert_eq!(
            store.profiles().unwrap()[0]
                .credential_fingerprint
                .as_deref(),
            Some(fingerprint(SECRET).as_str())
        );
    } // 关闭连接，确保数据落盘

    // 全库 dump：所有表、所有列都字符串化后扫描
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!tables.is_empty(), "库中应有业务表");

    let mut dump = String::new();
    for table in &tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let column_count = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..column_count {
                let text = match row.get_ref(i).unwrap() {
                    ValueRef::Null => "NULL".to_string(),
                    ValueRef::Integer(v) => v.to_string(),
                    ValueRef::Real(v) => v.to_string(),
                    ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned(),
                    ValueRef::Blob(v) => String::from_utf8_lossy(v).into_owned(),
                };
                dump.push_str(&text);
                dump.push('\n');
            }
        }
    }

    assert!(!dump.contains(SECRET), "鉴权原文不得入库");
    assert!(!dump.contains("super-secret"), "鉴权原文片段也不得入库");
    assert!(
        dump.contains(&fingerprint(SECRET)),
        "指纹应已持久化（证明档案确实写入）"
    );
}

// 7. 回填：半开区间精确改写；区间外不动；重放不覆盖手动修正
#[test]
fn reassign_range_backfills_only_inside_and_survives_replay() {
    let mut store = Store::open_in_memory().unwrap();
    let t_switch = T0 + 4 * HOUR;
    apply(&mut store, Some("https://a.example.com"), Some("sk-a"), T0);
    let a_id = store.profiles().unwrap()[0].id;
    apply(
        &mut store,
        Some("https://b.example.com"),
        Some("sk-b"),
        t_switch,
    );
    let b_id = store.profiles().unwrap()[1].id;

    let t10 = T0 + 10 * MINUTE;
    let t20 = T0 + 20 * MINUTE;
    let t30 = T0 + 30 * MINUTE;
    let dir = TempDir::new();
    ingest_lines(
        &mut store,
        &dir,
        "backfill.jsonl",
        &[
            usage_line("r10", t10, 1, 0),
            usage_line("r20", t20, 2, 0),
            usage_line("r30", t30, 3, 0),
        ],
    );
    assert_eq!(profile_of(&store, "r20"), Some(a_id));

    // [t20, t30) 改 Unknown：t20 命中，t30（半开端点）不动
    assert_eq!(store.reassign_profile_range(None, t20, t30).unwrap(), 1);
    assert_eq!(profile_of(&store, "r10"), Some(a_id));
    assert_eq!(profile_of(&store, "r20"), None);
    assert_eq!(profile_of(&store, "r30"), Some(a_id), "end 为开区间");

    // [t10, t20) 改为档案 B：区间外（r20）不动
    assert_eq!(
        store.reassign_profile_range(Some(b_id), t10, t20).unwrap(),
        1
    );
    assert_eq!(profile_of(&store, "r10"), Some(b_id));
    assert_eq!(profile_of(&store, "r20"), None);
    assert_eq!(profile_of(&store, "r30"), Some(a_id));

    // 空区间不更新任何行
    assert_eq!(
        store.reassign_profile_range(Some(a_id), t20, t20).unwrap(),
        0
    );

    // 重放同一条记录（UPSERT）不得覆盖手动归因
    if let ParsedLine::Record(record) = parse_line(usage_line("r10", t10, 99, 9).as_bytes()) {
        store.upsert_records(&[record]).unwrap();
    } else {
        panic!("fixture 行应可解析");
    }
    assert_eq!(
        profile_of(&store, "r10"),
        Some(b_id),
        "手动修正必须比后到者胜更持久"
    );
}

// 9. v1 → v2 迁移：存量记录保留且 profile_id = NULL（Unknown），新表可用
#[test]
fn migrates_v1_database_forward_to_v3() {
    let dir = TempDir::new();
    let db_path = dir.path().join("legacy.sqlite3");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
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
                cc_version            TEXT
            );
            CREATE INDEX idx_usage_records_timestamp ON usage_records(timestamp_ms);
            CREATE INDEX idx_usage_records_session ON usage_records(session_id);
            CREATE TABLE file_checkpoints (
                file_path   TEXT PRIMARY KEY NOT NULL,
                byte_offset INTEGER NOT NULL,
                mtime_ms    INTEGER NOT NULL,
                size        INTEGER NOT NULL
            );
            INSERT INTO usage_records
                VALUES ('legacy-1', 's', 1000, 1, 2, 3, 4, 'm', 'cli', 0, NULL, NULL);
            PRAGMA user_version = 1;
            "#,
        )
        .unwrap();
    }

    // v3（D22 修订）：旧口径数据整体挪入 usage_records_legacy 保留不销毁，
    // 新 usage_records 为空（等 checkpoint 清空后的全量重扫按 message.id
    // 身份重建），孤儿历史不进聚合但可追溯。
    let mut store = Store::open(&db_path).unwrap();
    assert_eq!(store.count_records().unwrap(), 0, "新表为空待重扫");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let legacy: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_records_legacy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(legacy, 1, "旧口径存量保留在 legacy 表");
        let checkpoints: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_checkpoints", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(checkpoints, 0, "checkpoint 已清空触发全量重扫");
    }

    // 迁移后的库可正常应用快照
    apply(&mut store, Some("https://a.example.com"), None, 2_000);
    assert_eq!(store.profiles().unwrap().len(), 1);
    drop(store);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);
}

// 10. R2-2 手动额度三列：默认 NULL、整体覆盖、未知档案不命中
#[test]
fn manual_quota_columns_are_nullable_and_updatable() {
    let dir = TempDir::new();
    let db_path = dir.path().join("quota.sqlite3");
    let mut store = Store::open(&db_path).unwrap();
    apply(
        &mut store,
        Some("https://api.example.com"),
        Some("sk-a"),
        T0,
    );
    let profile_id = store.profiles().unwrap()[0].id;

    let read_quota = |id: i64| {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT quota_manual_total, quota_manual_used, quota_reset_date
             FROM profiles WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .unwrap()
    };

    assert_eq!(
        read_quota(profile_id),
        (None, None, None),
        "未配置 = 三列全 NULL（UI 不显示配额条）"
    );

    assert!(store
        .update_profile_quota(
            profile_id,
            Some(2_000_000),
            Some(350_000),
            Some("2026-11-01")
        )
        .unwrap());
    assert_eq!(
        read_quota(profile_id),
        (
            Some(2_000_000),
            Some(350_000),
            Some("2026-11-01".to_string())
        )
    );

    // 整体覆盖：None 清除对应项（已用不从统计累加，仅存用户输入）
    assert!(store
        .update_profile_quota(profile_id, Some(3_000_000), None, None)
        .unwrap());
    assert_eq!(read_quota(profile_id), (Some(3_000_000), None, None));

    // 未知档案不命中
    assert!(!store
        .update_profile_quota(profile_id + 999, Some(1), None, None)
        .unwrap());
}

// 8. ingest/scan 端到端：乱序 JSONL 按 timestamp 逐条物化归因，重扫幂等
#[test]
fn ingest_materializes_attribution_end_to_end() {
    let mut store = Store::open_in_memory().unwrap();
    let t1 = T0 + HOUR;
    let t2 = T0 + 2 * HOUR;
    apply(&mut store, Some("https://a.example.com"), Some("sk-a"), T0);
    apply(&mut store, Some("https://b.example.com"), Some("sk-b"), t1);
    apply(&mut store, Some("https://a.example.com"), Some("sk-a"), t2);
    let profiles = store.profiles().unwrap();
    let a_id = profiles[0].id;
    let b_id = profiles[1].id;

    let dir = TempDir::new();
    fs::write(
        dir.path().join("mixed.jsonl"),
        format!(
            "{}\n",
            [
                usage_line("pre", T0 - HOUR, 1, 0),
                usage_line("a2", t2 + 10 * MINUTE, 6, 0),
                usage_line("b-grace", t1 + MINUTE, 3, 0),
                usage_line("a1", T0 + 10 * MINUTE, 2, 0),
                usage_line("a2-grace", t2 + MINUTE, 5, 0),
                usage_line("b1", t1 + 10 * MINUTE, 4, 0),
            ]
            .join("\n")
        ),
    )
    .unwrap();

    let stats = ingest::scan_dir(&mut store, dir.path()).unwrap();
    assert_eq!(stats.records, 6);
    assert_eq!(stats.bad_lines, 0);

    assert_eq!(profile_of(&store, "pre"), None);
    assert_eq!(profile_of(&store, "a1"), Some(a_id));
    assert_eq!(profile_of(&store, "b-grace"), None);
    assert_eq!(profile_of(&store, "b1"), Some(b_id));
    assert_eq!(profile_of(&store, "a2-grace"), None);
    assert_eq!(profile_of(&store, "a2"), Some(a_id));

    // 再扫一遍：checkpoint 无新行，归因结果不变
    let before = store.record_profiles().unwrap();
    let again = ingest::scan_dir(&mut store, dir.path()).unwrap();
    assert_eq!(again.records, 0);
    assert_eq!(store.record_profiles().unwrap(), before);
}
