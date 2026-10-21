//! 成本估算价格表管理与计价引擎
//!
//! - 内置兜底（bundled_prices.json）：覆盖主流模型，无网 100% 可用；
//! - 磁盘缓存（~/.token-watcher/pricing.json）：每 1 小时异步静默更新；
//! - 阶梯匹配与容错：剥离提供商前缀、动态解析家族最新版本、脏条目跳过、条目数低于 1000 拒绝换入；
//! - 诚实降级：未定价模型绝不假定 $0，严格计入 unpriced_tokens，is_complete=false。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// 编译期嵌入的最小核心价目表（Claude / OpenAI / DeepSeek / Gemini / GLM 主流核心模型）
pub const BUNDLED_PRICES_JSON: &str = include_str!("bundled_prices.json");

/// LiteLLM 远程价格全量 JSON 地址
pub const LITELLM_PRICES_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// 远程表最低健康条目数阈值（防止空响应或 schema 剧变清空价格表）
pub const MIN_VALID_MODELS: usize = 1000;

/// 磁盘缓存文件名
pub const PRICING_FILE_NAME: &str = "pricing.json";

/// 后台刷新周期（1 小时）
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(3600);

/// 单个模型的四项 Token 单价（单位：USD / Token）
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UnitPrice {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_read_input_token_cost: f64,
    pub cache_creation_input_token_cost: f64,
}

impl UnitPrice {
    /// 计算四项 Token 的总花费（美元 USD）
    #[inline]
    pub fn calculate(&self, input: i64, output: i64, cache_read: i64, cache_creation: i64) -> f64 {
        let cost = (input as f64) * self.input_cost_per_token
            + (output as f64) * self.output_cost_per_token
            + (cache_read as f64) * self.cache_read_input_token_cost
            + (cache_creation as f64) * self.cache_creation_input_token_cost;
        if cost.is_finite() && cost >= 0.0 {
            cost
        } else {
            0.0
        }
    }
}

/// Token 成本估算汇总（线上传输契约）
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenCost {
    /// 估算总花费（美元 USD）
    pub total: f64,
    /// 覆盖率（0.0 ~ 1.0，如 1.0 表示 100% 的 Token 已匹配到单价）
    pub coverage: f64,
    /// 是否全额已定价（unpriced_tokens == 0，杜绝浮点 epsilon 歧义）
    pub is_complete: bool,
    /// 未定价 Token 数量
    pub unpriced_tokens: i64,
}

impl Default for TokenCost {
    fn default() -> Self {
        Self {
            total: 0.0,
            coverage: 1.0,
            is_complete: true,
            unpriced_tokens: 0,
        }
    }
}

/// 内存价格表与模型名称规范化索引
#[derive(Debug, Clone)]
pub struct PriceTable {
    /// 标准化键名 (lowercase) -> 单价
    prices: HashMap<String, UnitPrice>,
    /// 家族名 (如 "claude-3-5-sonnet") -> 当前表中最新带日期的具体 key (如 "claude-3-5-sonnet-20241022")
    family_latest: HashMap<String, String>,
}

impl Default for PriceTable {
    fn default() -> Self {
        Self::bundled()
    }
}

impl PriceTable {
    /// 构建包含内置兜底价目表的 PriceTable
    pub fn bundled() -> Self {
        Self::from_json(BUNDLED_PRICES_JSON, 1).unwrap_or_else(|_| Self {
            prices: HashMap::new(),
            family_latest: HashMap::new(),
        })
    }

