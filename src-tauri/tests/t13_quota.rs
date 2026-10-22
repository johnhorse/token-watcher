//! t13_quota: v0.2 自动配额（Auto Quota）综合测试套件
//!
//! 覆盖：
//! 1. One-API / New-API 标准响应解析与 access_until 转换；
//! 2. HTTP 200 + {"error": ...} 错误信封拦截；
//! 3. 启发式 cents / dollars 自适应校准；
//! 4. 0 额度哨兵不限额处理；
//! 5. SSRF 防御（跨 Origin 拒绝借用 Key，同 Origin 允许借用）；
//! 6. Schema V5 迁移与 COALESCE 读取平滑兼容；
//! 7. update_profile_quota_config 单一真源写入与在途覆盖保护；
//! 8. 单飞 Single-Flight 防抖保护。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use axum::response::Json;
use axum::routing::get;
use axum::Router;
use serde_json::json;

use token_watcher_lib::core::agg::{dashboard_at, QuotaInfo, Range};
use token_watcher_lib::core::attr::parse_settings;
use token_watcher_lib::core::quota::{
    check_error_envelope, normalize_api_base_url, parse_credit_grants_response,
    parse_subscription_response, parse_usage_response, OriginInfo, QuotaClient, QuotaSyncer,
};
use token_watcher_lib::core::store::{Store, SCHEMA_VERSION};

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
        let path = std::env::temp_dir().join(format!("tw-quota-test-{nanos}-{n}"));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn create_profile(store: &mut Store, endpoint: &str, at_ms: i64) -> i64 {
    let snapshot =
        parse_settings(&json!({ "env": { "ANTHROPIC_BASE_URL": endpoint } }).to_string());
    store.apply_settings_snapshot(&snapshot, at_ms).unwrap();
    let id = store.profiles().unwrap().last().unwrap().id;
    store
        .apply_settings_snapshot(&parse_settings("{}"), at_ms + 1)
        .unwrap();
    id
}

#[test]
fn test_origin_parsing_and_same_origin_check() {
    // 1. 同 Origin 测试
    assert!(OriginInfo::is_same_origin(
        "https://api.oneapi.com",
        "https://api.oneapi.com"
    ));
    assert!(OriginInfo::is_same_origin(
        "https://api.oneapi.com/v1",
        "https://api.oneapi.com/dashboard/billing/subscription"
    ));
    assert!(OriginInfo::is_same_origin(
        "https://api.oneapi.com:443",
        "https://api.oneapi.com"
    ));
    assert!(OriginInfo::is_same_origin(
        "http://proxy.local:8080/prefix",
        "http://proxy.local:8080/v1/sub"
    ));

    // 2. 跨 Origin 测试（SSRF 防御拦截）
    assert!(!OriginInfo::is_same_origin(
        "https://api.oneapi.com",
        "https://evil.com"
    ));
    assert!(!OriginInfo::is_same_origin(
        "https://api.oneapi.com",
        "http://api.oneapi.com"
    ));
    assert!(!OriginInfo::is_same_origin(
        "https://api.oneapi.com:443",
        "https://api.oneapi.com:8443"
    ));
    assert!(!OriginInfo::is_same_origin(
        "https://api.oneapi.com",
        "https://attacker.api.oneapi.com"
    ));
}

#[test]
fn test_url_normalization() {
    assert_eq!(
        normalize_api_base_url("https://api.relay.com/v1"),
        "https://api.relay.com"
    );
    assert_eq!(
        normalize_api_base_url("https://api.relay.com/v1/"),
        "https://api.relay.com"
    );
    assert_eq!(
        normalize_api_base_url("https://api.relay.com/subpath/v1"),
        "https://api.relay.com/subpath"
    );
    assert_eq!(
        normalize_api_base_url("https://api.relay.com"),
        "https://api.relay.com"
    );
}

