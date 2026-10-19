# Design: token-watcher — 跨平台 AI Token 实时监控桌面小组件（v1.1 修订版）

Status: APPROVED（v1.1）
Mode: Builder

> **修订记录**：v1.0（2026-10-19 office hours 批准，原文见
> [token-watcher-design-20261019-original.md](token-watcher-design-20261019-original.md)）
> → v1.1（2026-10-19 工程评审 /plan-eng-review 全部决议 D14-D23 写回 +
> Codex/agy-Gemini 事实修正五项 + spec-review R2-1/2/3 遗留义务）。
> 评审全档：`panwenbo-unknown-eng-review-20261019-133740.md`（gstack 项目归档）。
> **本文件是实施的唯一权威设计来源**；v1.0 与本文件冲突处以本文件为准。

## Problem Statement

VibeCoding 开发者（Claude Code 重度用户）烧 token 如流水，却看不到消耗：现有工具要么是终端里的事后报表（ccusage、tokscale），要么是 macOS-only 的菜单栏配额条（ClaudeUsageBar、TokenEater、TokenBar）。没有一个工具同时满足：macOS + Windows 双平台、实时（秒级）图表、桌面 HUD 形态、多语言。更具体的痛点：**多订阅/中转用户**（如通过 CC Switch 在 Anthropic 官方、DeepSeek、Kimi 等中转间切换的用户）无法知道每个订阅各烧了多少、配额还剩多少——Claude Code 本地会话文件里根本没有这个字段。

作者本人就是目标用户。

## What Makes This Cool

- **到达脉冲折线**（v1.1 修正叙事，原「心电图」）：JSONL 按整轮（API 回合）落盘，
  精确的逐秒生成时刻不存在——图表画的是「用量到达事件」：每轮结束一个脉冲尖峰，
  脉冲间隔的平线 = **未报告用量**（不是「零消耗」），缺口 = 无观测覆盖时段。
  口径标注「Reported tokens/s」。这是对观测能力的诚实呈现，不是牺牲
- **订阅归因（推断·低置信）**：监听 settings.json 变化 + 时间戳关联，把每条用量
  归到当时的订阅档案；无法归入的时段归 Unknown 档案——多中转用户第一次看清
  每个后端的消耗，且界面不假装比实际知道得更精确（精确归因 = v0.2 本地代理模式）
- **诚实仪表美学**（Meter Face）：唯一信号色琥珀只给测量数据；预估/估算用 45°
  斜线纹理表示（不靠颜色、不靠图例）；无红绿评判色——表只报告，不训人
- **形态切换**：菜单栏+悬浮窗（v0.1）↔ 桌面角落 HUD（v0.2，游戏 FPS 计数器感）

## Constraints

- 双平台：macOS + Windows（Tauri v2，系统 WebView：WKWebView / WebView2）
- 小巧低占用：包体目标 <10MB；空闲 CPU≈0（fs 事件驱动，无轮询；折线空闲 30s
  冻结渲染，前导点变空心表示「笔已抬起」）；**常驻内存分平台目标
  （v1.1 修正，原 <150MB 单值）：macOS <150MB / Windows <250MB [estimated]**
  （WebView2 运行时基线本身就高于 WKWebView，单值目标在 Windows 必然撞穿）
- 技术栈：Rust（core 数据引擎）+ Tauri v2 + Svelte 前端
- i18n：中文/英文，默认跟随宿主机语言（**交付挂构建步骤 4**，随悬浮窗一起交付，
  见 R2-1 义务）；暗色模式默认跟随系统
- 纯开源社区工具，不商业化（本期）
- v0.1 数据源只做 Claude Code 本地 JSONL（`~/.claude/projects/**/*.jsonl`）

## Premises（全部经用户确认；3 为 v1.1 强化）

1. 品类空位真实存在，值得做开源工具
2. v0.1 数据源 = Claude Code 本地 JSONL + 监听 `~/.claude/settings.json` 的 env
   变化做订阅归因
