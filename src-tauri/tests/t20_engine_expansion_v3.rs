//! t20_engine_expansion_v3: v0.7.0 多数据源扩展 3.0 与思维链冰山模型集成测试套件
//!
//! 覆盖测试计划中的全部 Critical Paths (CP-1 ~ CP-3) 与 Edge Cases (EC-1 ~ EC-3)：
//! - CP-1: Antigravity Protobuf Wire-Walker 解密真实会话 (f1.f4 提取 4D tokens + thoughts + steps timestamp)
//! - CP-2: ZCode D24 扣减包含式缓存防双计 (net_input == 12735 & durationMs == 11140)
//! - CP-3: Schema v7 迁移、thoughts_tokens 持久化与隔离引擎名单归因
//! - EC-1: Antigravity SQLite 只读安全与快照降级
//! - EC-2: ZCode 脏数据下溢防范 (saturating_sub 保护)
//! - EC-3: Thinking Iceberg 零除保护与聚合比例计算

use std::fs;
use std::path::{Path, PathBuf};
use token_watcher_lib::core::agg::{
    available_engines_between, dashboard_at_with_prices_and_engine, detect_engine, Range,
};
use token_watcher_lib::core::ingest::{detect_file_kind, ingest_file, FileEngineKind};
use token_watcher_lib::core::parse::{
    parse_antigravity_db, parse_qoder_line, parse_workbuddy_line, parse_zcode_line, ParsedLine,
    UsageRecord,
};
use token_watcher_lib::core::store::{Store, SCHEMA_VERSION};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "tw-test-expansion-v3-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// -----------------------------------------------------------------------------
// CP-1: Antigravity Protobuf Wire-Walker 解密真实会话
// -----------------------------------------------------------------------------
#[test]
fn test_cp1_antigravity_protobuf_extraction_and_ingest() {
    let fixture_path = Path::new("tests/fixtures/antigravity/sample_conversation.db");
    assert!(
        fixture_path.exists(),
        "Antigravity sample fixture 必须存在于 tests/fixtures/antigravity/sample_conversation.db"
    );

    // 1. 直读解析测试
    let (records, max_idx) =
        parse_antigravity_db(fixture_path, -1).expect("解析 Antigravity DB 成功");
    assert_eq!(records.len(), 5, "应成功解出 5 条用量记录");
    assert_eq!(max_idx, 4, "max_idx 应为 4");

    // 验证第 1 条无缓存记录 (Record 0)
    let r0 = &records[0];
    assert_eq!(
        r0.record_id,
        "antigravity:f269529e-e7aa-462e-8d46-f83d3b9d6f31:0"
    );
    assert_eq!(
        r0.session_id,
        "antigravity:f269529e-e7aa-462e-8d46-f83d3b9d6f31"
    );
    assert_eq!(r0.input_tokens, 22985);
    assert_eq!(r0.output_tokens, 522);
    assert_eq!(r0.cache_read_tokens, 0);
    assert_eq!(r0.thoughts_tokens, 463, "thoughts_tokens 应精准解析出 463");
    assert_eq!(r0.model, "gemini-3.8-flash");
    assert_eq!(
        r0.timestamp_ms, 1789717583428,
        "时间戳应从 steps.metadata 正确提取"
    );

    // 验证带缓存与真实耗时的记录 (Record 2)
    let r2 = &records[2];
    assert_eq!(r2.input_tokens, 5621);
    assert_eq!(r2.output_tokens, 93);
    assert_eq!(r2.thoughts_tokens, 45);
    assert_eq!(r2.cache_read_tokens, 20370);
    assert_eq!(
        r2.duration_ms,
        Some(24617),
        "单轮耗时应从 steps.metadata 计算出 24617ms"
    );

    // 2. 增量摄取与 Checkpoint 幂等性测试
    let temp = TempDir::new("antigravity-ingest");
    let mut store = Store::open(&temp.path.join("db.sqlite3")).unwrap();

    let stats1 = ingest_file(&mut store, fixture_path).expect("首扫增量摄取成功");
    assert_eq!(stats1.records, 5);

    // 再次摄取同一文件，应依据 checkpoint 跳过已读记录
    let stats2 = ingest_file(&mut store, fixture_path).expect("次扫增量摄取成功");
    assert_eq!(stats2.records, 0, "再次摄取应无新增记录");
}