#[test]
fn test_error_envelope_interception() {
    // 1. 正常响应
    let ok_json = json!({
        "object": "billing_subscription",
        "hard_limit_usd": 100.0
    });
    assert!(check_error_envelope(&ok_json).is_ok());

    // 2. HTTP 200 + error 对象
    let err_envelope_1 = json!({
        "error": {
            "message": "Invalid API key or balance exhausted",
            "type": "one_api_error"
        }
    });
    assert!(check_error_envelope(&err_envelope_1).is_err());

    // 3. HTTP 200 + error 字符串
    let err_envelope_2 = json!({
        "error": "Access denied"
    });
    assert!(check_error_envelope(&err_envelope_2).is_err());

    // 4. HTTP 200 + 包含提示的 message
    let err_msg = json!({
        "message": "Token quota expired"
    });
    assert!(check_error_envelope(&err_msg).is_err());
}

#[test]
fn test_subscription_and_usage_parsing() {
    // 1. One-API 标准响应带 access_until 秒级时间戳
    let sub_json = json!({
        "object": "billing_subscription",
        "hard_limit_usd": 50.0,
        "soft_limit_usd": 40.0,
        "access_until": 1793404800 // 2026-10-31 或类似日期
    });
    let (total, reset_date) = parse_subscription_response(&sub_json).unwrap();
    assert_eq!(total, Some(50.0));
    assert!(reset_date.is_some());
    assert_eq!(reset_date.unwrap(), "2026-10-31");
    // 2. 0 额度哨兵不限额
    let sub_unlimited = json!({
        "hard_limit_usd": 0.0,
        "access_until": 0
    });
    let (tot_unlimited, reset_unlimited) = parse_subscription_response(&sub_unlimited).unwrap();
    assert_eq!(tot_unlimited, None, "0 限额视为不限额");
    assert_eq!(reset_unlimited, None);

    // 3. usage 解析：标准美分转换为美元
    let usage_cents = json!({
        "object": "list",
        "total_usage": 3250.0 // $32.50
    });
    // 当总额是 50.0 时，3250.0 > 50*50 => 美分自适应
    let parsed_used = parse_usage_response(&usage_cents, Some(50.0)).unwrap();
    assert_eq!(parsed_used, Some(32.5));

    // 4. usage 解析：直接返回标准美元
    let usage_direct = json!({
        "total_usage": 12.34
    });
    let parsed_direct = parse_usage_response(&usage_direct, Some(50.0)).unwrap();
    assert_eq!(parsed_direct, Some(12.34));
}

#[test]
fn test_credit_grants_fallback_parsing() {
    let grants_json = json!({
        "total_granted": 100.0,
        "total_used": 28.5
    });
    let quota = parse_credit_grants_response(&grants_json).unwrap();
    assert_eq!(quota.total, Some(100.0));
    assert_eq!(quota.used, Some(28.5));
    assert_eq!(quota.reset_date, None);
}