    /// 从 LiteLLM 风格的 JSON 字符串解析价格表
    ///
    /// - 容忍未知字段；
    /// - 跳过缺失 `input_cost_per_token` 或单价为负数的非法条目；
    /// - 检查解析出的有效模型数是否满足 `min_models`。
    pub fn from_json(json_str: &str, min_models: usize) -> Result<Self, String> {
        let raw_map: HashMap<String, serde_json::Value> =
            serde_json::from_str(json_str).map_err(|e| format!("JSON 反序列化失败: {e}"))?;

        let mut prices = HashMap::new();
        let mut family_latest: HashMap<String, String> = HashMap::new();

        for (key, val) in raw_map {
            // 过滤 LiteLLM 的 sample_spec 或通配符
            if key == "sample_spec" || key.ends_with('/') || !val.is_object() {
                continue;
            }

            let input_cost = match val.get("input_cost_per_token").and_then(|v| v.as_f64()) {
                Some(c) if c.is_finite() && c >= 0.0 => c,
                _ => continue, // input_cost 缺失或非有限正数，跳过该条目
            };

            let output_cost = val
                .get("output_cost_per_token")
                .and_then(|v| v.as_f64())
                .filter(|c| c.is_finite() && *c >= 0.0)
                .unwrap_or(0.0);

            let lower_key = key.to_ascii_lowercase();
            let is_anthropic = lower_key.contains("claude") || lower_key.contains("anthropic");

            // Cache Read: Anthropic 默认 input*0.1; 其它提供商默认等于 input (无读折扣)
            let cache_read = val
                .get("cache_read_input_token_cost")
                .and_then(|v| v.as_f64())
                .filter(|c| c.is_finite() && *c >= 0.0)
                .unwrap_or(if is_anthropic {
                    input_cost * 0.1
                } else {
                    input_cost
                });

            // Cache Creation: Anthropic 专有 input*1.25; 其它提供商为 0.0
            let cache_creation = val
                .get("cache_creation_input_token_cost")
                .and_then(|v| v.as_f64())
                .filter(|c| c.is_finite() && *c >= 0.0)
                .unwrap_or(if is_anthropic { input_cost * 1.25 } else { 0.0 });

            let unit = UnitPrice {
                input_cost_per_token: input_cost,
                output_cost_per_token: output_cost,
                cache_read_input_token_cost: cache_read,
                cache_creation_input_token_cost: cache_creation,
            };

            // 注册原始小写 key 与剥离前缀的 key
            let stripped_key = strip_provider_prefix(&lower_key).to_string();
            prices.insert(lower_key.clone(), unit);
            if stripped_key != lower_key {
                prices.entry(stripped_key.clone()).or_insert(unit);
            }

            // 维护家族最新候选索引（按日期后缀 YYYYMMDD 或 YYYY-MM-DD 提取）
            if let Some((family, date_suffix)) = extract_family_and_date(&stripped_key) {
                match family_latest.get(&family) {
                    Some(existing_key) => {
                        if let Some((_, existing_date)) = extract_family_and_date(existing_key) {
                            if date_suffix > existing_date {
                                family_latest.insert(family, stripped_key);
                            }
                        }
                    }
                    None => {
                        family_latest.insert(family, stripped_key);
                    }
                }
            }
        }

        if prices.len() < min_models {
            return Err(format!(
                "有效模型条目过少 ({} < {})，拒绝换入",
                prices.len(),
                min_models
            ));
        }

        Ok(Self {
            prices,
            family_latest,
        })
    }

    /// 模型单价阶梯查询（Fallthrough Lookup）
    ///
    /// 1. 精确匹配（小写）；
    /// 2. 剥离提供商前缀（如 `anthropic/`、`openai/`）后匹配；
    /// 3. 动态家族最新版本匹配（如 `claude-sonnet` -> 最新带日期 sonnet）；
    /// 4. 未命中返回 None（诚实标记为未定价）。
    pub fn get_unit_price(&self, model: &str) -> Option<UnitPrice> {
        let clean = model.trim().to_ascii_lowercase();
        if clean.is_empty() {
            return None;
        }

        // 1. 精确匹配
        if let Some(&price) = self.prices.get(&clean) {
            return Some(price);
        }

        // 2. 剥离 provider 前缀匹配
        let stripped = strip_provider_prefix(&clean);
        if let Some(&price) = self.prices.get(stripped) {
            return Some(price);
        }

        // 3. 动态家族别名匹配
        if let Some(target_key) = self.resolve_canonical_family(stripped) {
            if let Some(&price) = self.prices.get(&target_key) {
                return Some(price);
            }
        }

        None
    }