// -----------------------------------------------------------------------------
// CP-2: ZCode D24 扣减包含式缓存与防双计
// -----------------------------------------------------------------------------
#[test]
fn test_cp2_zcode_d24_saturating_sub_and_ingest() {
    let fixture_path = Path::new("tests/fixtures/zcode/model-io-session_sample.jsonl");
    assert!(
        fixture_path.exists(),
        "ZCode fixture 必须存在于 tests/fixtures/zcode/model-io-session_sample.jsonl"
    );

    let content = fs::read_to_string(fixture_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "fixture 中应包含 3 行真实记录");

    // 第 1 行：inputTokens: 35839, cacheReadTokens: 23104 -> net_input == 12735
    let r1 = match parse_zcode_line(lines[0].as_bytes(), "test-sess") {
        ParsedLine::Record(r) => r,
        other => panic!("预期为 Record，实际得到: {:?}", other),
    };
    assert_eq!(
        r1.input_tokens, 12735,
        "严格应用 D24: 35839 - 23104 = 12735"
    );
    assert_eq!(r1.output_tokens, 178);
    assert_eq!(r1.cache_read_tokens, 23104);
    assert_eq!(r1.duration_ms, Some(11140), "原生毫秒耗时应为 11140ms");
    assert_eq!(r1.model, "GLM-5.3");
    assert_eq!(r1.entrypoint, "zcode");

    // 验证通过 ingest_file 摄取
    let temp = TempDir::new("zcode-ingest");
    let mut store = Store::open(&temp.path.join("db.sqlite3")).unwrap();
    let stats = ingest_file(&mut store, fixture_path).unwrap();
    assert_eq!(stats.records, 3);
}

// -----------------------------------------------------------------------------
// CP-3: Schema v7 迁移与隔离引擎归因
// -----------------------------------------------------------------------------
#[test]
fn test_cp3_schema_v7_migration_and_isolation() {
    let temp = TempDir::new("schema-v7");
    let db_path = temp.path.join("v6_to_v7.db");

    // 模拟构造 v6 数据库
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
                cc_version            TEXT,
                profile_id            INTEGER,
                attribution_source    TEXT NOT NULL DEFAULT 'timeline',
                duration_ms           INTEGER
            );
            CREATE TABLE file_checkpoints (
                file_path   TEXT PRIMARY KEY NOT NULL,
                byte_offset INTEGER NOT NULL,
                mtime_ms    INTEGER NOT NULL,
                size        INTEGER NOT NULL
            );
            CREATE TABLE profiles (
                id                     INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint               TEXT NOT NULL,
                credential_fingerprint TEXT,
                display_name           TEXT NOT NULL,
                created_at             INTEGER NOT NULL
            );
            CREATE TABLE profile_timeline (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                profile_id INTEGER NOT NULL REFERENCES profiles(id),
                start_ms   INTEGER NOT NULL,
                end_ms     INTEGER
            );
            CREATE TABLE proxy_attributions (
                message_id   TEXT PRIMARY KEY NOT NULL,
                profile_id   INTEGER NOT NULL REFERENCES profiles(id),
                upstream_url TEXT,
                created_at   INTEGER NOT NULL
            );
            PRAGMA user_version = 6;
            "#,
        )
        .unwrap();
    }

    // 通过 Store::open 触发 Schema v7 迁移
    let mut store = Store::open(&db_path).unwrap();

    // 验证版本号已跃升为 7
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        assert_eq!(version, SCHEMA_VERSION);
    }

    // 验证写入包含 thoughts_tokens 的记录，且 4 大新引擎均被标记为隔离引擎 (profile_id = None, attribution_source = 'unknown')
    let rec_ag = UsageRecord {
        record_id: "antigravity:sess-1:1".to_string(),
        session_id: "antigravity:sess-1".to_string(),
        timestamp_ms: 1_000_000,
        input_tokens: 1000,
        output_tokens: 200,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        model: "gemini-3.8-flash".to_string(),
        entrypoint: "antigravity".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
        duration_ms: Some(1500),
        thoughts_tokens: 180,
    };
    let rec_zc = UsageRecord {
        record_id: "zcode:sess-2:req-1".to_string(),
        session_id: "zcode:sess-2".to_string(),
        timestamp_ms: 1_001_000,
        input_tokens: 2000,
        output_tokens: 300,
        cache_read_tokens: 500,
        cache_creation_tokens: 0,
        model: "GLM-5.3".to_string(),
        entrypoint: "zcode".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
        duration_ms: Some(2500),
        thoughts_tokens: 220,
    };
    let rec_qd = UsageRecord {
        record_id: "qoder:sess-3:msg-1".to_string(),
        session_id: "qoder:sess-3".to_string(),
        timestamp_ms: 1_002_000,
        input_tokens: 3000,
        output_tokens: 400,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        model: "qwen-2.5-coder-32b".to_string(),
        entrypoint: "qoder".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
        duration_ms: None,
        thoughts_tokens: 0,
    };
    let rec_wb = UsageRecord {
        record_id: "workbuddy:sess-4:msg-1".to_string(),
        session_id: "workbuddy:sess-4".to_string(),
        timestamp_ms: 1_003_000,
        input_tokens: 4000,
        output_tokens: 500,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        model: "hunyuan-code".to_string(),
        entrypoint: "workbuddy".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
        duration_ms: None,
        thoughts_tokens: 0,
    };

    store
        .upsert_records(&[rec_ag, rec_zc, rec_qd, rec_wb])
        .unwrap();

    // 检查落库结果
    let (pid_ag, src_ag) = store
        .record_attribution_details("antigravity:sess-1:1")
        .unwrap()
        .unwrap();
    assert_eq!(pid_ag, None, "Antigravity 应被隔离，不归入任何 profile");
    assert_eq!(src_ag, "unknown");

    let (pid_zc, src_zc) = store
        .record_attribution_details("zcode:sess-2:req-1")
        .unwrap()
        .unwrap();
    assert_eq!(pid_zc, None, "ZCode 应被隔离");
    assert_eq!(src_zc, "unknown");

    let (pid_qd, src_qd) = store
        .record_attribution_details("qoder:sess-3:msg-1")
        .unwrap()
        .unwrap();
    assert_eq!(pid_qd, None, "Qoder 应被隔离");
    assert_eq!(src_qd, "unknown");

    let (pid_wb, src_wb) = store
        .record_attribution_details("workbuddy:sess-4:msg-1")
        .unwrap()
        .unwrap();
    assert_eq!(pid_wb, None, "WorkBuddy 应被隔离");
    assert_eq!(src_wb, "unknown");
}

