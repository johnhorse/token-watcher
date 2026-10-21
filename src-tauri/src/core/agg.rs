//! T7：SQL 侧聚合层 —— Dashboard 五区块 + 托盘指标
//!
//! 口径一致性（SC7）：悬浮窗大数字与菜单栏托盘数字出自本模块同一套 SQL；
//! 聚合全部在 SQLite 内完成（GROUP BY / 聚合函数），不把 range 内全量行
//! 拉进 Rust 内存求和。形状契约见 docs/contracts/ui-data-contract.md。
//!
//! 时段口径（本地日历对齐：同一个自然日内数字不随时间漂移，且
//! today ⊂ week ⊂ month）：
//! - `today`：本地今天 00:00 起
//! - `week` ：含今天在内的本地 7 个日历日
//! - `month`：含今天在内的本地 30 个日历日
//! - `all`  ：全量
//!
//! 托盘激活会话（设计文档「菜单栏 / 托盘项」）：最近 [`ACTIVE_WINDOW_MS`]
//! 内有新 usage 记录的会话；多会话并行时取**最近一条** usage 记录所属会话
//! （不求和）。无活跃会话回退今日累计。

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use super::pricing::{PriceTable, TokenCost};
use super::store::{Store, Totals};
use super::CoreError;
/// 激活会话判定窗口（5 分钟）。
/// 与 `attr::GRACE_MS` 当前同值但语义独立：两者同属 OQ4 的可调旋钮。
pub const ACTIVE_WINDOW_MS: i64 = 5 * 60 * 1000;

/// 到达脉冲折线固定窗口：最近 1 小时（与 range 无关）
const PULSE_WINDOW_MS: i64 = 60 * 60 * 1000;

/// Unknown 虚拟卡的显示名占位；UI 依 `profile_id == null` 自行本地化
const UNKNOWN_DISPLAY_NAME: &str = "Unknown";

/// 时段筛选（`get_dashboard(range)` 入参）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Range {
    Today,
    Week,
    Month,
    All,
}

/// 托盘指标（设计文档「四指标可配」：总 / 输入 / 缓存读取 / 输出）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayMetric {
    Total,
    Input,
    CacheRead,
    Output,
    CacheRate,
}

/// 四项 token + 总和（契约 `totals` / `ProfileCard.tokens` 形状）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub total: i64,
}

impl TokenTotals {
    fn new(input: i64, output: i64, cache_read: i64, cache_creation: i64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_creation,
            total: input + output + cache_read + cache_creation,
        }
    }
}

/// 到达脉冲（契约 `usage-pulse` 事件 / `Dashboard.pulses` 元素）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UsagePulse {
    pub t_ms: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
}

/// 模型分布横条的一行
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSlice {
    pub model: String,
    pub tokens: i64,
    pub cost: Option<f64>,
}
/// 配额信息（支持手动 Token 计数与中转站 API 自动同步）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaInfo {
    pub quota_type: String,          // "manual" | "api"
    pub unit: String,                // "usd" | "tokens" | "cny"
    pub total: Option<f64>,
    pub used: Option<f64>,
    pub reset_date: Option<String>,
    pub updated_at: Option<i64>,
    pub sync_status: Option<String>, // "ok" | "error" | "inactive"
    pub sync_error: Option<String>,
}

/// 订阅档案卡片（`profile_id = null` = Unknown 虚拟卡）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileCard {
    pub profile_id: Option<i64>,
    pub display_name: String,
    pub endpoint: Option<String>,
    /// v0.1 恒 true：卡面「推断 · 低置信」标签
    pub inferred: bool,
    pub tokens: TokenTotals,
    pub quota: Option<QuotaInfo>,
    pub cost: Option<TokenCost>,
}
/// 会话列表的一行
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRow {
    pub session_id: String,
    /// cwd 尾段，缺失时用 session_id 前 8 位
    pub title: String,
    pub last_active_ms: i64,
    pub total: i64,
    /// 最近 [`ACTIVE_WINDOW_MS`] 内有新记录（与托盘同源定义）
    pub active: bool,
    /// 数据源引擎（"claude" | "codex"）
    pub engine: String,
}

/// 悬浮窗一次水合的全部数据（契约 `Dashboard`）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dashboard {
    pub range: Range,
    pub totals: TokenTotals,
    pub cost: TokenCost,
    pub pulses: Vec<UsagePulse>,
    pub models: Vec<ModelSlice>,
    pub profiles: Vec<ProfileCard>,
    pub sessions: Vec<SessionRow>,
}
/// 桌面角落 HUD 紧凑概览数据（< 1ms 轻量查询）
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HudSummary {
    /// 今日累计总 Token
    pub total_today: i64,
    /// 平均燃烧速率（tokens/min，兼容老字段）
    pub rate_per_min: i64,
    /// 当前瞬时生成速率（tokens/s）
    pub current_rate: f64,
    /// 激活会话累计总 Token
    pub session_total: i64,
    /// 激活会话当前模型
    pub current_model: String,
    /// 缓存命中率（0.0 - 100.0%）
    pub cache_rate: f64,
    /// 激活会话输入 Token 数
    pub input_tokens: i64,
    /// 激活会话输出 Token 数
    pub output_tokens: i64,
    /// 当前启用的指标键名列表
    pub hud_metrics: Vec<String>,
    /// 鼠标穿透锁定状态
    pub hud_locked: bool,
}