3. **订阅归因是推断不是事实，且以诚实降级为原则**：v0.1 归因以 Unknown 档案为主
   导（歧义时段一律归 Unknown，宁缺勿错），UI 全程以「推断 · 低置信」标注；
   并发终端 / env 穿透 / 全局单点等场景下时间线推断必然失效（Codex 与 agy
   独立一致指出，agy 评为 Critical）——彻底解法是 v0.2 的本地代理模式，
   **已提至 v0.2 路线图首位**。装工具前的历史无法回溯归因（全部 Unknown）
4. 四个统计维度：会话 / 模型 / 程序（entrypoint）/ 订阅档案。token 四项直读
   usage 字段；成本估算与自动配额进 v0.2；v0.1 配额条为纯手动配置
5. 自建 SQLite 聚合存储，历史不受 Claude Code 默认 30 天会话清理影响
6. 分发 GitHub Releases + CI 双平台；签名后置
7. design tokens day one；v0.1 单默认主题 Meter Face；Slate/Mint 预设 v0.2

## Cross-Model Perspective

**Codex 冷读**（已采纳）：flight recorder 进 v0.2；归因必须标注推断（进 Premise 3）；
其垂直切片原型建议（notify+serde+rusqlite、增量 tail+checkpoint+去重、先菜单栏后 HUD）被 Approach B 采纳。
工程评审阶段 Codex 复审 6 findings（3 High / 3 Medium）全部裁决采纳：
F1 归因验收超证据（→ D20）、F2 扫描/watch 交接竞态（→ D21）、F3 幂等≠更新正确（→ D22）、
F4 档案身份混淆（→ D23）、F5 CLI 绕过单实例守卫（→ 单一 ingestion owner）、F6 图表宣称超出观测（→ 事实修正）。

**agy-Gemini 第三路复审**（7 findings 全处置）：Critical 归因架构倒挂（→ D23 诚实降级
+ 代理提前 v0.2 首位）；High 三项全采纳：心电图物理不存在（→ 到达脉冲叙事）、
Windows 托盘画不下数字（→ 静态图标+tooltip+弹窗）、WebView2 内存/1px 漂移
（→ 分平台内存目标 + 空闲冻结）；Medium 两项采纳：隐藏窗口水合、[lib] 显式
+ tw-ingest 不入包；Low 一项采纳：禁 macOS Sandbox。

**三方美学合成**：石墨底非纯黑；唯一强调色只给测量数据；拒绝发光/blur/霓虹；
机械式短动效（80-240ms）；折线线性连接保留尖峰；LIVE 是状态不是装饰；CJK 走系统字体。默认主题 **Meter Face 合成版**。

## Approaches Considered

- A 全功能一步到位 —— 被否（战线太长，烂尾风险高）
- B **垂直切片先行**（v0.1 = 可信数据管道 + 菜单栏/悬浮窗）✅
- C Go + Wails —— 被否（tray/透明置顶窗支持弱；GC 内存高一档）

## Recommended Approach

**Approach B**。数据层与 A 完全一致，分期只影响 UI 形态交付节奏。

### 架构（v0.1）

```
~/.claude/projects/**/*.jsonl ──notify(FSEvents/ReadDirectoryChangesW)──┐
~/.claude/settings.json ──watch(env 变化→订阅档案时间线)────────────────┤
                                                                        ▼
                    Rust core：增量 tail（byte checkpoint，mtime/size 对账）
                    → 行级解析（三态：完整行/不完整尾行缓冲/坏行跳过计数）
                    → 去重（uuid 身份 + 后到者胜 UPSERT，D22）
                    → SQLite（usage_records / file_checkpoints /
                      profile_timeline / profiles 同事务分批提交）
                    → SQL 侧聚合 → Tauri event push（窗口隐藏时暂停，重开水合）
                                                                        ▼
                    Svelte 前端：菜单栏/托盘项 + 悬浮窗（380×560）
```

- crates：`notify`、`serde`/`serde_json`、`rusqlite`（bundled）
- **watcher 交接顺序（D21）**：先注册 fs watcher、后枚举文件——注册与枚举之间
  到达的事件不丢失（枚举时从 checkpoint 续读，天然覆盖重叠窗口）；恢复时
  size/mtime 与 checkpoint 对账，不符则单文件重扫（uuid UPSERT 幂等）
