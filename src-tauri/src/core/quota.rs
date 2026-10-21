//! v0.2 自动配额核心模块（Auto Quota）
//!
//! 核心能力：
//! 1. One-API / New-API / 开源自建中转站账单接口客户端；
//! 2. 混合凭据借用与严格 SSRF 防护（借用 key 仅限同 Origin）；
//! 3. HTTP 200 错误信封拦截、多货币与单位量级自适应、降级链路；
//! 4. 后台 QuotaSyncer 协调器（短生命周期连接、单飞防抖、只在首扫后启动）。

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::store::Store;

/// 配额同步获取到的标准化结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchedQuota {
    pub total: Option<f64>,
    pub used: Option<f64>,
    pub reset_date: Option<String>,
}

/// 归一化后的 Origin 解析结果 (scheme, host, port)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OriginInfo {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl OriginInfo {
    /// 从 URL 解析出 scheme, host, port。
    /// 若无法解析则返回 None。
    pub fn parse(raw_url: &str) -> Option<Self> {
        let trimmed = raw_url.trim();
        if trimmed.is_empty() {
            return None;
        }
        let parsed = reqwest::Url::parse(trimmed).ok()?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        let host = parsed.host_str()?.to_ascii_lowercase();
        let port = parsed
            .port_or_known_default()
            .unwrap_or(match scheme.as_str() {
                "https" => 443,
                "http" => 80,
                _ => 80,
            });
        Some(Self { scheme, host, port })
    }

    /// 校验 target_url 的 Origin 是否与 base_url 的 Origin 完全一致（P1-3 SSRF 防御）
    pub fn is_same_origin(base_url: &str, target_url: &str) -> bool {
        match (Self::parse(base_url), Self::parse(target_url)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

/// 检查给定 URL 是否属于环回地址（localhost, 127.0.0.1, [::1]）
pub fn is_loopback_address(url_str: &str) -> bool {
    let lower = url_str.to_ascii_lowercase();
    lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("[::1]")
}

/// 剥离 URL 尾部可能存在的 `/v1` 或多余斜杠
pub fn normalize_api_base_url(raw: &str) -> String {
    let mut s = raw.trim().trim_end_matches('/').to_string();
    if s.ends_with("/v1") {
        s.truncate(s.len() - 3);
        s = s.trim_end_matches('/').to_string();
    }
    s
}

/// One-API / New-API 响应体解析
/// 拦截 HTTP 200 错误信封
pub fn check_error_envelope(json: &serde_json::Value) -> Result<(), String> {
    if let Some(err) = json.get("error") {
        if !err.is_null() {
            if let Some(msg) = err.get("message").and_then(|m| m.as_str()) {
                return Err(format!("上游接口错误: {msg}"));
            }
            if let Some(msg) = err.as_str() {
                return Err(format!("上游接口错误: {msg}"));
            }
            return Err("上游接口返回错误信封".to_string());
        }
    }
    if let Some(msg) = json.get("message").and_then(|m| m.as_str()) {
        let lower = msg.to_ascii_lowercase();
        if lower.contains("error")
            || lower.contains("invalid")
            || lower.contains("unauthorized")
            || lower.contains("failed")
            || lower.contains("token")
        {
            return Err(format!("上游接口提示错误: {msg}"));
        }
    }
    Ok(())
}

/// 解析 subscription 响应，获取 hard_limit_usd 等上限及过期日期
/// 优先级: hard_limit_usd > soft_limit_usd > system_hard_limit_usd
pub fn parse_subscription_response(
    json: &serde_json::Value,
) -> Result<(Option<f64>, Option<String>), String> {
    check_error_envelope(json)?;

    let hard = json.get("hard_limit_usd").and_then(|v| v.as_f64());
    let soft = json.get("soft_limit_usd").and_then(|v| v.as_f64());
    let sys = json.get("system_hard_limit_usd").and_then(|v| v.as_f64());

    let limit = hard.or(soft).or(sys);
    // 0 或负数视为无上限 (None)
    let total = match limit {
        Some(val) if val > 0.0 => Some(val),
        _ => None,
    };

    let reset_date = json
        .get("access_until")
        .and_then(|v| v.as_i64())
        .and_then(|sec| {
            if sec <= 0 {
                return None;
            }
            // 转换为 YYYY-MM-DD
            let naive = chrono_date_from_timestamp(sec);
            Some(naive)
        });

    Ok((total, reset_date))
}

/// 解析 usage 响应获取 total_usage
pub fn parse_usage_response(
    json: &serde_json::Value,
    total_limit: Option<f64>,
) -> Result<Option<f64>, String> {
    check_error_envelope(json)?;

    let raw_usage = json.get("total_usage").and_then(|v| v.as_f64());
    let Some(val) = raw_usage else {
        return Ok(None);
    };

    // 启发式量级校准：
    // 若 val 包含小数且在合理范围，按标准单位；
    // 若为巨大整数（> 100,000）且当有 total 时 val > total * 50，或者 val > 1,000,000，
    // 则按美分（val / 100.0）折算，或者 One-API quota per unit（500,000 per USD => val / 500000.0）。
    // 在 OpenAI/One-API 标准接口中，billing/usage 的 total_usage 单位是 cents (美分)！
    // 官方规范：OpenAI usage 接口返回的 total_usage 是 cents (例如 100 表示 $1.00)。
    // 但是部分 New-API / One-API 已经折算成标准美元。
    // 我们在此按以下逻辑自适应：
    // 1. 如果 total_limit 存在，且 val > total_limit * 50.0，说明 val 是美分或者 quota：
    //    如果 val / 100.0 接近合理范围（<= total_limit * 5.0），则 val / 100.0；
    // 2. 如果 total_limit 不存在：若 val > 100,000 且 val.fract() == 0.0，试算美分。
    let adjusted = if let Some(tot) = total_limit {
        if (val > tot && (val.fract() == 0.0 || val > tot * 10.0) && (val / 100.0) <= tot * 10.0)
            || val > tot * 50.0
        {
            val / 100.0
        } else {
            val
        }
    } else if val > 1000.0 && val.fract() == 0.0 {
        val / 100.0
    } else {
        val
    };

    Ok(Some(adjusted))
}

/// 解析降级 credit_grants 响应
pub fn parse_credit_grants_response(json: &serde_json::Value) -> Result<FetchedQuota, String> {
    check_error_envelope(json)?;

    let total = json.get("total_granted").and_then(|v| v.as_f64());
    let used = json.get("total_used").and_then(|v| v.as_f64());

    let total_clean = match total {
        Some(t) if t > 0.0 => Some(t),
        _ => None,
    };

    Ok(FetchedQuota {
        total: total_clean,
        used,
        reset_date: None,
    })
}

fn chrono_date_from_timestamp(sec: i64) -> String {
    // 简易 UTC 转换为 YYYY-MM-DD，不引入额外重量级时区库
    let days = sec / 86400;
    // 1970-01-01 起算的天数转年月日 (基于通用历法算法)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// 构造当月 1 号与明天的日期字符串 (start_date, end_date)
pub fn current_month_and_tomorrow() -> (String, String) {
    let now_sec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let today_str = chrono_date_from_timestamp(now_sec);
    let tomorrow_str = chrono_date_from_timestamp(now_sec + 86400);

    let start_date = if today_str.len() >= 8 {
        format!("{}-01", &today_str[..7])
    } else {
        "2026-01-01".to_string()
    };
    (start_date, tomorrow_str)
}

/// Quota API 客户端
pub struct QuotaClient {
    client: reqwest::Client,
}

impl Default for QuotaClient {
    fn default() -> Self {
        Self::new()
    }
}

impl QuotaClient {
    pub fn new() -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("token-watcher/0.2 (quota-monitor)"),
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    /// 执行一次账单接口拉取
    /// 依次探测:
    /// 1. {base}/v1/dashboard/billing/subscription + usage
    /// 2. {base}/dashboard/billing/subscription + usage
    /// 3. {base}/dashboard/billing/credit_grants
    /// 4. {base}/api/dashboard/billing/credit_grants
    pub async fn fetch_quota(&self, base_url: &str, api_key: &str) -> Result<FetchedQuota, String> {
        let clean_base = normalize_api_base_url(base_url);
        let (start_date, end_date) = current_month_and_tomorrow();

        // 尝试主探测链
        let candidates = [
            (
                format!("{clean_base}/v1/dashboard/billing/subscription"),
                format!("{clean_base}/v1/dashboard/billing/usage?start_date={start_date}&end_date={end_date}"),
            ),
            (
                format!("{clean_base}/dashboard/billing/subscription"),
                format!("{clean_base}/dashboard/billing/usage?start_date={start_date}&end_date={end_date}"),
            ),
        ];

        let mut last_err = String::new();

        for (sub_url, usage_url) in candidates {
            match self
                .try_subscription_and_usage(&sub_url, &usage_url, api_key)
                .await
            {
                Ok(q) => return Ok(q),
                Err(e) => {
                    last_err = e;
                }
            }
        }

        // 尝试降级探测链 credit_grants
        let fallback_candidates = [
            format!("{clean_base}/dashboard/billing/credit_grants"),
            format!("{clean_base}/api/dashboard/billing/credit_grants"),
            format!("{clean_base}/v1/dashboard/billing/credit_grants"),
        ];

        for fb_url in fallback_candidates {
            if let Ok(q) = self.try_credit_grants(&fb_url, api_key).await {
                return Ok(q);
            }
        }

        if last_err.is_empty() {
            Err("中转站配额接口未响应或返回无效数据".to_string())
        } else {
            Err(last_err)
        }
    }

    async fn try_subscription_and_usage(
        &self,
        sub_url: &str,
        usage_url: &str,
        api_key: &str,
    ) -> Result<FetchedQuota, String> {
        let sub_resp = self
            .client
            .get(sub_url)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .send()
            .await
            .map_err(|e| format!("请求 subscription 失败: {e}"))?;

        let status = sub_resp.status();
        if !status.is_success() {
            return Err(format!("接口返回 HTTP {status}"));
        }

        let sub_json = sub_resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| format!("解析 subscription JSON 失败: {e}"))?;

        let (total, reset_date) = parse_subscription_response(&sub_json)?;

        // 联动提取 usage
        let mut used = None;
        if let Ok(usage_resp) = self
            .client
            .get(usage_url)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .send()
            .await
        {
            if usage_resp.status().is_success() {
                if let Ok(usage_json) = usage_resp.json::<serde_json::Value>().await {
                    if let Ok(parsed_used) = parse_usage_response(&usage_json, total) {
                        used = parsed_used;
                    }
                }
            }
        }

        Ok(FetchedQuota {
            total,
            used,
            reset_date,
        })
    }

    async fn try_credit_grants(&self, url: &str, api_key: &str) -> Result<FetchedQuota, String> {
        let resp = self
            .client
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .send()
            .await
            .map_err(|e| format!("请求 credit_grants 失败: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(format!("接口返回 HTTP {status}"));
        }

        let json = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| format!("解析 credit_grants JSON 失败: {e}"))?;

        parse_credit_grants_response(&json)
    }
}

/// 后台 QuotaSyncer 协调器
pub struct QuotaSyncer {
    db_path: PathBuf,
    client: QuotaClient,
    in_flight: Mutex<HashSet<i64>>,
    claude_settings_path: PathBuf,
}

impl QuotaSyncer {
    pub fn new(db_path: PathBuf, claude_settings_path: PathBuf) -> Self {
        Self {
            db_path,
            client: QuotaClient::new(),
            in_flight: Mutex::new(HashSet::new()),
            claude_settings_path,
        }
    }

    /// 同步单个 profile 的配额。
    /// 支持 Single-Flight 防抖，同一个 profile_id 并发时直接返回或等待。
    pub async fn sync_profile(&self, profile_id: i64) -> Result<super::agg::QuotaInfo, String> {
        // Single-Flight 防抖
        {
            let mut guard = self.in_flight.lock().await;
            if guard.contains(&profile_id) {
                return Err("配额正在同步中，请勿频繁点击".to_string());
            }
            guard.insert(profile_id);
        }

        let res = self.do_sync_profile(profile_id).await;

        {
            let mut guard = self.in_flight.lock().await;
            guard.remove(&profile_id);
        }

        res
    }

    /// 核心拉取与回写逻辑
    async fn do_sync_profile(&self, profile_id: i64) -> Result<super::agg::QuotaInfo, String> {
        // 1. 读取 profile 当前的配置与鉴权信息（使用短生命周期只读查询）
        let (endpoint, quota_type, quota_unit, quota_api_url, quota_api_key) = {
            let conn = rusqlite::Connection::open(&self.db_path)
                .map_err(|e| format!("打开数据库失败: {e}"))?;
            conn.query_row(
                "SELECT endpoint, quota_type, quota_unit, quota_api_url, quota_api_key
                 FROM profiles WHERE id = ?1",
                [profile_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .map_err(|e| format!("未找到档案 ID {profile_id}: {e}"))?
        };

        if quota_type != "api" {
            return Err("当前档案未启用 API 自动配额".to_string());
        }

        // 确定用于请求的 URL
        let target_url = quota_api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(endpoint.as_str());

        // 确定用于鉴权的 Key（混合凭据与 SSRF 检查）
        let (api_key, is_inactive) =
            self.resolve_credential(&endpoint, target_url, quota_api_key.as_deref());

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        if is_inactive {
            // 非活跃且无独立 Key => 标记 inactive
            self.write_sync_result(
                profile_id,
                None,
                None,
                None,
                "inactive",
                Some("当前未激活且未配置独立 Key，请先激活或配置专属 Key"),
                now_ms,
            )?;
            return Ok(super::agg::QuotaInfo {
                quota_type,
                unit: quota_unit,
                total: None,
                used: None,
                reset_date: None,
                updated_at: Some(now_ms),
                sync_status: Some("inactive".to_string()),
                sync_error: Some("当前未激活且未配置独立 Key，请先激活或配置专属 Key".to_string()),
            });
        }

        let Some(key) = api_key else {
            let err_msg = "缺少查询 Key，跨站点必须配置专属 Key";
            self.write_sync_result(profile_id, None, None, None, "error", Some(err_msg), now_ms)?;
            return Err(err_msg.to_string());
        };

        // 2. 发起网络请求
        match self.client.fetch_quota(target_url, &key).await {
            Ok(fetched) => {
                self.write_sync_result(
                    profile_id,
                    fetched.total,
                    fetched.used,
                    fetched.reset_date.as_deref(),
                    "ok",
                    None,
                    now_ms,
                )?;
                Ok(super::agg::QuotaInfo {
                    quota_type,
                    unit: quota_unit,
                    total: fetched.total,
                    used: fetched.used,
                    reset_date: fetched.reset_date,
                    updated_at: Some(now_ms),
                    sync_status: Some("ok".to_string()),
                    sync_error: None,
                })
            }
            Err(e) => {
                self.write_sync_result(profile_id, None, None, None, "error", Some(&e), now_ms)?;
                Err(e)
            }
        }
    }

    /// 凭据借用与 SSRF 检查逻辑 (P1-3, P1-6)
    /// 返回: (Option<ApiKey>, is_inactive)
    fn resolve_credential(
        &self,
        profile_endpoint: &str,
        target_url: &str,
        custom_key: Option<&str>,
    ) -> (Option<String>, bool) {
        // 1. 若配置了独立 Key，直接使用（不受跨 Origin 借用限制）
        if let Some(k) = custom_key {
            let trimmed = k.trim();
            if !trimmed.is_empty() {
                return (Some(trimmed.to_string()), false);
            }
        }

        // 2. 若未配置独立 Key，必须借用 settings.json Key
        // P1-3: 仅当目标 target_url 的 Origin 与档案 profile_endpoint 完全相同时才允许借用！
        if !OriginInfo::is_same_origin(profile_endpoint, target_url) {
            // 跨 Origin 拒绝借用生产 Key！
            return (None, false);
        }

        // 3. 读取 settings.json 检查是否为当前活跃档案
        let (active_endpoint, active_key) = self.read_active_settings();
        let Some(cur_endpoint) = active_endpoint else {
            return (None, true); // 无法读取 settings => inactive
        };

        // P1-6: 本地代理模式下的上游识别：
        // 若 settings 是 127.0.0.1，无法直接拿 127.0.0.1 和中转站匹配
        // 但若 profile_endpoint == cur_endpoint，或者同 Origin
        let is_active = OriginInfo::is_same_origin(&cur_endpoint, profile_endpoint);
        if !is_active {
            // 非活跃档案且未填独立 Key
            return (None, true);
        }

        // 活跃档案且同 Origin，允许借用 settings Key
        (active_key, false)
    }

    fn read_active_settings(&self) -> (Option<String>, Option<String>) {
        let Ok(content) = std::fs::read_to_string(&self.claude_settings_path) else {
            return (None, None);
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
            return (None, None);
        };
        let Some(env) = json.get("env") else {
            return (None, None);
        };

        let endpoint = env
            .get("ANTHROPIC_BASE_URL")
            .and_then(|v| v.as_str())
            .unwrap_or("https://api.anthropic.com")
            .to_string();

        let key = env
            .get("ANTHROPIC_AUTH_TOKEN")
            .or_else(|| env.get("ANTHROPIC_API_KEY"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        (Some(endpoint), key)
    }

    /// 短生命周期连接写回配额结果，带在途写入保护
    #[allow(clippy::too_many_arguments)]
    fn write_sync_result(
        &self,
        profile_id: i64,
        total: Option<f64>,
        used: Option<f64>,
        reset_date: Option<&str>,
        sync_status: &str,
        sync_error: Option<&str>,
        now_ms: i64,
    ) -> Result<(), String> {
        let mut store = Store::open(&self.db_path).map_err(|e| e.to_string())?;
        store
            .save_profile_quota_sync_result(
                profile_id,
                total,
                used,
                reset_date,
                sync_status,
                sync_error,
                now_ms,
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 批量同步所有启用了 API 配额的档案（后台轮询调用）
    pub async fn sync_all_active_profiles(&self) {
        let profiles = {
            let Ok(store) = Store::open(&self.db_path) else {
                return;
            };
            store.list_api_quota_profiles().unwrap_or_default()
        };

        for pid in profiles {
            let _ = self.sync_profile(pid).await;
        }
    }
}