// -----------------------------------------------------------------------------
// Qoder 与 WorkBuddy 规范解析与摄取
// -----------------------------------------------------------------------------
#[test]
fn test_qoder_and_workbuddy_fixtures_ingest() {
    let qoder_fixture = Path::new("tests/fixtures/qoder/sample_session.jsonl");
    let workbuddy_fixture = Path::new("tests/fixtures/workbuddy/sample_session.jsonl");

    assert!(qoder_fixture.exists());
    assert!(workbuddy_fixture.exists());

    assert_eq!(detect_file_kind(qoder_fixture, b""), FileEngineKind::Qoder);
    assert_eq!(
        detect_file_kind(workbuddy_fixture, b""),
        FileEngineKind::WorkBuddy
    );

    let qd_line = fs::read_to_string(qoder_fixture)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    match parse_qoder_line(qd_line.as_bytes(), "test-qd") {
        ParsedLine::Record(r) => {
            assert_eq!(r.model, "qwen-2.5-coder-32b");
            assert_eq!(r.entrypoint, "qoder");
        }
        other => panic!("预期 Record，得到 {:?}", other),
    }

    let wb_line = fs::read_to_string(workbuddy_fixture)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    match parse_workbuddy_line(wb_line.as_bytes(), "test-wb") {
        ParsedLine::Record(r) => {
            assert_eq!(r.model, "hunyuan-code");
            assert_eq!(r.entrypoint, "workbuddy");
        }
        other => panic!("预期 Record，得到 {:?}", other),
    }

    let temp = TempDir::new("qoder-wb-ingest");
    let mut store = Store::open(&temp.path.join("db.sqlite3")).unwrap();

    let stats_qd = ingest_file(&mut store, qoder_fixture).unwrap();
    assert_eq!(stats_qd.records, 2);

    let stats_wb = ingest_file(&mut store, workbuddy_fixture).unwrap();
    assert_eq!(stats_wb.records, 2);
}

// -----------------------------------------------------------------------------
// EC-1: Antigravity 快照副本降级测试
// -----------------------------------------------------------------------------
#[test]
fn test_ec1_antigravity_snapshot_fallback() {
    let fixture_path = Path::new("tests/fixtures/antigravity/sample_conversation.db");
    let temp = TempDir::new("ag-snap");
    let copy_path = temp.path.join("temp_conv.db");
    fs::copy(fixture_path, &copy_path).unwrap();

    // 模拟宿主持有排他事务锁
    let locker = rusqlite::Connection::open(&copy_path).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE;").unwrap();

    // 在宿主持有排他写锁时尝试读取：验证快照降级安全读取且不崩溃
    let (records, max_idx) = parse_antigravity_db(&copy_path, -1).unwrap();
    assert_eq!(records.len(), 5);
    assert_eq!(max_idx, 4);

    locker.execute_batch("ROLLBACK;").unwrap();
}