- **单实例守卫（D15）**：tauri-plugin-single-instance；第二实例唤起已有悬浮窗后退出；
  验收：双开无第二托盘、第二实例唤起主窗
- **单一 ingestion owner（F5）**：GUI 与 tw-ingest 双 bin 并存，但任一时刻只有
  一个进程拥有写入库；GUI 运行时 tw-ingest 只读或直接拒绝
- **单 crate 双 bin + 显式 [lib]（D14）**：`token-watcher`（GUI）+ `tw-ingest`
  （开发验证 CLI，不入安装包）；core 模块经 `[lib]` 共享
- 永不持久化认证 token；settings.json 只读 env 键名与 base URL，凭据只存 SHA-256 指纹

### 数据源契约（v1.1 新增章节）

见 [docs/data-source/claude-code-jsonl.md](../data-source/claude-code-jsonl.md)
（2026-10-19 本机 probe 定案）。要点：只取 `type=assistant` 且带 `message.usage`
的行；四项直读 `input_tokens` / `output_tokens` / `cache_read_input_tokens` /
`cache_creation_input_tokens`；模型维度用 `message.model`（中转映射后真名），
不用 `requestedModel`；行级三态与 checkpoint 约定以契约文档为准。

### 悬浮窗信息层级（线框已定稿，assets/token-watcher-wireframe-20261019.png）

今日总量大数字 → 到达脉冲折线（最近 1 小时）→ 模型分布横条 → 订阅档案卡片 →
会话列表；时段 chips（今天/7天/30天/全部）一处切换全局过滤。

- **首扫进度态（D16）**：首扫秒~分钟级（重度用户），期间显示骨架 + 文件计数 +
  百分比（Tauri event 通道推送进度），完成后无缝切换真实数字；
  `~/.claude` 不存在 / 无文件的空态复用同一套骨架逻辑
- **三档错误态（D17）**：① 根目录不可读——顶部横幅（中性色，无红绿评判）；
  ② 单文件失败——跳过并计数，UI 可见「跳过 N 文件」；③ SQLite 损坏/锁——
  只读降级模式横幅。三档各自可见且可恢复；中英文案
- **隐藏窗口水合（agy Medium）**：窗口隐藏时暂停 event 推送（CPU≈0）；
  重新显示时从 SQLite 全量水合一次，再续接增量推送
- **v0.1 配额口径（R2-2 定案）**：订阅档案卡片交付**纯手动**配额条——已用只来自
  用户输入、重置日仅作倒计时展示、到重置日**不自动清零**；卡片以「手动」等宽字
  标注。不提供「用统计值填充已用」动作（v0.2 视需求）。自动配额（中转 billing
  接口、官方启发式估算）进 v0.2
- 归因时间线之外的时间段（含装工具前的全部历史）统一归入「Unknown」档案，
  置灰卡片（无配额条），这是 Premise 3「显示未知时段」的落地位置

### 菜单栏 / 托盘项（可配置）

默认显示**当前激活会话**的总 Token；四指标可配（总/输入/缓存读取/输出）；
无活跃会话时回退今日累计。「激活会话」= 最近 5 分钟内有新 usage 记录的会话；
多会话并行时取最近一条 usage 记录所属的会话（不求和——求和方案留 Open
Question 4 按体感调）。分钟级刷新（空闲 CPU≈0），悬浮窗内才秒级。

**双平台呈现（v1.1 修正：Windows 数字入图标方案已否决——16-20px 像素物理
画不下数字）**：

- **macOS**：NSStatusItem 文本 title（Tauri tray title，原生支持）
- **Windows**：系统托盘**静态图标** + tooltip 放当前数字（分钟级刷新）+
  点击弹出悬浮窗看全量。Windows 常驻数字的真答案是 v0.2 的 HUD

### 订阅档案识别规则（D23 + R2-3 修订）

- **档案身份与凭据解耦（D23）**：档案有独立的持久 ID（首次出现时分配，此后
  稳定不变）；端点（base URL）与凭据指纹是档案的映射属性——同一订阅换 key
  不产生新档案，凭据轮换只更新映射