    /// 动态家族别名解析
    fn resolve_canonical_family(&self, name: &str) -> Option<String> {
        // 先直接查家族最新索引 (如 "claude-3-5-sonnet")
        if let Some(target) = self.family_latest.get(name) {
            return Some(target.clone());
        }

        // 裸名泛化归一（如 "claude-sonnet" -> 优先 3.7 sonnet，其次 3.5 sonnet）
        match name {
            "claude-sonnet" => self
                .family_latest
                .get("claude-3-7-sonnet")
                .or_else(|| self.family_latest.get("claude-3-5-sonnet"))
                .cloned(),
            "claude-opus" => self
                .family_latest
                .get("claude-3-opus")
                .or_else(|| self.family_latest.get("claude-opus"))
                .cloned(),
            "claude-haiku" => self
                .family_latest
                .get("claude-3-5-haiku")
                .or_else(|| self.family_latest.get("claude-3-haiku"))
                .cloned(),
            "claude-3-sonnet" => self.family_latest.get("claude-3-sonnet").cloned(),
            "claude-3-5-sonnet" => self.family_latest.get("claude-3-5-sonnet").cloned(),
            "claude-3-7-sonnet" => self.family_latest.get("claude-3-7-sonnet").cloned(),
            "claude-3-opus" => self.family_latest.get("claude-3-opus").cloned(),
            "claude-3-haiku" => self.family_latest.get("claude-3-haiku").cloned(),
            "deepseek-chat" => self.family_latest.get("deepseek-chat").cloned(),
            "deepseek-reasoner" => self.family_latest.get("deepseek-reasoner").cloned(),
            _ => None,
        }
    }

    /// 当前价格表中的模型总数
    pub fn len(&self) -> usize {
        self.prices.len()
    }

    /// 价格表是否为空
    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }
}

/// 剥离常见的提供商前缀
pub fn strip_provider_prefix(s: &str) -> &str {
    let prefixes = [
        "anthropic/",
        "anthropic.",
        "bedrock/",
        "bedrock.",
        "openai/",
        "openai.",
        "deepseek/",
        "deepseek.",
        "openrouter/",
        "azure_ai/",
        "us.anthropic.",
        "eu.anthropic.",
    ];

    for prefix in prefixes {
        if let Some(stripped) = s.strip_prefix(prefix) {
            return stripped;
        }
    }
    s
}

/// 从形如 "claude-3-5-sonnet-20241022" 或 "claude-3-opus-20240229-v1:0" 中提取 (family, date)
fn extract_family_and_date(s: &str) -> Option<(String, String)> {
    // 移除末尾可能的版本标记如 "-v1:0" 或 ":0"
    let base = s.split(':').next().unwrap_or(s);
    let base = base.strip_suffix("-v1").unwrap_or(base);
    let base = base.strip_suffix("-v2").unwrap_or(base);

    // 匹配末尾 8 位纯数字日期 202xxxxx
    if let Some(idx) = base.rfind('-') {
        let suffix = &base[idx + 1..];
        if suffix.len() == 8
            && suffix.starts_with("202")
            && suffix.chars().all(|c| c.is_ascii_digit())
        {
            let family = base[..idx].to_string();
            let date = suffix.to_string();
            return Some((family, date));
        }
    }
    None
}

/// 价格表生命周期管理器
pub struct PriceManager {
    table: Arc<RwLock<PriceTable>>,
    cache_path: PathBuf,
}

impl PriceManager {
    /// 构造 PriceManager 并同步完成初始加载
    ///
    /// 1. 若磁盘缓存 `pricing.json` 存在且解析有效（>= 1000 模型），优先使用；
    /// 2. 否则平稳回退至内置兜底价目表 `bundled()`。
    pub fn new(data_dir: &Path) -> Self {
        let cache_path = data_dir.join(PRICING_FILE_NAME);
        let table = if cache_path.is_file() {
            match fs::read_to_string(&cache_path) {
                Ok(content) => match PriceTable::from_json(&content, MIN_VALID_MODELS) {
                    Ok(parsed) => {
                        eprintln!(
                            "[token-watcher] 成功加载磁盘价格缓存: {} 个模型",
                            parsed.len()
                        );
                        parsed
                    }
                    Err(e) => {
                        eprintln!("[token-watcher] 磁盘价格缓存校验未通过 ({e})，回退内置价目表");
                        PriceTable::bundled()
                    }
                },
                Err(e) => {
                    eprintln!("[token-watcher] 读取磁盘价格缓存失败 ({e})，回退内置价目表");
                    PriceTable::bundled()
                }
            }
        } else {
            PriceTable::bundled()
        };

        Self {
            table: Arc::new(RwLock::new(table)),
            cache_path,
        }
    }