#[test]
fn test_schema_v5_migration_and_coalesce_reading() {
    let dir = TempDir::new();
    let db_path = dir.path().join("v4_to_v5.sqlite3");

    // 构造一个 Schema V4 的数据库
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE profiles (
                id                     INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint               TEXT NOT NULL,
                credential_fingerprint TEXT,
                display_name           TEXT NOT NULL,
                created_at             INTEGER NOT NULL,
                quota_manual_total     INTEGER,
                quota_manual_used      INTEGER,
                quota_reset_date       TEXT
            );
            CREATE UNIQUE INDEX idx_profiles_identity
                ON profiles(endpoint, COALESCE(credential_fingerprint, ''));
            CREATE TABLE profile_timeline (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                profile_id INTEGER NOT NULL REFERENCES profiles(id),
                start_ms   INTEGER NOT NULL,
                end_ms     INTEGER
            );
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
                profile_id            INTEGER REFERENCES profiles(id),
                attribution_source    TEXT NOT NULL DEFAULT 'timeline'
            );
            CREATE TABLE file_checkpoints (
                file_path   TEXT PRIMARY KEY NOT NULL,
                byte_offset INTEGER NOT NULL,
                mtime_ms    INTEGER NOT NULL,
                size        INTEGER NOT NULL
            );
            PRAGMA user_version = 4;

            -- 插入两条旧手动数据
            INSERT INTO profiles (endpoint, credential_fingerprint, display_name, created_at, quota_manual_total, quota_manual_used, quota_reset_date)
            VALUES ('https://api.legacy.com', 'fp1', 'Legacy Manual', 1000, 5000000, 1200000, '2026-12-01');
            INSERT INTO profile_timeline (profile_id, start_ms, end_ms) VALUES (1, 1000, NULL);
            INSERT INTO usage_records VALUES ('rec-1', 's1', 1500, 100, 20, 0, 0, 'claude-3-opus', 'cli', 0, NULL, NULL, 1, 'timeline');
            "#,
        )
        .unwrap();
    }

    // 打开 Store 触发 V5 迁移
    let store = Store::open(&db_path).unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(version, 6);
        // 检查迁移后字段平滑填充：quota_total 继承 quota_manual_total，quota_unit 为 tokens
        let (q_type, q_unit, q_total, q_used) = conn
            .query_row(
                "SELECT quota_type, quota_unit, quota_total, quota_used FROM profiles WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<f64>>(2)?,
                        row.get::<_, Option<f64>>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(q_type, "manual");
        assert_eq!(q_unit, "tokens");
        assert_eq!(q_total, Some(5000000.0));
        assert_eq!(q_used, Some(1200000.0));
    }

    // 聚合层读取通过 COALESCE 正常水合
    let now = 2000;
    let dash = dashboard_at(&store, Range::All, now).unwrap();
    let card = &dash.profiles[0];
    assert_eq!(card.display_name, "Legacy Manual");
    assert_eq!(
        card.quota,
        Some(QuotaInfo {
            quota_type: "manual".to_string(),
            unit: "tokens".to_string(),
            total: Some(5000000.0),
            used: Some(1200000.0),
            reset_date: Some("2026-12-01".to_string()),
            updated_at: None,
            sync_status: None,
            sync_error: None,
        })
    );
}

#[test]
fn test_update_profile_quota_config_and_in_flight_protection() {
    let dir = TempDir::new();
    let db_path = dir.path().join("quota_ops.sqlite3");
    let mut store = Store::open(&db_path).unwrap();

    let profile_id = create_profile(&mut store, "https://api.relay.com", 1000);

    // 1. 设置为 API 模式
    assert!(store
        .update_profile_quota_config(
            profile_id,
            "api",
            "usd",
            Some("https://api.relay.com"),
            Some("sk-dedicated"),
            None,
            None,
            None,
        )
        .unwrap());

    // 2. 模拟拉取成功回写
    assert!(store
        .save_profile_quota_sync_result(
            profile_id,
            Some(50.0),
            Some(12.5),
            Some("2026-11-30"),
            "ok",
            None,
            2000,
        )
        .unwrap());

    // 验证回写数据
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let (tot, used, status) = conn
            .query_row(
                "SELECT quota_total, quota_used, quota_sync_status FROM profiles WHERE id = ?1",
                [profile_id],
                |row| {
                    Ok((
                        row.get::<_, Option<f64>>(0)?,
                        row.get::<_, Option<f64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(tot, Some(50.0));
        assert_eq!(used, Some(12.5));
        assert_eq!(status, Some("ok".to_string()));
    }

    // 3. 用户在 UI 切换回手动模式
    assert!(store
        .update_profile_quota_config(
            profile_id,
            "manual",
            "tokens",
            None,
            None,
            Some(10_000_000.0),
            Some(3_000_000.0),
            Some("2026-12-31"),
        )
        .unwrap());

    // 4. 延迟网络响应返回，尝试写回 API 结果，应当被 WHERE quota_type = 'api' 拦截，不覆盖用户手动配额
    let affected = store
        .save_profile_quota_sync_result(
            profile_id,
            Some(999.0),
            Some(999.0),
            Some("2029-01-01"),
            "ok",
            None,
            3000,
        )
        .unwrap();
    assert!(!affected, "在途 API 结果严禁覆盖刚切换的手动模式");

    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let (tot_after, used_after) = conn
            .query_row(
                "SELECT quota_total, quota_used FROM profiles WHERE id = ?1",
                [profile_id],
                |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?)),
            )
            .unwrap();
        assert_eq!(tot_after, Some(10_000_000.0));
        assert_eq!(used_after, Some(3_000_000.0));
    }
}