// -----------------------------------------------------------------------------
// EC-2: ZCode 脏数据下溢防范 (saturating_sub)
// -----------------------------------------------------------------------------
#[test]
fn test_ec2_zcode_dirty_data_saturating_sub() {
    // 模拟脏数据：inputTokens: 100 < cacheReadTokens: 500
    let dirty_json = r#"{
        "type": "turn",
        "sessionId": "dirty-sess",
        "requestId": "req-dirty-1",
        "completedAt": "2026-10-22T08:00:00.000Z",
        "response": {
            "usage": {
                "inputTokens": 100,
                "outputTokens": 50,
                "cacheReadTokens": 500,
                "cacheWriteTokens": 0
            }
        }
    }"#;

    let res = parse_zcode_line(dirty_json.as_bytes(), "dirty-sess");
    match res {
        ParsedLine::Record(r) => {
            assert_eq!(
                r.input_tokens, 0,
                "saturating_sub 应保底为 0，防止无符号整数下溢"
            );
            assert_eq!(r.output_tokens, 50);
            assert_eq!(r.cache_read_tokens, 500);
        }
        other => panic!("预期 Record，实际: {:?}", other),
    }
}

// -----------------------------------------------------------------------------
// EC-3: Thinking Iceberg 零除与聚合比例计算
// -----------------------------------------------------------------------------
#[test]
fn test_ec3_thinking_iceberg_computation_and_zero_division() {
    let temp = TempDir::new("iceberg");
    let mut store = Store::open(&temp.path.join("db.sqlite3")).unwrap();
    let now = 2_000_000_000i64;

    // 1. 空数据库或 output_tokens == 0: thinking_share 应为 None，绝不产生 NaN 或除以零
    let dash_empty =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, None).unwrap();
    assert_eq!(
        dash_empty.thinking_share, None,
        "空数据库 thinking_share 应为 None"
    );
    assert_eq!(dash_empty.thoughts_tokens, 0);

    // 2. 插入仅无思考链记录 (output > 0, thoughts == 0)
    let rec_plain = UsageRecord {
        record_id: "codex:sess-1:resp-1".to_string(),
        session_id: "codex:sess-1".to_string(),
        timestamp_ms: now - 5000,
        input_tokens: 1000,
        output_tokens: 500,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        model: "gpt-4o".to_string(),
        entrypoint: "codex".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
        duration_ms: None,
        thoughts_tokens: 0,
    };
    store.upsert_records(&[rec_plain]).unwrap();

    let dash_plain =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, None).unwrap();
    assert_eq!(
        dash_plain.thinking_share, None,
        "无思考 Token 时 thinking_share 应为 None"
    );
    assert_eq!(dash_plain.thoughts_tokens, 0);

    // 3. 插入 Antigravity 记录：500 output, 400 thoughts
    let rec_think = UsageRecord {
        record_id: "antigravity:sess-2:0".to_string(),
        session_id: "antigravity:sess-2".to_string(),
        timestamp_ms: now - 2000,
        input_tokens: 5000,
        output_tokens: 500,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        model: "gemini-3.8-flash".to_string(),
        entrypoint: "antigravity".to_string(),
        is_sidechain: false,
        cwd: None,
        cc_version: None,
        duration_ms: Some(3000),
        thoughts_tokens: 400,
    };
    store.upsert_records(&[rec_think]).unwrap();

    // 全局聚合：总 output = 500 + 500 = 1000, 总 thoughts = 400 -> thinking_share = 400 / 1000 = 0.4
    let dash_all =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, None).unwrap();
    assert_eq!(dash_all.thoughts_tokens, 400);
    assert_eq!(dash_all.thinking_share, Some(0.4));

    // 按引擎切片聚合: 仅 Antigravity -> output = 500, thoughts = 400 -> 400 / 500 = 0.8 (80%)
    let dash_ag =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("antigravity"))
            .unwrap();
    assert_eq!(dash_ag.engine.as_deref(), Some("antigravity"));
    assert_eq!(dash_ag.thoughts_tokens, 400);
    assert_eq!(
        dash_ag.thinking_share,
        Some(0.8),
        "Gemini 背后深层思考占比应达 80%"
    );
}