- 归属主判据 = 规范化的 `ANTHROPIC_BASE_URL`（scheme+host+port；path 差异
  提示不区分）
- 次级键（可选，R2-3）= `ANTHROPIC_AUTH_TOKEN` **或** `ANTHROPIC_API_KEY`
  （取已设置者）的指纹（SHA-256 前 8 位 hex），区分同 URL 不同 key 的订阅；
  **永不持久化原文**
- 显示名用户可编辑，默认从 host 生成（`api.deepseek.com` → `DeepSeek`）；
  `api.anthropic.com` 默认「Anthropic 官方」
- **归因诚实降级（D20/D23）**：时间线歧义时段（切换宽限期内、并发终端、
  resume 场景）一律归 Unknown 档案（宁缺勿错）；一切归因结果带
  「推断 · 低置信」标签；归因边界行为由 fixture 定案（并发 / resume 夹具）

### 更新语义与去重（D22，v1.1 新增章节）

- 逻辑记录身份 = `uuid`；版本优先级 = 后到者胜（文件内行序 / 跨文件导入序）；
  UPSERT REPLACE 语义，绝不累加 → SC3「重启不双计」的机制保证
- 详见数据源契约文档

### 默认主题：Meter Face 合成版（v0.1 唯一预设）

- 底色：暖石墨 `#262420`（暗）/ `#F2EFE9`（亮）；发丝线分区 `#3A3730` / `#DFDACD`
- 唯一信号色：琥珀 `#E8A33D`（只给测量：曲线、LIVE 点、活跃刻度）
- 诚实编码：45° 斜线纹理填充 = 估算，实心 = 实测/手动；等宽字标注紧贴数值
- 字体：Archivo（Latin）+ Fragment Mono（数字）；CJK 回退 PingFang SC /
  Microsoft YaHei UI（系统字体，不打包）
- 动效：逐位 120ms 上滑（机械寄存器）；burst 最多 3 步 80ms 中间值；
  **折线空闲 30s 冻结渲染**（v1.1：持续左漂 ~1px/s 与空闲 CPU≈0 冲突，冻结
  后前导点变空心）；全程兼容 reduced-motion
- 排除项：全 UI 零象形图标（只用点/斜线/发丝线/进度条四种图形记号）；
  无红绿评判色（错误态文案也用中性色）；2px 硬倒角；无毛玻璃

### 测试策略（D18，v1.1 新增章节）

v0.1 = **仅 cargo test**（单元 + fixture 集成测试：JSONL fixture 与归因时间线
fixture，覆盖 Test Plan 的 17 条代码路径）；UI 行为走 Success Criteria 手验清单；
vitest 推迟至 v0.2 UI 逻辑增重时再议。fixture 定案先于存储实现（F3）。

### v0.2 路线图（代理模式提至首位，D23）

**本地代理模式（归因精确到请求，同 URL 多 key 的彻底解法）** → 桌面角落 HUD
（透明置顶+点击穿透；Windows 常驻数字的真答案）→ 成本估算（LiteLLM 价格表，
1h 缓存）→ 自动配额（官方启发式 / 中转 billing 接口）→ 主题切换器
（+Calibrated Slate、+Mint Pulse）→ flight recorder 时间线 → 数据源扩展
（Codex CLI ~/.codex/ 等）。

## Open Questions

1. 中转站 billing API 格式差异大——v0.2 摸清主流格式再做
2. Claude Code 版本升级可能改 JSONL schema——容错策略已在数据源契约定案
   （缺字段跳过+计数、未知字段忽略、不 panic）；上线后持续观察
3. 产品名 token-watcher 是否最终定名
4. 激活会话 5 分钟阈值、并行「取最近 vs 求和」——实装后按体感调
5. 同 base URL 多 key：v0.1 指纹次级键（AUTH_TOKEN/API_KEY 已设置者）；
   代理模式（v0.2 首位）是彻底解法

## Success Criteria（v0.1 验收硬指标）

