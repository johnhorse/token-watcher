# Changelog

所有对 Token Watcher 的重大变更均记录于此文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.0.0/)，
版本号遵循 [语义化版本 2.0.0](https://semver.org/lang/zh-CN/)。

---

## [0.7.0] - 2026-10-22

### Added
- **多数据源扩展 3.0（Google Antigravity、智谱 ZCode、阿里 Qoder、腾讯 WorkBuddy）**：
  - **Google Antigravity**：支持解析本地 SQLite 会话库（`conversations/*.db`），内置零额外依赖 `ProtobufReader` 穿透解码嵌套元数据，精准还原 4D Token、思维链用量与起止耗时，支持并发忙锁独立快照安全降级；
  - **智谱 ZCode**：解析 `model-io-session_*.jsonl`，严格应用 D24 规则扣除包含式缓存防双计，映射单轮原生毫秒耗时 `durationMs` 与轮次 `attempt`；
  - **阿里 Qoder 与腾讯 WorkBuddy**：解析官方项目级会话流 JSONL，支持 4D Token、思维链及秒级/毫秒级时间戳自适应规范化；
  - **基准价目表扩充**：定价表补充 `GLM-5.3`、`gemini-3.8-flash`、`gemini-3.0` 系列模型及 `qwen-2.5-coder-32b`、`hunyuan-code`。
- **Thinking Iceberg™（思维链冰山模型）**：
  - 底层存储升级至 Schema v7，`usage_records` 增加 `thoughts_tokens` 持久化列；
  - 看板新增 Thinking Iceberg 冰山模型卡片，直观透视「水面之上可见输出」与「水面之下深层思考」；
  - 徽标矩阵拓展至 12 款主流引擎，全链路注册隔离名单，防止新数据源污染 Claude 订阅时间线。

### Fixed
- **状态栏指标聚合口径纠偏**：
  - 修复状态栏（菜单栏 / 托盘）在活跃会话期仅显示单会话导致数值跳变的缺陷，始终保持展示今日全天全局汇总数据，与 HUD 活跃会话切片彻底解耦。

## [0.6.1] - 2026-10-22

### Added
- **HUD 多激活会话垂直平铺展示与状态栏独立汇总**：
  - 当存在多个激活会话时，HUD 极简胶囊自适应展开为多行垂直平铺展示，桌面小组件联动展示活跃会话微卡片；
  - 状态栏菜单新增「活跃会话展示数」（1/2/3/5 条，默认 3 条）单选配置，状态栏数字始终保持聚合全局汇总。
- **极简胶囊与桌面小组件独立透明度调节**：
  - 极简胶囊与卡片小组件各自支持 50%、70%、80%、90%、100% 独立透明度分级；
  - 采用背景磨砂通透质感，文字、数字、边框保持纯白高亮清晰，在状态栏菜单与 HUD 右键菜单中均可实时调节。

### Fixed
- **修复主面板内容无法滚动问题**：
  - 修复 `HudApp.svelte` 中全局样式意外覆盖导致 `#app` 容器被强加 `overflow: hidden !important` 的缺陷，将其严格限制在 `html[data-hud]` 作用域内，彻底恢复主面板正常平滑滚动。
- **修复 GitHub Release 发布页面说明空白问题**：
  - 在 GitHub Actions 发版工作流中新增 `Extract release notes` 步骤，自动根据 tag 提取 `CHANGELOG.md` 对应版本说明传递给 `tauri-action`，并补齐已发布版本的发布说明。
- **重构 Token 生成速率计算模型并消除闲置破折号**：
  - 升级底层存储至 Schema v6，支持记录每轮对话真实耗时（`duration_ms`）；
  - 速率由最近 2 轮对话汇总计算 `(out1 + out2) / (dur1 + dur2)`，真实反映 LLM 生成速率；
  - 对话闲置或停顿时锁定保持最新计算出的速率数值，绝不再倒退显示为 `—`。

## [0.6.0] - 2026-10-22

### Added
- **开机自动启动（Autostart）**：
  - 集成 `tauri-plugin-autostart` 官方插件，macOS（LaunchAgent）与 Windows（注册表）双端兼容；
  - 安装后首次启动默认开启自启，并在状态栏托盘菜单新增「开机自动启动」（`Launch at Startup`）勾选项，随时自由开关。
- **GitHub 版本更新检测与通知（Update Notification）**：
  - 后端异步拉取 GitHub Releases API，采用语义化三段版本号智能比对并配置 6 小时内存缓存保护；
  - 发现新版本时在主面板顶部展示置顶更新横幅（`🎉 发现新版本 vX.Y.Z · [查看更新] [✕]`），点击即直达 GitHub Release 页面，支持关闭。
- **桌面卡片小组件（Desktop Widget）与双形态平滑切换**：
  - 小组件大数字下方全面联动右键菜单勾选指标（当前速率、会话总 Token、当前模型、缓存率、输入、输出），以自适应微胶囊网格呈现，奇数指标自适应跨列；
  - 模型分布精选 Top 3 消耗最高模型，超出时提供「+N 更多模型 →」快捷入口，点击唤起并聚焦主面板；
  - 托盘菜单与悬浮窗右键菜单均支持一键形态切换与鼠标穿透锁定。
- **启动窗口避让策略与形态独立坐标记忆**：
  - 主面板启动显式配置屏幕居中（`center: true`），彻底消除启动时与右上角小组件的重合遮挡；
  - 小组件与极简胶囊升级为独立坐标持久化（`widget_x/y` 与 `capsule_x/y`），小组件默认屏幕右上角，极简胶囊默认桌面正顶部居中，互不干扰且无缝向后兼容。

### Fixed
- **修复浅色桌面背景下悬浮窗四周出现直角黑边的视觉瑕疵**：
  - 全链路为悬浮窗根节点（`html`、`body`、`#app`）注入透明属性（`background: transparent !important`），并在首屏脚本中提前识别 HUD 窗口；
  - 消除卡片与胶囊外部超出边界被窗口硬裁切的大半径 `box-shadow`，改用内部精致微光边框，确保圆角外部像素绝对透明。
## [0.5.1] - 2026-10-21

### Added
- **首页多引擎大盘切片筛选胶囊（EngineChips）**：
  - 首页时段切换器下方新增原生数据源切片胶囊（`[全部]`、`[Claude]`、`[Codex]`、`[OpenCode]`、`[Continue]`、`[omp]`、`[Cline]`、`[Roo]`、`[Aider]`）；
  - 动态统计并展示当前时段内各引擎的实际 Token 消耗量与专属工控徽标色彩，未产生数据的引擎自动收敛隐藏；
  - 支持后端多引擎联动切片聚合（`dashboard_with_prices_and_engine`），点击后大盘总 Token、折线图、模型排行、费用均精准下钻联动。
- **会话列表极速搜索与过滤**：
  - 会话列表顶部新增紧凑搜索框，支持按会话名称、路径、模型或引擎名称快速过滤，秒级定位历史会话。
- **多源数据解析与工作目录提取增强**：
  - 新增 `extract_omp_cwd`：从 omp 会话日志提取项目真实工作路径（`cwd`），消除跨项目会话归属模糊；
  - 完善多引擎模型字段提取容错。
- **测试套件扩充与工程保障**：
  - 新增 `tests/t19_engine_slicing.rs`：测试多引擎聚合切片与可用引擎统计逻辑；
  - 新增 `test_codex_watcher_dir_linkage`：模拟 macOS FSEvents 深度目录事件并验证端到端捕获；
  - 全量 19 个测试套件、141 项测试全部通过，Clippy 零告警。

### Fixed
- **修复 macOS FSEvents 深度目录聚合导致会话增量漏统计的致命缺陷**：
  - 针对 macOS FSEvents 在高频追加写入时将文件事件聚合为目录变动（如 `.../sessions/YYYY/MM/DD`）的行为，新增 `DataEngine::collect_dir_sources` 通用深度目录递归反查机制；
  - 递归扫描反查 Codex 的 `sessions/YYYY/MM/DD/rollout-*.jsonl`、Claude 项目目录的 `*.jsonl` 等各引擎深层数据源，彻底消除密集调用执行期间 90%+ 的 Token 增量丢失问题。

## [0.5.0] - 2026-10-21

### Added
- **多数据源生态 2.0（OpenCode / Continue.dev / omp 原生接入）**：
  - **OpenCode 原生直读**：以纯只读模式（`PRAGMA query_only = ON;`）增量读取 `~/.local/share/opencode/opencode.db` 的 `message` 表，基于 `max(time_created)` 作为时间戳游标，彻底消除与 OpenCode 运行时的 SQLite WAL 锁冲突；
  - **Continue.dev 会话解析**：扫描 `~/.continue/sessions/*.json`，结合 `(mtime, size)` 毫秒级跳过与全量解析，提取会话历史数组与 `promptLogs` 中的 Token 消耗；
  - **omp 项目分层日志接入**：递归扫描 `~/.omp/agent/sessions/**/*.jsonl`，按项目目录分层提取 `assistant` 角色下的 `message.usage` 四维指标；
  - **微徽标矩阵扩充**：会话列表与会话飞行记录仪全面适配 `[OpenCode]`（翡翠绿）、`[Continue]`（青蓝）、`[omp]`（深金黄）微徽标；
  - 新增 `t18_multi_source_v2.rs` 集成测试，全量 18 个测试套件 137 项测试全绿，Clippy 零告警。

## [0.4.1] - 2026-10-21

### Added
- **飞行面板（HUD 挂件）与状态栏全景体验升级**：
  - **飞行面板 6 大指标动态可配置**：支持当前速率、激活会话总 Token、当前模型、缓存率、输入 Token 数、输出 Token 数自定义勾选，默认显示 `[当前速率 ｜ 激活会话总 Token ｜ 缓存率]`；
  - **右键原生配置菜单与动态自适应拉伸**：右击飞行面板弹出原生 macOS 勾选菜单，最少保留 2 项，悬浮胶囊根据指标数量自适应调整宽度并以细竖线分隔；
  - **瞬时生成速率算法纠偏**：修正以往将输入与上下文缓存混入计算导致虚高为 `K/s` 的缺陷，生成速率严格按照活跃窗口内纯输出 Token 统计，还原真实的 `token/s`；
  - **状态栏多选与缓存率接入**：状态栏新增「缓存率」指标选项，升级为多选支持（1-2 项多选，以 `•` 点号间隔展示）；
  - **状态栏菜单文案清晰化**：将「显示悬浮窗 / 隐藏悬浮窗」替换为符合直觉的「显示主面板 / 隐藏主面板」；
  - **macOS 程序坞（Dock）智能联动**：关闭主面板时自动移出 Dock，重新唤起主面板时自动恢复 Dock 图标；
  - **首页活动折线图丝滑趋势升级**：引入高斯核平滑采样与 Catmull-Rom 三次贝塞尔样条拟合，搭配主题自适应渐变面积衬底，消除突兀折角与零落针刺。

## [0.4.0] - 2026-10-21

### Added
- **多数据源生态扩展（Cline / Roo Code / Aider）**：
  - **Cline 原生直读**：支持 VS Code `globalStorage/saoudrizwan.claude-dev/tasks` 目录下的 `ui_messages.json` 与 `api_conversation_history.json` 自动探测与全景解析；
  - **Roo Code 原生直读**：支持 `globalStorage/rooveterinaryinc.roo-cline/tasks` 任务目录同构接入；
  - **Aider 终端日志接入**：增量读取 `~/.aider.analytics.jsonl`（或 `~/.aider/analytics.jsonl`），支持 `message_send` JSONL 事件流解析；
  - **三类独立数据源扫描器**：重构扫描通道为 `JsonlDir`（Claude/Codex 目录）、`TaskJsonDir`（Cline/Roo 任务目录树）、`SingleJsonlFile`（Aider 单文件），杜绝无界递归，支持单引擎缺失零告警平滑容错；
  - **稳定内容键与 Rewind 防护**：Cline/Roo 采用基于事件时间戳 `ts` 与内容哈希的稳定主键，彻底消除在 IDE 中回滚（Rewind）或重试时的会话覆盖与重复计数问题；
  - **(mtime, size) 增量跳过**：整体覆写的 JSON 数组文件在未发生修改时毫秒级跳过，变更时仅向 UI 上报增量新记录，杜绝脉冲洪泛；
  - **四维 Token 归一化严格分支（D24）**：精准区分 Anthropic 排他口径（净输入不扣减缓存）与 OpenAI 包含式口径（分离扣减 cache_read 与 cache_write），破损行严格拒绝；
  - **Aider 30 分钟时间切片（D29）**：基于用户 ID 与时间间隙动态切分会话，使终端工具具备真实单次会话的下钻颗粒度；
  - **Store 层归因严格物理隔离（D28）**：新引擎记录严格默认归属 Unknown 档案，杜绝串染用户既有的 Claude Code 订阅档案；
  - **Cursor 接入指引**：明确 Cursor 本地无 Token 字段事实，提供无缝的本地代理模式（`127.0.0.1:4040`）接入指引。
- **UI 极简工控微徽标矩阵升级**：
  - 会话列表与会话飞行记录仪全面适配 `[Cline]`（冷蓝）、`[Roo]`（焦糖橙）、`[Aider]`（靛紫）、`[Cursor]`（银灰）微徽标；
  - 维持 9px 等宽字体，严格满足 WCAG AA 4.5:1 对比度标准；
  - 国际化文案全面升级自适应多引擎提示。
- **飞行面板（HUD 挂件）与状态栏全景体验升级**：
  - **飞行面板 6 大指标动态可配置**：支持当前速率、激活会话总 Token、当前模型、缓存率、输入 Token 数、输出 Token 数自定义勾选，默认显示 `[当前速率 ｜ 激活会话总 Token ｜ 缓存率]`；
  - **右键原生配置菜单与动态自适应拉伸**：右击飞行面板弹出原生 macOS 勾选菜单，最少保留 2 项，悬浮胶囊根据指标数量自适应调整宽度并以细竖线分隔；
  - **瞬时生成速率算法纠偏**：修正以往将输入与上下文缓存混入计算导致虚高为 `K/s` 的缺陷，生成速率严格按照活跃窗口内纯输出 Token 统计，还原真实的 `token/s`；
  - **状态栏多选与缓存率接入**：状态栏新增「缓存率」指标选项，升级为多选支持（1-2 项多选，以 `•` 点号间隔展示）；
  - **状态栏菜单文案清晰化**：将「显示悬浮窗 / 隐藏悬浮窗」替换为符合直觉的「显示主面板 / 隐藏主面板」；
  - **macOS 程序坞（Dock）智能联动**：关闭主面板时自动移出 Dock，重新唤起主面板时自动恢复 Dock 图标；
  - **首页活动折线图丝滑趋势升级**：引入高斯核平滑采样与 Catmull-Rom 三次贝塞尔样条拟合，搭配主题自适应渐变面积衬底，消除突兀折角与零落针刺。

### Changed
- 项目各组件版本号统一升级至 `0.4.0`；
- 聚合层 `detect_engine` 与会话飞行记录仪全景支持多引擎切片。
## [0.3.0] - 2026-10-20

### Added
- **Flight Recorder 会话飞行记录仪**：
  - 点击主看板任一会话，平滑下钻至全屏「会话飞行记录仪」视图；
  - 呈现起止时刻、交互总时长、请求总次数、四维 Token 宏观指标及公网基准成本估算；
  - **逐次吞吐脉冲谱带（Pulse Strip）**：交互式柱状谱带，直观对比实算输入、输出与缓存命中分布，支持点击柱形定位请求；
  - **科学突刺（Spike）客观归因**：基于数据驱动双门限判定，客观标识「未缓存输入主导」、「缓存写入/上下文填充」、「长文本生成」；
  - **跃迁检测与子链隔离**：隔离主链与子链（sidechain）事件，模型切换与订阅档案切换仅在同链序列内计算，消除交替虚假告警；
  - **LIVE 实时联动**：新增定向事件 `session-flight-updated`，当前查看的活跃会话在后台写入新记录时自动防抖增量更新。
- **多数据源扩展（Multi-Engine Ingestion）**：
  - 支持 OpenAI Codex CLI 本地会话日志（`~/.codex/sessions/**/rollout-*.jsonl`）与配置（`~/.codex/config.toml`）；
  - **Fast State Replay（极速状态重放）**：增量断点续读时毫秒级重放会话元数据（session_meta / turn_context），确保模型信息 100% 精准不丢；
  - **四维 Token 归一化**：针对 OpenAI 包含式 prompt 输入进行精准分离扣减，与 Claude Code 口径（净输入/输出/缓存读/缓存写）严格对齐；
  - **命名空间主键**：主键格式化为 `codex:{session_id}:{response_id}`，防止会话 resume 迁移历史记录导致覆盖或搬家；
  - **多根独立首扫与平滑容错**：独立探测 `~/.claude` 与 `~/.codex`，单工具缺失时不报假警报，正常接入已安装工具。
- **UI 来源极简工控徽标**：
  - 主看板会话列表行内增加 `[Codex]` / `[Claude]` 极简工控微徽标（9px，符合 WCAG AA 4.5:1 对比度）；
  - 会话飞行记录仪头部同步展示对应引擎来源徽标；
  - 首扫与数据缺失提示文案自适应支持双引擎（Claude Code 与 Codex CLI）。

### Changed
- 升级项目版本号至 `0.3.0`（包含 Cargo.toml、tauri.conf.json、package.json 等）；
- `SessionRow` 与 `SessionFlightRecorder` 结构扩展 `engine` 字段，契约测试同步固化。

---

## [0.2.1] - 2026-10-20

### Fixed
- 修复 macOS 启动时 tokio::spawn 无运行时上下文导致的崩溃 (SIGABRT)。

---

## [0.2.0] - 2026-10-20

### Added
- **本地代理模式（Local Proxy Mode）**：
  - 内置基于 Axum 的轻量级本地中转代理（`http://127.0.0.1:{port}`）；
  - 精确到每次请求的订阅档案归因与透传，一键接管/还原 `settings.json`；
- **桌面角落 HUD 模式（Desktop HUD）**：
  - 透明置顶悬浮胶囊，支持点击穿透（Click-Through）与最小化干扰常驻；
- **成本估算（Cost Estimation）**：
  - 内置 LiteLLM 价格表与动态公网拉取缓存（1h TTL），精准估算会话与模型花费；
- **自动配额（Auto Quota）**：
  - 支持中转站 OneAPI / NewAPI 标准 billing 接口额度查询与手动配额双模式；
- **主题切换器（Theme Switcher）**：
  - 提供 Meter Face（经典暖石墨）、Calibrated Slate（冷调石板）、Mint Pulse（极简薄荷）三套工控美学主题，全量消除 FOUC 闪烁。

---

## [0.1.0] - 2026-10-19

### Added
- 初始版本发布：
  - 自动扫描解析 Claude Code 本地 JSONL 会话记录；
  - 菜单栏 / 托盘用量展示；
  - 悬浮窗看板：今日/7天/30天/全部时段用量聚合与到达脉冲折线；
  - 模型分布与基于 `settings.json` 的订阅档案推断归因；
  - 自建 SQLite 存储，去重与幂等保障。
