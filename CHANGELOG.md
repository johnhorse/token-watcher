# Changelog

所有对 Token Watcher 的重大变更均记录于此文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.0.0/)，
版本号遵循 [语义化版本 2.0.0](https://semver.org/lang/zh-CN/)。

---

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