/// 脉冲谱带点（轻量，用于顶部谱带渲染）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PulsePoint {
    pub index: usize,
    pub timestamp_ms: i64,
    pub total_tokens: i64,
    pub cache_read_tokens: i64,
    pub is_sidechain: bool,
    pub is_spike: bool,
}

/// 单次请求事件明细
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlightRecordEvent {
    pub request_index: usize,
    pub record_id: String,
    pub timestamp_ms: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub total_tokens: i64,
    pub model: String,
    pub entrypoint: String,
    pub is_sidechain: bool,
    pub profile_id: Option<i64>,
    pub attribution_source: String,
    pub cost: TokenCost,
    pub model_switched: bool,
    pub profile_switched: bool,
    pub is_spike: bool,
    pub spike_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionModelSlice {
    pub model: String,
    pub tokens: TokenTotals,
    pub cost: TokenCost,
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionProfileSlice {
    pub profile_id: Option<i64>,
    pub display_name: String,
    pub tokens: TokenTotals,
    pub cost: TokenCost,
}

/// 会话飞行记录仪完整视图响应
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFlightRecorder {
    pub session_id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub cc_version: Option<String>,
    pub engine: String,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub duration_seconds: i64,
    pub is_active: bool,
    pub total_requests: usize,
    pub totals: TokenTotals,
    pub cost: TokenCost,
    pub cache_hit_ratio: f64,
    pub models_used: Vec<SessionModelSlice>,
    pub profiles_used: Vec<SessionProfileSlice>,
    pub pulse_strip: Vec<PulsePoint>,
    pub events: Vec<FlightRecordEvent>,
    pub events_truncated: bool,
}

/// 当前 Unix epoch 毫秒
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 聚合 Dashboard（使用系统当前时间，默认内置价目表）
pub fn dashboard(store: &Store, range: Range) -> Result<Dashboard, CoreError> {
    dashboard_at_with_prices(store, range, now_ms(), None)
}

/// 聚合 Dashboard（使用系统当前时间，支持注入外部价目表）
pub fn dashboard_with_prices(
    store: &Store,
    range: Range,
    prices: Option<&PriceTable>,
) -> Result<Dashboard, CoreError> {
    dashboard_at_with_prices(store, range, now_ms(), prices)
}
/// 同上，`now_ms` 显式传入（测试可注入固定时钟，默认使用内置兜底价目表）；
/// 脉冲窗口与「活跃」判定都以它为准。
pub fn dashboard_at(store: &Store, range: Range, now_ms: i64) -> Result<Dashboard, CoreError> {
    dashboard_at_with_prices(store, range, now_ms, None)
}

/// 支持注入外部价目表的聚合函数
pub fn dashboard_at_with_prices(
    store: &Store,
    range: Range,
    now_ms: i64,
    prices: Option<&PriceTable>,
) -> Result<Dashboard, CoreError> {
    let (start_ms, end_ms) = range_bounds(store, range, now_ms)?;
    let totals = token_totals(store.totals_between(start_ms, end_ms)?);
    let default_table;
    let prices = match prices {
        Some(p) => p,
        None => {
            default_table = PriceTable::bundled();
            &default_table
        }
    };
    let (models, priced_tokens, total_cost, unpriced_tokens) =
        models_between(store, start_ms, end_ms, prices)?;
    let profiles = profiles_between(store, start_ms, end_ms, prices)?;

    let total_tokens = totals.total;
    let coverage = if total_tokens == 0 {
        1.0
    } else {
        priced_tokens as f64 / total_tokens as f64
    };
    let cost = TokenCost {
        total: total_cost,
        coverage: if coverage.is_finite() { coverage } else { 1.0 },
        is_complete: unpriced_tokens == 0,
        unpriced_tokens,
    };

    Ok(Dashboard {
        range,
        totals,
        cost,
        pulses: recent_pulses(store, now_ms)?,
        models,
        profiles,
        sessions: sessions_between(store, start_ms, end_ms, now_ms)?,
    })
}
/// 托盘数字（使用系统当前时间）
pub fn tray_metric(store: &Store, metric: TrayMetric) -> Result<i64, CoreError> {
    tray_metric_at(store, metric, now_ms())
}

/// 托盘数字：有活跃会话 → 该会话今日内该指标合计；无 → 今日累计该指标。
/// 口径与 `dashboard(Today).totals` 同源（SC7）。
pub fn tray_metric_at(store: &Store, metric: TrayMetric, now_ms: i64) -> Result<i64, CoreError> {
    let today = local_date_at(store, now_ms)?;
    let (start_ms, end_ms) = store.local_day_bounds(&today)?;

    // 并行多会话：全局最近一条记录所属会话即激活会话（不求和）
    let latest: Option<(String, i64)> = store
        .conn()
        .query_row(
            "SELECT session_id, timestamp_ms FROM usage_records
             ORDER BY timestamp_ms DESC, record_id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    if metric == TrayMetric::CacheRate {
        let (input, cache_read): (i64, i64) = match latest {
            Some((session_id, ts_ms)) if ts_ms >= now_ms - ACTIVE_WINDOW_MS => {
                store.conn().query_row(
                    "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(cache_read_tokens), 0)
                     FROM usage_records
                     WHERE session_id = ?1 AND timestamp_ms >= ?2 AND timestamp_ms < ?3",
                    params![session_id, start_ms, end_ms],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?
            }
            _ => {
                store.conn().query_row(
                    "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(cache_read_tokens), 0)
                     FROM usage_records
                     WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2",
                    params![start_ms, end_ms],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?
            }
        };
        let denom = input + cache_read;
        return if denom > 0 {
            Ok((cache_read * 100) / denom)
        } else {
            Ok(0)
        };
    }

    let expr = metric_expr(metric);
    let value: i64 = match latest {
        Some((session_id, ts_ms)) if ts_ms >= now_ms - ACTIVE_WINDOW_MS => store.conn().query_row(
            &format!(
                "SELECT COALESCE(SUM({expr}), 0) FROM usage_records
                     WHERE session_id = ?1 AND timestamp_ms >= ?2 AND timestamp_ms < ?3"
            ),
            params![session_id, start_ms, end_ms],
            |row| row.get(0),
        )?,
        _ => store.conn().query_row(
            &format!(
                "SELECT COALESCE(SUM({expr}), 0) FROM usage_records
                 WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2"
            ),
            params![start_ms, end_ms],
            |row| row.get(0),
        )?,
    };
    Ok(value)
}

/// HUD 紧凑数据（使用系统当前时间）
pub fn hud_summary(
    store: &Store,
    hud_locked: bool,
    hud_metrics: &[String],
) -> Result<HudSummary, CoreError> {
    hud_summary_with_metrics_at(store, hud_locked, hud_metrics, now_ms())
}

/// HUD 紧凑数据（纯函数，指定时间点便于测试，采用默认指标）
pub fn hud_summary_at(
    store: &Store,
    hud_locked: bool,
    now_ms: i64,
) -> Result<HudSummary, CoreError> {
    hud_summary_with_metrics_at(store, hud_locked, &crate::app_settings::default_hud_metrics(), now_ms)
}

/// HUD 紧凑数据（带指标列表定制）
pub fn hud_summary_with_metrics_at(
    store: &Store,
    hud_locked: bool,
    hud_metrics: &[String],
    now_ms: i64,
) -> Result<HudSummary, CoreError> {
    let today = local_date_at(store, now_ms)?;
    let (start_ms, end_ms) = store.local_day_bounds(&today)?;
    let total_today = token_totals(store.totals_between(start_ms, end_ms)?).total;

    // 1. 查询最近活跃的会话及其聚合数据与模型
    let active_session_row = {
        let mut stmt = store.conn().prepare_cached(
            "SELECT session_id,
                    MAX(timestamp_ms) AS last_ts,
                    model,
                    SUM(input_tokens),
                    SUM(output_tokens),
                    SUM(cache_read_tokens),
                    SUM(input_tokens + output_tokens + cache_read_tokens + cache_creation_tokens)
             FROM usage_records
             WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2
             GROUP BY session_id
             ORDER BY last_ts DESC
             LIMIT 1",
        )?;
        stmt
            .query_row(params![start_ms, end_ms], |row| {
                let session_id: String = row.get(0)?;
                let last_ts: i64 = row.get(1)?;
                let model: String = row.get(2)?;
                let input: i64 = row.get(3)?;
                let output: i64 = row.get(4)?;
                let cache_read: i64 = row.get(5)?;
                let total: i64 = row.get(6)?;
                Ok((session_id, last_ts, model, input, output, cache_read, total))
            })
            .ok()
    };

    let (session_total, current_model, cache_rate, input_tokens, output_tokens, last_active_ts) =
        if let Some((_, last_ts, model, input, output, cache_read, total)) = active_session_row {
            let denom = input + cache_read;
            let cr = if denom > 0 {
                (cache_read as f64 / denom as f64) * 100.0
            } else {
                0.0
            };
            (total, model, cr, input, output, last_ts)
        } else {
            (0, "—".to_string(), 0.0, 0, 0, 0)
        };

    // 2. 燃烧速率 (rate_per_min) 保持 5 分钟总窗口平均（契约兼容）：
    let five_min_ago = now_ms - ACTIVE_WINDOW_MS;
    let sum_5m = token_totals(store.totals_between(five_min_ago, now_ms + 1)?).total;
    let rate_per_min = (sum_5m / 5).max(0);

    // 3. 瞬时生成速率 (current_rate, tokens/s)：
    // 仅基于活跃窗口（≤ 30s）内的 output_tokens 输出量与耗时计算，绝不掺入巨大的输入/缓存读取量。
    let is_active = last_active_ts > 0 && (now_ms - last_active_ts) <= 30_000;
    let current_rate = if is_active {
        let window_start = now_ms - 30_000;
        let mut stmt = store.conn().prepare_cached(
            "SELECT timestamp_ms, output_tokens FROM usage_records
             WHERE timestamp_ms >= ?1 AND timestamp_ms <= ?2
             ORDER BY timestamp_ms ASC",
        )?;
        let records = stmt
            .query_map(params![window_start, now_ms + 1000], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        if records.is_empty() {
            0.0
        } else {
            let total_out: i64 = records.iter().map(|(_, out)| *out).sum();
            let first_ts = records.first().unwrap().0;
            let last_ts = records.last().unwrap().0;
            let time_span_s = if records.len() > 1 && (last_ts - first_ts) >= 1000 {
                (last_ts - first_ts) as f64 / 1000.0
            } else {
                let elapsed = (now_ms - last_ts).max(1000) as f64 / 1000.0;
                elapsed.min(10.0)
            };
            if time_span_s > 0.0 {
                (total_out as f64 / time_span_s).max(0.0)
            } else {
                0.0
            }
        }
    } else {
        0.0
    };

    Ok(HudSummary {
        total_today,
        rate_per_min,
        current_rate,
        session_total,
        current_model,
        cache_rate,
        input_tokens,
        output_tokens,
        hud_metrics: hud_metrics.to_vec(),
        hud_locked,
    })
}


// ---------------------------------------------------------------------------
// 内部实现
// ---------------------------------------------------------------------------

fn token_totals(totals: Totals) -> TokenTotals {
    TokenTotals::new(
        totals.input,
        totals.output,
        totals.cache_read,
        totals.cache_creation,
    )
}

/// `now_ms` 所在的本地日历日 `YYYY-MM-DD`
fn local_date_at(store: &Store, now_ms: i64) -> Result<String, CoreError> {
    let date = store.conn().query_row(
        "SELECT date(?1 / 1000, 'unixepoch', 'localtime')",
        [now_ms],
        |row| row.get(0),
    )?;
    Ok(date)
}

/// range → `[start_ms, end_ms)`，毫秒 epoch
fn range_bounds(store: &Store, range: Range, now_ms: i64) -> Result<(i64, i64), CoreError> {
    if range == Range::All {
        return Ok((i64::MIN, i64::MAX));
    }
    let today = local_date_at(store, now_ms)?;
    let (today_start, tomorrow_start) = store.local_day_bounds(&today)?;
    let start_ms = match range {
        Range::Today => today_start,
        Range::Week => local_day_start_offset(store, &today, "-6 day")?,
        Range::Month => local_day_start_offset(store, &today, "-29 day")?,
        Range::All => unreachable!("all 已在上方提前返回"),
    };
    Ok((start_ms, tomorrow_start))
}

/// 本地日期偏移若干天后的 00:00（UTC 毫秒）；跨 DST 由 SQLite 换算保证
fn local_day_start_offset(
    store: &Store,
    local_date: &str,
    modifier: &str,
) -> Result<i64, CoreError> {
    let date: String = store.conn().query_row(
        "SELECT date(?1, ?2)",
        params![local_date, modifier],
        |row| row.get(0),
    )?;
    Ok(store.local_day_bounds(&date)?.0)
}

/// 最近 1 小时的到达脉冲，按时间升序（UI 侧顺序累计）
fn recent_pulses(store: &Store, now_ms: i64) -> Result<Vec<UsagePulse>, CoreError> {
    let mut stmt = store.conn().prepare_cached(
        "SELECT timestamp_ms, input_tokens, output_tokens,
                cache_read_tokens, cache_creation_tokens
         FROM usage_records
         WHERE timestamp_ms >= ?1 AND timestamp_ms <= ?2
         ORDER BY timestamp_ms, record_id",
    )?;
    let pulses = stmt
        .query_map(params![now_ms - PULSE_WINDOW_MS, now_ms], |row| {
            Ok(UsagePulse {
                t_ms: row.get(0)?,
                input: row.get(1)?,
                output: row.get(2)?,
                cache_read: row.get(3)?,
                cache_creation: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(pulses)
}

/// 按模型聚合，tokens 降序；tokens 为 0 的模型不出现。
/// 附带计算每个模型的估算成本，并返回 (models, priced_tokens, priced_cost, unpriced_tokens)。
fn models_between(
    store: &Store,
    start_ms: i64,
    end_ms: i64,
    prices: &PriceTable,
) -> Result<(Vec<ModelSlice>, i64, f64, i64), CoreError> {
    let mut stmt = store.conn().prepare_cached(
        "SELECT model,
                SUM(input_tokens),
                SUM(output_tokens),
                SUM(cache_read_tokens),
                SUM(cache_creation_tokens),
                SUM(input_tokens + output_tokens
                    + cache_read_tokens + cache_creation_tokens) AS tokens
         FROM usage_records
         WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2
         GROUP BY model
         HAVING tokens > 0
         ORDER BY tokens DESC, model ASC",
    )?;
    let mut priced_tokens = 0i64;
    let mut total_cost = 0.0f64;
    let mut unpriced_tokens = 0i64;

    let models = stmt
        .query_map(params![start_ms, end_ms], |row| {
            let model: String = row.get(0)?;
            let input: i64 = row.get(1)?;
            let output: i64 = row.get(2)?;
            let cache_read: i64 = row.get(3)?;
            let cache_creation: i64 = row.get(4)?;
            let tokens: i64 = row.get(5)?;

            let unit = prices.get_unit_price(&model);
            let cost = unit.map(|u| u.calculate(input, output, cache_read, cache_creation));
            Ok((model, tokens, cost))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut slices = Vec::with_capacity(models.len());
    for (model, tokens, cost) in models {
        match cost {
            Some(c) => {
                priced_tokens += tokens;
                total_cost += c;
            }
            None => {
                unpriced_tokens += tokens;
            }
        }
        slices.push(ModelSlice {
            model,
            tokens,
            cost,
        });
    }

    Ok((slices, priced_tokens, total_cost, unpriced_tokens))
}

/// 计算每个 profile_id 的模型细分成本
fn profile_costs_between(
    store: &Store,
    start_ms: i64,
    end_ms: i64,
    prices: &PriceTable,
) -> Result<std::collections::HashMap<Option<i64>, TokenCost>, CoreError> {
    let mut stmt = store.conn().prepare_cached(
        "SELECT r.profile_id,
                r.model,
                SUM(r.input_tokens),
                SUM(r.output_tokens),
                SUM(r.cache_read_tokens),
                SUM(r.cache_creation_tokens),
                SUM(r.input_tokens + r.output_tokens
                    + r.cache_read_tokens + r.cache_creation_tokens) AS tokens
         FROM usage_records r
         WHERE r.timestamp_ms >= ?1 AND r.timestamp_ms < ?2
         GROUP BY r.profile_id, r.model
         HAVING tokens > 0",
    )?;

    let mut map: std::collections::HashMap<Option<i64>, (f64, i64, i64)> =
        std::collections::HashMap::new();

    let rows = stmt.query_map(params![start_ms, end_ms], |row| {
        let profile_id: Option<i64> = row.get(0)?;
        let model: String = row.get(1)?;
        let input: i64 = row.get(2)?;
        let output: i64 = row.get(3)?;
        let cache_read: i64 = row.get(4)?;
        let cache_creation: i64 = row.get(5)?;
        let tokens: i64 = row.get(6)?;
        Ok((profile_id, model, input, output, cache_read, cache_creation, tokens))
    })?;

    for item in rows {
        let (profile_id, model, input, output, cache_read, cache_creation, tokens) = item?;
        let entry = map.entry(profile_id).or_insert((0.0, 0, 0));
        if let Some(unit) = prices.get_unit_price(&model) {
            entry.0 += unit.calculate(input, output, cache_read, cache_creation);
            entry.1 += tokens;
        } else {
            entry.2 += tokens;
        }
    }

    let mut result = std::collections::HashMap::new();
    for (profile_id, (cost, priced, unpriced)) in map {
        let total = priced + unpriced;
        let coverage = if total == 0 {
            1.0
        } else {
            priced as f64 / total as f64
        };
        result.insert(
            profile_id,
            TokenCost {
                total: cost,
                coverage: if coverage.is_finite() { coverage } else { 1.0 },
                is_complete: unpriced == 0,
                unpriced_tokens: unpriced,
            },
        );
    }

    Ok(result)
}

/// 按档案聚合：非 Unknown 按 tokens 降序在前，Unknown 虚拟卡固定最后。
/// tokens 为 0 的档案（含 Unknown）不出现。
fn profiles_between(
    store: &Store,
    start_ms: i64,
    end_ms: i64,
    prices: &PriceTable,
) -> Result<Vec<ProfileCard>, CoreError> {
    let profile_costs = profile_costs_between(store, start_ms, end_ms, prices)?;
    let mut stmt = store.conn().prepare_cached(
        "SELECT r.profile_id,
                p.display_name, p.endpoint,
                COALESCE(p.quota_total, p.quota_manual_total) AS quota_total,
                COALESCE(p.quota_used, p.quota_manual_used) AS quota_used,
                p.quota_reset_date,
                COALESCE(p.quota_type, 'manual') AS quota_type,
                COALESCE(p.quota_unit, CASE WHEN p.quota_manual_total IS NOT NULL THEN 'tokens' ELSE 'usd' END) AS quota_unit,
                p.quota_updated_at,
                p.quota_sync_status,
                p.quota_sync_error,
                SUM(r.input_tokens), SUM(r.output_tokens),
                SUM(r.cache_read_tokens), SUM(r.cache_creation_tokens),
                SUM(r.input_tokens + r.output_tokens
                    + r.cache_read_tokens + r.cache_creation_tokens) AS tokens
         FROM usage_records r
         LEFT JOIN profiles p ON p.id = r.profile_id
         WHERE r.timestamp_ms >= ?1 AND r.timestamp_ms < ?2
         GROUP BY r.profile_id
         HAVING tokens > 0
         ORDER BY (r.profile_id IS NULL) ASC, tokens DESC, r.profile_id ASC",
    )?;
    let cards = stmt
        .query_map(params![start_ms, end_ms], |row| {
            let profile_id: Option<i64> = row.get(0)?;
            let display_name: Option<String> = row.get(1)?;
            let endpoint: Option<String> = row.get(2)?;
            let quota_total: Option<f64> = row.get(3)?;
            let quota_used: Option<f64> = row.get(4)?;
            let quota_reset_date: Option<String> = row.get(5)?;
            let quota_type: String = row.get(6)?;
            let quota_unit: String = row.get(7)?;
            let quota_updated_at: Option<i64> = row.get(8)?;
            let quota_sync_status: Option<String> = row.get(9)?;
            let quota_sync_error: Option<String> = row.get(10)?;
            let tokens = TokenTotals::new(row.get(11)?, row.get(12)?, row.get(13)?, row.get(14)?);
            // 未配置：若为 manual 且 total, used, reset_date 均为 NULL，则为 None
            let quota = if profile_id.is_none()
                || (quota_type == "manual"
                    && quota_total.is_none()
                    && quota_used.is_none()
                    && quota_reset_date.is_none())
            {
                None
            } else {
                Some(QuotaInfo {
                    quota_type,
                    unit: quota_unit,
                    total: quota_total,
                    used: quota_used,
                    reset_date: quota_reset_date,
                    updated_at: quota_updated_at,
                    sync_status: quota_sync_status,
                    sync_error: quota_sync_error,
                })
            };
            let cost = profile_costs.get(&profile_id).copied();
            Ok(ProfileCard {
                profile_id,
                display_name: display_name.unwrap_or_else(|| UNKNOWN_DISPLAY_NAME.to_string()),
                endpoint,
                inferred: true,
                tokens,
                quota,
                cost,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cards)
}

/// 按会话聚合，按最后活跃降序；tokens 为 0 的会话不出现。
/// `cwd` 是裸列：SQLite 规定查询里恰好一个 max()/min() 聚合时，
/// 裸列取该极值行的值 —— 这里拿的就是最后一条记录的 cwd。
fn sessions_between(
    store: &Store,
    start_ms: i64,
    end_ms: i64,
    now_ms: i64,
) -> Result<Vec<SessionRow>, CoreError> {
    let mut stmt = store.conn().prepare_cached(
        "SELECT session_id,
                MAX(timestamp_ms) AS last_active_ms,
                cwd,
                SUM(input_tokens + output_tokens
                    + cache_read_tokens + cache_creation_tokens) AS total,
                record_id,
                entrypoint
         FROM usage_records
         WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2
         GROUP BY session_id
         HAVING total > 0
         ORDER BY last_active_ms DESC, session_id ASC",
    )?;
    let sessions = stmt
        .query_map(params![start_ms, end_ms], |row| {
            let session_id: String = row.get(0)?;
            let last_active_ms: i64 = row.get(1)?;
            let cwd: Option<String> = row.get(2)?;
            let total: i64 = row.get(3)?;
            let record_id: String = row.get(4)?;
            let entrypoint: String = row.get(5)?;
            let engine = detect_engine(&record_id, &entrypoint).to_string();
            Ok(SessionRow {
                title: session_title(&session_id, cwd.as_deref()),
                session_id,
                last_active_ms,
                total,
                active: last_active_ms >= now_ms - ACTIVE_WINDOW_MS,
                engine,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sessions)
}

/// 托盘指标对应的 SQL 列表达式；静态字符串，无注入面
fn metric_expr(metric: TrayMetric) -> &'static str {
    match metric {
        TrayMetric::Total => {
            "(input_tokens + output_tokens + cache_read_tokens + cache_creation_tokens)"
        }
        TrayMetric::Input => "input_tokens",
        TrayMetric::CacheRead => "cache_read_tokens",
        TrayMetric::Output => "output_tokens",
        TrayMetric::CacheRate => "0",
    }
}

/// 会话标题：cwd 尾段（兼容 `/` 与 `\`），缺失/为空时取 id 前 8 位
fn session_title(session_id: &str, cwd: Option<&str>) -> String {
    let tail = cwd
        .map(|c| c.trim_end_matches(['/', '\\']))
        .and_then(|c| c.rsplit(['/', '\\']).next())
        .unwrap_or("");
    if tail.is_empty() {
        if let Some(rest) = session_id.strip_prefix("aider:") {
            format!("aider-{}", rest.chars().take(8).collect::<String>())
        } else {
            session_id.chars().take(8).collect()
        }
    } else {
        tail.to_string()
    }
}

/// 判断记录所属数据源引擎（`"claude"` | `"codex"` | `"cline"` | `"roo"` | `"aider"` | `"cursor"` | `"opencode"` | `"continue"` | `"omp"`）
pub fn detect_engine(record_id: &str, entrypoint: &str) -> &'static str {
    if record_id.starts_with("codex:") || entrypoint.starts_with("codex") {
        "codex"
    } else if record_id.starts_with("cline:") || entrypoint.starts_with("cline") {
        "cline"
    } else if record_id.starts_with("roo:") || entrypoint.starts_with("roo") {
        "roo"
    } else if record_id.starts_with("aider:") || entrypoint.starts_with("aider") {
        "aider"
    } else if record_id.starts_with("cursor:") || entrypoint.starts_with("cursor") {
        "cursor"
    } else if record_id.starts_with("opencode:") || entrypoint.starts_with("opencode") {
        "opencode"
    } else if record_id.starts_with("continue:") || entrypoint.starts_with("continue") {
        "continue"
    } else if record_id.starts_with("omp:") || entrypoint.starts_with("omp") {
        "omp"
    } else {
        "claude"
    }
}

/// 查询单个会话的完整飞行记录（Flight Recorder）
pub fn session_flight_recorder(
    store: &Store,
    prices: Option<&PriceTable>,
    session_id: &str,
    limit: Option<usize>,
    now_ms: i64,
) -> Result<Option<SessionFlightRecorder>, CoreError> {
    let default_table;
    let prices = match prices {
        Some(p) => p,
        None => {
            default_table = PriceTable::bundled();
            &default_table
        }
    };

    struct RawFlightRow {
        record_id: String,
        timestamp_ms: i64,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_tokens: i64,
        cache_creation_tokens: i64,
        model: String,
        entrypoint: String,
        is_sidechain: bool,
        cwd: Option<String>,
        cc_version: Option<String>,
        profile_id: Option<i64>,
        display_name: Option<String>,
        attribution_source: String,
    }

    let mut stmt = store.conn().prepare_cached(
        "SELECT r.record_id, r.timestamp_ms, r.input_tokens, r.output_tokens,
                r.cache_read_tokens, r.cache_creation_tokens,
                r.model, r.entrypoint, r.is_sidechain, r.cwd, r.cc_version,
                r.profile_id, p.display_name, r.attribution_source
         FROM usage_records r
         LEFT JOIN profiles p ON r.profile_id = p.id
         WHERE r.session_id = ?1
         ORDER BY r.timestamp_ms ASC, r.record_id ASC",
    )?;

    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(RawFlightRow {
                record_id: row.get(0)?,
                timestamp_ms: row.get(1)?,
                input_tokens: row.get(2)?,
                output_tokens: row.get(3)?,
                cache_read_tokens: row.get(4)?,
                cache_creation_tokens: row.get(5)?,
                model: row.get(6)?,
                entrypoint: row.get(7)?,
                is_sidechain: row.get::<_, i64>(8)? != 0,
                cwd: row.get(9)?,
                cc_version: row.get(10)?,
                profile_id: row.get(11)?,
                display_name: row.get(12)?,
                attribution_source: row.get(13)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if rows.is_empty() {
        return Ok(None);
    }

    let total_requests = rows.len();
    let start_time_ms = rows.first().unwrap().timestamp_ms;
    let end_time_ms = rows.last().unwrap().timestamp_ms;
    let duration_seconds = ((end_time_ms - start_time_ms) / 1000).max(0);
    let is_active = (now_ms - end_time_ms) <= ACTIVE_WINDOW_MS;
    let last_cwd = rows.iter().rev().find_map(|r| r.cwd.clone());
    let last_cc_version = rows.iter().rev().find_map(|r| r.cc_version.clone());
    let title = session_title(session_id, last_cwd.as_deref());

    let mut input_sum: i64 = 0;
    let mut output_sum: i64 = 0;
    let mut cache_read_sum: i64 = 0;
    let mut cache_creation_sum: i64 = 0;

    for r in &rows {
        input_sum += r.input_tokens;
        output_sum += r.output_tokens;
        cache_read_sum += r.cache_read_tokens;
        cache_creation_sum += r.cache_creation_tokens;
    }
    let totals = TokenTotals::new(input_sum, output_sum, cache_read_sum, cache_creation_sum);
    let cache_hit_ratio = if input_sum + cache_read_sum == 0 {
        0.0
    } else {
        cache_read_sum as f64 / (input_sum + cache_read_sum) as f64
    };

    let mean_tokens = if total_requests == 0 {
        0.0
    } else {
        totals.total as f64 / total_requests as f64
    };

    let mut model_map: std::collections::BTreeMap<String, (TokenTotals, f64, i64, i64)> =
        std::collections::BTreeMap::new();
    let mut profile_map: std::collections::BTreeMap<Option<i64>, (String, TokenTotals, f64, i64, i64)> =
        std::collections::BTreeMap::new();

    let mut pulse_strip = Vec::with_capacity(total_requests);
    let mut all_events = Vec::with_capacity(total_requests);

    let mut prev_main_model: Option<String> = None;
    let mut prev_main_profile: Option<Option<i64>> = None;

    let mut session_cost_sum = 0.0;
    let mut session_priced_tokens = 0i64;
    let mut session_unpriced_tokens = 0i64;

    let engine = rows
        .first()
        .map(|r| detect_engine(&r.record_id, &r.entrypoint).to_string())
        .unwrap_or_else(|| "claude".to_string());

    for (idx, r) in rows.into_iter().enumerate() {
        let row_total = r.input_tokens + r.output_tokens + r.cache_read_tokens + r.cache_creation_tokens;
        let is_spike = row_total >= 10_000 && (row_total as f64) >= 2.5 * mean_tokens;
        let spike_reason = if is_spike {
            if r.input_tokens >= row_total / 2 && r.cache_read_tokens < r.input_tokens {
                Some("未缓存输入主导".to_string())
            } else if r.cache_creation_tokens >= row_total / 3 {
                Some("缓存写入/上下文填充".to_string())
            } else if r.output_tokens >= row_total / 3 {
                Some("长文本生成".to_string())
            } else if r.cache_read_tokens >= row_total / 2 {
                Some("缓存命中复用".to_string())
            } else {
                Some("吞吐突刺".to_string())
            }
        } else {
            None
        };

        let unit = prices.get_unit_price(&r.model);
        let cost = match unit {
            Some(u) => {
                let total = u.calculate(
                    r.input_tokens,
                    r.output_tokens,
                    r.cache_read_tokens,
                    r.cache_creation_tokens,
                );
                TokenCost {
                    total,
                    coverage: 1.0,
                    is_complete: true,
                    unpriced_tokens: 0,
                }
            }
            None => TokenCost {
                total: 0.0,
                coverage: 0.0,
                is_complete: false,
                unpriced_tokens: row_total,
            },
        };

        session_cost_sum += cost.total;
        if cost.is_complete {
            session_priced_tokens += row_total;
        } else {
            session_unpriced_tokens += cost.unpriced_tokens;
            session_priced_tokens += row_total - cost.unpriced_tokens;
        }

        let (model_switched, profile_switched) = if !r.is_sidechain {
            let m_sw = prev_main_model.as_ref().is_some_and(|m| m != &r.model);
            let p_sw = prev_main_profile.as_ref().is_some_and(|p| p != &r.profile_id);
            prev_main_model = Some(r.model.clone());
            prev_main_profile = Some(r.profile_id);
            (m_sw, p_sw)
        } else {
            (false, false)
        };

        // 聚合模型切片
        {
            let entry = model_map
                .entry(r.model.clone())
                .or_insert_with(|| (TokenTotals::default(), 0.0, 0, 0));
            entry.0.input += r.input_tokens;
            entry.0.output += r.output_tokens;
            entry.0.cache_read += r.cache_read_tokens;
            entry.0.cache_creation += r.cache_creation_tokens;
            entry.0.total += row_total;
            entry.1 += cost.total;
            if cost.is_complete {
                entry.2 += row_total;
            } else {
                entry.3 += cost.unpriced_tokens;
                entry.2 += row_total - cost.unpriced_tokens;
            }
        }

        // 聚合档案切片
        {
            let d_name = r
                .display_name
                .clone()
                .unwrap_or_else(|| UNKNOWN_DISPLAY_NAME.to_string());
            let entry = profile_map
                .entry(r.profile_id)
                .or_insert_with(|| (d_name, TokenTotals::default(), 0.0, 0, 0));
            entry.1.input += r.input_tokens;
            entry.1.output += r.output_tokens;
            entry.1.cache_read += r.cache_read_tokens;
            entry.1.cache_creation += r.cache_creation_tokens;
            entry.1.total += row_total;
            entry.2 += cost.total;
            if cost.is_complete {
                entry.3 += row_total;
            } else {
                entry.4 += cost.unpriced_tokens;
                entry.3 += row_total - cost.unpriced_tokens;
            }
        }

        pulse_strip.push(PulsePoint {
            index: idx + 1,
            timestamp_ms: r.timestamp_ms,
            total_tokens: row_total,
            cache_read_tokens: r.cache_read_tokens,
            is_sidechain: r.is_sidechain,
            is_spike,
        });

        all_events.push(FlightRecordEvent {
            request_index: idx + 1,
            record_id: r.record_id,
            timestamp_ms: r.timestamp_ms,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cache_read_tokens: r.cache_read_tokens,
            cache_creation_tokens: r.cache_creation_tokens,
            total_tokens: row_total,
            model: r.model,
            entrypoint: r.entrypoint,
            is_sidechain: r.is_sidechain,
            profile_id: r.profile_id,
            attribution_source: r.attribution_source,
            cost,
            model_switched,
            profile_switched,
            is_spike,
            spike_reason,
        });
    }

    let max_events = limit.unwrap_or(200);
    let (events, events_truncated) = if all_events.len() > max_events {
        let start_idx = all_events.len() - max_events;
        (all_events[start_idx..].to_vec(), true)
    } else {
        (all_events, false)
    };

    let mut models_used = Vec::new();
    for (model, (m_tokens, m_cost, m_priced, m_unpriced)) in model_map {
        let coverage = if m_tokens.total == 0 {
            1.0
        } else {
            m_priced as f64 / m_tokens.total as f64
        };
        let cost = TokenCost {
            total: m_cost,
            coverage: if coverage.is_finite() { coverage } else { 1.0 },
            is_complete: m_unpriced == 0,
            unpriced_tokens: m_unpriced,
        };
        let share = if totals.total == 0 {
            0.0
        } else {
            m_tokens.total as f64 / totals.total as f64
        };
        models_used.push(SessionModelSlice {
            model,
            tokens: m_tokens,
            cost,
            share,
        });
    }
    models_used.sort_by_key(|a| std::cmp::Reverse(a.tokens.total));

    let mut profiles_used = Vec::new();
    for (profile_id, (display_name, p_tokens, p_cost, p_priced, p_unpriced)) in profile_map {
        let coverage = if p_tokens.total == 0 {
            1.0
        } else {
            p_priced as f64 / p_tokens.total as f64
        };
        let cost = TokenCost {
            total: p_cost,
            coverage: if coverage.is_finite() { coverage } else { 1.0 },
            is_complete: p_unpriced == 0,
            unpriced_tokens: p_unpriced,
        };
        profiles_used.push(SessionProfileSlice {
            profile_id,
            display_name,
            tokens: p_tokens,
            cost,
        });
    }
    profiles_used.sort_by_key(|a| std::cmp::Reverse(a.tokens.total));

    let session_coverage = if totals.total == 0 {
        1.0
    } else {
        session_priced_tokens as f64 / totals.total as f64
    };
    let cost = TokenCost {
        total: session_cost_sum,
        coverage: if session_coverage.is_finite() { session_coverage } else { 1.0 },
        is_complete: session_unpriced_tokens == 0,
        unpriced_tokens: session_unpriced_tokens,
    };

    Ok(Some(SessionFlightRecorder {
        session_id: session_id.to_string(),
        title,
        cwd: last_cwd,
        cc_version: last_cc_version,
        engine,
        start_time_ms,
        end_time_ms,
        duration_seconds,
        is_active,
        total_requests,
        totals,
        cost,
        cache_hit_ratio,
        models_used,
        profiles_used,
        pulse_strip,
        events,
        events_truncated,
    }))
}
