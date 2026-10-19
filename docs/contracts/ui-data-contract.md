# 前端数据契约（Tauri command / event payload）

> truth-source for T6（UI）、T7（聚合）、T4/T5（app 壳）。
> 目的：UI 与 Rust 两端并行实现，形状先定案。serde Serialize 字段名与本文
> 完全一致（snake_case）；TS 侧同名。实现内部自由，形状变更须经主 session。

## 命令（UI → Rust，#[tauri::command]）

### `get_dashboard(range: Range) -> Dashboard`
一次性水合：窗口隐藏后重开（水合语义）、chips 切换、或收到 `dashboard-dirty`
事件后防抖重拉。`Range` = `"today" | "week" | "month" | "all"`（今天=本地 00:00 起）。

### `get_scan_status() -> ScanStatus`
启动时/收到 `scan-progress` 前主动查询一次（防事件早于前端监听就绪）。

### `update_profile_display(profile_id: i64, display_name: string) -> ()`
编辑档案显示名。

### `update_profile_quota(profile_id: i64, total: Option<i64>, used: Option<i64>, reset_date: Option<string>) -> ()`
手动额度（R2-2：纯手动、不清零、仅倒计时展示）。

### `reassign_usage(profile_id: Option<i64>, start_ms: i64, end_ms: i64) -> ()`
手动修正归因（T2 store 层回填函数的 UI 入口；None = 归 Unknown）。

## 事件（Rust → UI，emit）

### `scan-progress`（D16 首扫进度态）
```json
{ "phase": "scanning" | "done" | "empty",
  "files_done": 83, "files_total": 364, "percent": 22.8 }
```
`empty` = ~/.claude 不存在或无文件（复用骨架态文案）；首扫完成发 `done` 后
紧发一次 `dashboard-dirty`。

### `data-health`（D17 三档错误态；仅状态变化时发）
```json
{ "root_unreadable": false,
  "skipped_files": 2,
  "db_readonly": false }
```
三档独立呈现：不可读横幅 / 「跳过 N 文件」计数 / 只读降级横幅。

### `dashboard-dirty`
无 payload。语义：聚合结果已变化（增量落库后聚合刷新），UI 防抖（~500ms）
重拉 `get_dashboard(当前 range)`。

### `usage-pulse`（秒级表面：折线专用）
```json
{ "t_ms": 1792404225883,
  "input": 84036, "output": 2087,
  "cache_read": 128, "cache_creation": 0 }
```
一次 emit = 一条 usage 记录落库（到达脉冲叙事：脉冲即到达事件，不是生成时刻）。
折线固定最近 1 小时窗口、画 Reported tokens/s 口径；UI 自行累计，空闲 30s
冻结渲染。窗口隐藏时 Rust 侧暂停 emit（水合语义），重开时 `get_dashboard` 全量拉。

## 数据形状

```ts
interface Dashboard {
  range: Range;
  totals: { input: number, output: number,
            cache_read: number, cache_creation: number, total: number };
  pulses: UsagePulse[];            // 固定最近 1 小时（与 range 无关）
  models: { model: string, tokens: number }[];   // 按 tokens 降序
  profiles: ProfileCard[];         // 按 tokens 降序；含 Unknown 虚拟卡
  sessions: SessionRow[];          // 按最后活跃降序
}

interface ProfileCard {
  profile_id: number | null;       // null = Unknown（置灰卡、无配额条）
  display_name: string;            // Unknown 卡 = i18n "Unknown"
  endpoint: string | null;         // 规范化 scheme://host:port
  inferred: boolean;               // v0.1 恒 true：卡面「推断 · 低置信」标签
  tokens: { input: number, output: number,
            cache_read: number, cache_creation: number, total: number };
  quota: { total: number | null, used: number | null,
           reset_date: string | null } | null;   // null = 未配置/Unknown
}

interface SessionRow {
  session_id: string;
  title: string;                   // cwd 尾段或 id 前 8 位
  last_active_ms: number;
  total: number;                   // 该会话四项总和
  active: boolean;                 // 最近 5 分钟内有新记录（菜单栏同源定义）
}
```

口径一致性（SC7）：`totals.total` ≡ 菜单栏「今日累计」回退态数字（同一 SQL
口径）；四项定义与数据源契约一致。

T7 实现固化的补充语义（主 session 采纳）：
- **空档不呈现**：models / profiles / sessions（含 Unknown 卡）tokens=0 时
  不出现在列表——有数据才呈现，避免空卡噪音
- **range 语义**：`week`/`month` = 含今天在内 7/30 个**本地日历日**（非滚动
  7×24h），保证 today ⊂ week ⊂ month 且日内数字不漂移
- **Unknown 卡本地化**：UI 侧按 `profile_id == null` 判定 Unknown 并取
  i18n 文案，不依赖 Rust 返回的 `"Unknown"` 字面量

## 状态机（UI 启动序）

1. 前端就绪 → `get_scan_status()`：`scanning` 则进入首扫骨架态并订阅事件；
2. `scan-progress(done)` / 首次 `dashboard-dirty` → 拉取 `get_dashboard` 无缝切换；
3. 运行期：`usage-pulse` 累计折线；`dashboard-dirty` 防抖重拉大数字/分布/卡片/列表；
4. 任何时刻 `data-health` 更新三档错误态呈现。