    /// 获取内存价格表的只读句柄
    pub fn table(&self) -> Arc<RwLock<PriceTable>> {
        Arc::clone(&self.table)
    }

    /// 启动后台异步刷新任务（1 小时周期），静默网络失败，更新后发出 Tauri 事件
    pub fn spawn_background_refresher<R: tauri::Runtime>(
        self: Arc<Self>,
        app: Option<tauri::AppHandle<R>>,
    ) {
        tauri::async_runtime::spawn(async move {
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent("token-watcher/0.2")
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[token-watcher] 无法构建价格拉取 HTTP 客户端: {e}");
                    return;
                }
            };

            // 启动时检查：若磁盘缓存不存在或修改时间已超过 1 小时，立即先执行一次拉取
            let need_initial_fetch = match fs::metadata(&self.cache_path) {
                Ok(meta) => match meta.modified() {
                    Ok(mtime) => SystemTime::now()
                        .duration_since(mtime)
                        .map(|age| age >= REFRESH_INTERVAL)
                        .unwrap_or(true),
                    Err(_) => true,
                },
                Err(_) => true,
            };

            if need_initial_fetch {
                self.fetch_and_update(&client, app.as_ref()).await;
            }

            let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
            loop {
                ticker.tick().await;
                self.fetch_and_update(&client, app.as_ref()).await;
            }
        });
    }

    /// 执行一次价格拉取、解析与原子写盘
    async fn fetch_and_update<R: tauri::Runtime>(
        &self,
        client: &reqwest::Client,
        app: Option<&tauri::AppHandle<R>>,
    ) {
        match client.get(LITELLM_PRICES_URL).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(text) => match PriceTable::from_json(&text, MIN_VALID_MODELS) {
                    Ok(new_table) => {
                        let count = new_table.len();
                        // 1. 原子写盘 (tmp + rename)
                        let tmp_path = self.cache_path.with_extension("json.tmp");
                        if let Err(e) = fs::write(&tmp_path, text.as_bytes()) {
                            eprintln!("[token-watcher] 写入临时价格缓存失败: {e}");
                            let _ = fs::remove_file(&tmp_path);
                            return;
                        }
                        if let Err(e) = fs::rename(&tmp_path, &self.cache_path) {
                            eprintln!("[token-watcher] 替换价格缓存文件失败: {e}");
                            let _ = fs::remove_file(&tmp_path);
                            return;
                        }

                        // 2. 换入内存 RwLock
                        if let Ok(mut guard) = self.table.write() {
                            *guard = new_table;
                            eprintln!("[token-watcher] 价格表已更新并换入内存: {count} 个模型");
                        }

                        // 3. 发出 pricing-updated 事件通知前端全量水合
                        if let Some(app_handle) = app {
                            use tauri::Emitter;
                            let _ = app_handle.emit("pricing-updated", ());
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[token-watcher] 远程价格表解析未通过健康校验: {e}（保留现有缓存）"
                        );
                    }
                },
                Err(e) => {
                    eprintln!("[token-watcher] 读取远程价格表响应体失败: {e}");
                }
            },
            Ok(resp) => {
                eprintln!(
                    "[token-watcher] 远程价格源返回非成功状态码: {}",
                    resp.status()
                );
            }
            Err(e) => {
                // 静默失败原则：仅记录诊断日志，不打扰前端
                eprintln!("[token-watcher] 后台拉取价格表网络失败: {e}");
            }
        }
    }
}