// -----------------------------------------------------------------------------
// 跨引擎切片下钻与可用引擎名单
// -----------------------------------------------------------------------------
#[test]
fn test_all_five_engines_slicing_and_detection() {
    let temp = TempDir::new("five-engines");
    let mut store = Store::open(&temp.path.join("db.sqlite3")).unwrap();
    let now = 2_000_000_000i64;

    let records = vec![
        UsageRecord {
            record_id: "claude-uuid-1".to_string(),
            session_id: "sess-claude".to_string(),
            timestamp_ms: now - 5000,
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: "claude-3-7-sonnet".to_string(),
            entrypoint: "claude".to_string(),
            is_sidechain: false,
            cwd: None,
            cc_version: None,
            duration_ms: None,
            thoughts_tokens: 0,
        },
        UsageRecord {
            record_id: "antigravity:sess-ag:1".to_string(),
            session_id: "sess-ag".to_string(),
            timestamp_ms: now - 4000,
            input_tokens: 200,
            output_tokens: 40,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: "gemini-3.8-flash".to_string(),
            entrypoint: "antigravity".to_string(),
            is_sidechain: false,
            cwd: None,
            cc_version: None,
            duration_ms: None,
            thoughts_tokens: 30,
        },
        UsageRecord {
            record_id: "zcode:sess-zc:req-1".to_string(),
            session_id: "sess-zc".to_string(),
            timestamp_ms: now - 3000,
            input_tokens: 300,
            output_tokens: 60,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: "GLM-5.3".to_string(),
            entrypoint: "zcode".to_string(),
            is_sidechain: false,
            cwd: None,
            cc_version: None,
            duration_ms: None,
            thoughts_tokens: 0,
        },
        UsageRecord {
            record_id: "qoder:sess-qd:msg-1".to_string(),
            session_id: "sess-qd".to_string(),
            timestamp_ms: now - 2000,
            input_tokens: 400,
            output_tokens: 80,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: "qwen-2.5-coder-32b".to_string(),
            entrypoint: "qoder".to_string(),
            is_sidechain: false,
            cwd: None,
            cc_version: None,
            duration_ms: None,
            thoughts_tokens: 0,
        },
        UsageRecord {
            record_id: "workbuddy:sess-wb:msg-1".to_string(),
            session_id: "sess-wb".to_string(),
            timestamp_ms: now - 1000,
            input_tokens: 500,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: "hunyuan-code".to_string(),
            entrypoint: "workbuddy".to_string(),
            is_sidechain: false,
            cwd: None,
            cc_version: None,
            duration_ms: None,
            thoughts_tokens: 0,
        },
    ];

    store.upsert_records(&records).unwrap();

    assert_eq!(
        detect_engine(&records[0].record_id, &records[0].entrypoint),
        "claude"
    );
    assert_eq!(
        detect_engine(&records[1].record_id, &records[1].entrypoint),
        "antigravity"
    );
    assert_eq!(
        detect_engine(&records[2].record_id, &records[2].entrypoint),
        "zcode"
    );
    assert_eq!(
        detect_engine(&records[3].record_id, &records[3].entrypoint),
        "qoder"
    );
    assert_eq!(
        detect_engine(&records[4].record_id, &records[4].entrypoint),
        "workbuddy"
    );
    // 验证 available_engines_between 统计
    let available = available_engines_between(&store, now - 10000, now).unwrap();
    let engine_names: Vec<&str> = available.iter().map(|e| e.engine.as_str()).collect();
    assert!(engine_names.contains(&"claude"));
    assert!(engine_names.contains(&"antigravity"));
    assert!(engine_names.contains(&"zcode"));
    assert!(engine_names.contains(&"qoder"));
    assert!(engine_names.contains(&"workbuddy"));

    // 验证单引擎切片下钻
    let dash_ag =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("antigravity"))
            .unwrap();
    assert_eq!(dash_ag.totals.total, 240);
    assert_eq!(dash_ag.engine.as_deref(), Some("antigravity"));

    let dash_zc =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("zcode")).unwrap();
    assert_eq!(dash_zc.totals.total, 360);
    assert_eq!(dash_zc.engine.as_deref(), Some("zcode"));

    let dash_qd =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("qoder")).unwrap();
    assert_eq!(dash_qd.totals.total, 480);
    assert_eq!(dash_qd.engine.as_deref(), Some("qoder"));

    let dash_wb =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("workbuddy"))
            .unwrap();
    assert_eq!(dash_wb.totals.total, 600);
    assert_eq!(dash_wb.engine.as_deref(), Some("workbuddy"));

    let dash_cl =
        dashboard_at_with_prices_and_engine(&store, Range::All, now, None, Some("claude")).unwrap();
    assert_eq!(dash_cl.totals.total, 120);
    assert_eq!(dash_cl.engine.as_deref(), Some("claude"));
}