1. **悬浮窗**：每轮 usage 落盘后 <2s 内 UI 可见（fs 事件→推送→更新）
   （v1.1 修正：原「秒级图表更新」，到达脉冲叙事下以整轮可见为准）
2. **菜单栏/托盘**：用量变化后 ≤60s 反映（Windows tooltip 同节奏）
3. 重启应用/重扫文件，总量绝不重复计数（uuid UPSERT + checkpoint 幂等）
4. 删除源 JSONL 文件，历史统计仍在（自建 SQLite）
5. macOS 与 Windows 均能构建运行
6. 空闲（无 coding 活动）CPU 占用≈0，无轮询；折线空闲 30s 冻结渲染
7. 口径一致性：菜单栏「今日累计」态与悬浮窗大数字完全相等；两个表面 token
   四项口径定义完全一致
8. 双开：无第二托盘，第二实例唤起主窗后退出（D15）
9. 首扫期间 UI 持续反馈进度，完成后无缝切换（D16）
10. 三档数据层失败（不可读/单文件跳过/DB 降级）各自可见且可恢复（D17）

## Distribution Plan

- GitHub Releases：macOS（.dmg/.app，universal）+ Windows（.msi/.exe）
- CI：GitHub Actions matrix（macos-latest + windows-latest），tauri-action
- **禁 macOS App Sandbox**（v1.1，agy Low 采纳）：需读 `~/.claude`，Sandbox
  必拦；GitHub Releases 直接分发 + 公证路线后置
- **tw-ingest 不入安装包**：bundle 只打包 GUI 主 bin
- 签名/公证后置：README 写清 Gatekeeper/SmartScreen 绕过指引

## 构建顺序（每步可独立验证；v1.1 挂点已标）

1. Rust core：全量解析 → SQLite，tw-ingest CLI 打印今日总量
   （验证：与 ccusage 同量级、口径差异可解释）
2. 增量：notify + checkpoint + 去重，**先注册 watcher 再枚举 + mtime/size 对账**
   （验证：重启不双计、删源不丢史、交接窗口不丢事件）
3. Tauri 壳：托盘 + 指标配置 + **单实例守卫（D15）** + 单一 ingestion owner
   （验证：默认当前会话总量、回退今日累计；双开验收 SC8）
4. 悬浮窗：Svelte + Meter Face + 到达脉冲折线 + **首扫进度态（D16）** +
   **三档错误态（D17）** + **隐藏水合** + **i18n 中英（R2-1 挂点）**
   （验证：SC1/SC6/SC7/SC9/SC10 手验清单、reduced-motion）
5. 订阅档案：settings.json 监听 + 归因时间线 + 手动修正 UI + 手动额度配额条
   （验证：切中转后新会话归新档案、旧会话不变；装前历史全部归 Unknown 且可见；
   归因卡片带「推断 · 低置信」标签）

## Decision Ledger（D14-D23 摘要）

| # | 决议 |
|---|---|
| D14 | 单 crate 双 bin + 显式 [lib]；tw-ingest 不入安装包 |
| D15 | 单实例守卫（tauri-plugin-single-instance，第二实例唤起主窗退出） |
| D16 | 首扫进度态（骨架+文件计数+百分比；空态复用） |
| D17 | 三档错误态（不可读横幅/单文件跳过计数/DB 只读降级） |
| D18 | 仅 cargo test（fixture 单元+集成；UI 走 SC 手验） |
| D19 | agy 复审用新会话（独立性保证） |
| D20 | 归因歧义归 Unknown + 切换宽限期 + fixture 验收 |
| D21 | 先注册 watcher 再枚举 + 恢复 mtime/size 对账 |
| D22 | 逻辑记录身份=uuid + 后到者胜 + UPSERT 不累加；fixture 定案先于存储实现 |
| D23 | 归因诚实降级（Unknown 主导+低置信标签）；档案=持久 ID+端点/凭据映射；本地代理模式提至 v0.2 首位 |

事实修正（非决策，观测更正）：到达脉冲叙事；Windows 托盘静态图标；
分平台内存目标；折线空闲冻结；隐藏窗口水合。
