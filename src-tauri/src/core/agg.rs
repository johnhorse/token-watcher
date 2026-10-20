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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayMetric {
    Total,
    Input,
    CacheRead,
    Output,
}

/// 四项 token + 总和（契约 `totals` / `ProfileCard.tokens` 形状）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelSlice {
    pub model: String,
    pub tokens: i64,
}

/// 手动额度（R2-2：三列全 NULL = 未配置，卡面不显示配额条）
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuotaInfo {
    pub total: Option<i64>,
    pub used: Option<i64>,
    pub reset_date: Option<String>,
}

/// 订阅档案卡片（`profile_id = null` = Unknown 虚拟卡）
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileCard {
    pub profile_id: Option<i64>,
    pub display_name: String,
    pub endpoint: Option<String>,
    /// v0.1 恒 true：卡面「推断 · 低置信」标签
    pub inferred: bool,
    pub tokens: TokenTotals,
    pub quota: Option<QuotaInfo>,
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
}

/// 悬浮窗一次水合的全部数据（契约 `Dashboard`）
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dashboard {
    pub range: Range,
    pub totals: TokenTotals,
    pub pulses: Vec<UsagePulse>,
    pub models: Vec<ModelSlice>,
    pub profiles: Vec<ProfileCard>,
    pub sessions: Vec<SessionRow>,
}

/// 桌面角落 HUD 紧凑概览数据（< 1ms 轻量查询）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HudSummary {
    /// 今日累计总 Token
    pub total_today: i64,
    /// 最近 5 分钟平均燃烧速率（tokens/min）
    pub rate_per_min: i64,
    /// 鼠标穿透锁定状态
    pub hud_locked: bool,
}

/// 当前 Unix epoch 毫秒
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 聚合 Dashboard（使用系统当前时间）
pub fn dashboard(store: &Store, range: Range) -> Result<Dashboard, CoreError> {
    dashboard_at(store, range, now_ms())
}

/// 同上，`now_ms` 显式传入（测试可注入固定时钟）；
/// 脉冲窗口与「活跃」判定都以它为准。
pub fn dashboard_at(store: &Store, range: Range, now_ms: i64) -> Result<Dashboard, CoreError> {
    let (start_ms, end_ms) = range_bounds(store, range, now_ms)?;
    Ok(Dashboard {
        range,
        totals: token_totals(store.totals_between(start_ms, end_ms)?),
        pulses: recent_pulses(store, now_ms)?,
        models: models_between(store, start_ms, end_ms)?,
        profiles: profiles_between(store, start_ms, end_ms)?,
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
    let expr = metric_expr(metric);

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
pub fn hud_summary(store: &Store, hud_locked: bool) -> Result<HudSummary, CoreError> {
    hud_summary_at(store, hud_locked, now_ms())
}

/// HUD 紧凑数据（纯函数，指定时间点便于测试）
pub fn hud_summary_at(
    store: &Store,
    hud_locked: bool,
    now_ms: i64,
) -> Result<HudSummary, CoreError> {
    let today = local_date_at(store, now_ms)?;
    let (start_ms, end_ms) = store.local_day_bounds(&today)?;
    let total_today = token_totals(store.totals_between(start_ms, end_ms)?).total;
    let five_min_ago = now_ms - ACTIVE_WINDOW_MS;
    let sum_5m = token_totals(store.totals_between(five_min_ago, now_ms + 1)?).total;
    let rate_per_min = (sum_5m / 5).max(0);
    Ok(HudSummary {
        total_today,
        rate_per_min,
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

/// 按模型聚合，tokens 降序；tokens 为 0 的模型不出现
fn models_between(store: &Store, start_ms: i64, end_ms: i64) -> Result<Vec<ModelSlice>, CoreError> {
    let mut stmt = store.conn().prepare_cached(
        "SELECT model,
                SUM(input_tokens + output_tokens
                    + cache_read_tokens + cache_creation_tokens) AS tokens
         FROM usage_records
         WHERE timestamp_ms >= ?1 AND timestamp_ms < ?2
         GROUP BY model
         HAVING tokens > 0
         ORDER BY tokens DESC, model ASC",
    )?;
    let models = stmt
        .query_map(params![start_ms, end_ms], |row| {
            Ok(ModelSlice {
                model: row.get(0)?,
                tokens: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(models)
}

/// 按档案聚合：非 Unknown 按 tokens 降序在前，Unknown 虚拟卡固定最后。
/// tokens 为 0 的档案（含 Unknown）不出现。
fn profiles_between(
    store: &Store,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<ProfileCard>, CoreError> {
    let mut stmt = store.conn().prepare_cached(
        "SELECT r.profile_id,
                p.display_name, p.endpoint,
                p.quota_manual_total, p.quota_manual_used, p.quota_reset_date,
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
            let quota_total: Option<i64> = row.get(3)?;
            let quota_used: Option<i64> = row.get(4)?;
            let quota_reset_date: Option<String> = row.get(5)?;
            let tokens = TokenTotals::new(row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?);
            // 三列全 NULL = 未配置（R2-2），Unknown 卡自然也是 None
            let quota =
                if quota_total.is_none() && quota_used.is_none() && quota_reset_date.is_none() {
                    None
                } else {
                    Some(QuotaInfo {
                        total: quota_total,
                        used: quota_used,
                        reset_date: quota_reset_date,
                    })
                };
            Ok(ProfileCard {
                profile_id,
                display_name: display_name.unwrap_or_else(|| UNKNOWN_DISPLAY_NAME.to_string()),
                endpoint,
                inferred: true,
                tokens,
                quota,
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
                    + cache_read_tokens + cache_creation_tokens) AS total
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
            Ok(SessionRow {
                title: session_title(&session_id, cwd.as_deref()),
                session_id,
                last_active_ms,
                total: row.get(3)?,
                active: last_active_ms >= now_ms - ACTIVE_WINDOW_MS,
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
    }
}

/// 会话标题：cwd 尾段（兼容 `/` 与 `\`），缺失/为空时取 id 前 8 位
fn session_title(session_id: &str, cwd: Option<&str>) -> String {
    let tail = cwd
        .map(|c| c.trim_end_matches(['/', '\\']))
        .and_then(|c| c.rsplit(['/', '\\']).next())
        .unwrap_or("");
    if tail.is_empty() {
        session_id.chars().take(8).collect()
    } else {
        tail.to_string()
    }
}