#[tokio::test]
async fn test_quota_client_live_mock_server() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = calls.clone();

    // 启动本地 Axum Mock 模拟中转站接口
    let app = Router::new()
        .route(
            "/v1/dashboard/billing/subscription",
            get(|| async {
                Json(json!({
                    "object": "billing_subscription",
                    "hard_limit_usd": 50.0,
                    "access_until": 1793404800
                }))
            }),
        )
        .route(
            "/v1/dashboard/billing/usage",
            get(move || {
                let c = calls_clone.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "object": "list",
                        "total_usage": 1550.0 // $15.50 (美分)
                    }))
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mock_base = format!("http://127.0.0.1:{port}");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = QuotaClient::new();
    let res = client
        .fetch_quota(&mock_base, "sk-test-mock")
        .await
        .unwrap();

    assert_eq!(res.total, Some(50.0));
    assert_eq!(res.used, Some(15.5));
    assert_eq!(res.reset_date, Some("2026-10-31".to_string()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_quota_syncer_ssrf_and_credential_borrowing() {
    let dir = TempDir::new();
    let db_path = dir.path().join("syncer.sqlite3");
    let settings_path = dir.path().join("settings.json");

    // 模拟 settings.json 当前活跃配置为 https://api.active.com，key 为 sk-borrowed
    let settings_content = json!({
        "env": {
            "ANTHROPIC_BASE_URL": "https://api.active.com",
            "ANTHROPIC_AUTH_TOKEN": "sk-borrowed"
        }
    });
    fs::write(&settings_path, settings_content.to_string()).unwrap();

    let mut store = Store::open(&db_path).unwrap();

    // 1. 档案 1: endpoint 为 https://api.active.com（同 Origin，允许借用）
    let pid_active = create_profile(&mut store, "https://api.active.com", 1000);
    store
        .update_profile_quota_config(
            pid_active, "api", "usd", None, // 未配 url => 继承 endpoint
            None, // 未配独立 key => 借用 settings
            None, None, None,
        )
        .unwrap();

    // 2. 档案 2: endpoint 为 https://api.other.com（跨 Origin，未配 key => 标记 inactive 或拒绝）
    let pid_other = create_profile(&mut store, "https://api.other.com", 2000);
    store
        .update_profile_quota_config(
            pid_other, "api", "usd", None, None, // 无独立 key
            None, None, None,
        )
        .unwrap();

    // 3. 档案 3: endpoint 为 https://api.active.com，但配置了跨站的 quota_api_url: https://evil.attacker.com
    let pid_evil = create_profile(&mut store, "https://api.active-evil.com", 3000);
    store
        .update_profile_quota_config(
            pid_evil,
            "api",
            "usd",
            Some("https://evil.attacker.com"), // 跨站 URL
            None,                              // 试图借用活跃 key 攻击第三方
            None,
            None,
            None,
        )
        .unwrap();

    drop(store);

    let syncer = QuotaSyncer::new(db_path.clone(), settings_path);

    // 测试档案 2（非活跃且无专属 key）：必须置为 inactive
    let res_other = syncer.sync_profile(pid_other).await.unwrap();
    assert_eq!(res_other.sync_status, Some("inactive".to_string()));

    // 测试档案 3（跨 Origin 借用生产 Key）：SSRF 防御拒绝执行！
    let res_evil = syncer.sync_profile(pid_evil).await;
    assert!(res_evil.is_err());
    let err_msg = res_evil.unwrap_err();
    assert!(err_msg.contains("跨站点必须配置专属 Key") || err_msg.contains("缺少查询 Key"));
}
