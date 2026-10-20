# token-watcher

跨平台（macOS + Windows）AI token 实时监控桌面小组件。读取 Claude Code（`~/.claude/projects/**/*.jsonl`）
与 OpenAI Codex CLI（`~/.codex/sessions/**/rollout-*.jsonl`）本地会话数据，菜单栏 + 悬浮窗实时展示 token 消耗；
多订阅/中转用户可按订阅档案归因统计每个后端各消耗了多少。

<!-- 截图占位：macOS 菜单栏 + 悬浮窗（v0.1 发布时补） -->
<!-- 截图占位：Windows 托盘 + 悬浮窗（v0.1 发布时补） -->

## 功能（v0.1 规划）

- **菜单栏 / 托盘**：macOS 菜单栏 title 显示当前激活会话总 token（分钟级刷新）；
  Windows 托盘静态图标 + tooltip 数字 + 点击弹出悬浮窗
- **悬浮窗**（380×560）：今日总量大数字、到达脉冲折线（最近 1 小时）、模型分布、
  订阅档案卡片、会话列表；今天 / 7 天 / 30 天 / 全局时段切换
- **订阅档案归因（推断）**：监听 `~/.claude/settings.json` 的 env 变化建立时间线，
  按 `ANTHROPIC_BASE_URL` 把每条用量归到当时的订阅档案；无法归入的时间段
  （含装工具前的全部历史）进「Unknown」档案。**归因是推断不是事实**——UI 以
  「推断 · 低置信」明确标注，精确归因（本地代理模式）在 v0.2 交付
- **手动额度配额条**：订阅档案卡片上纯手动配置额度 / 已用 / 重置日
- **自建 SQLite 存储**：删除源 JSONL 历史仍在；重启重扫绝不双计（uuid 去重 + checkpoint）
- **隐私**：永不持久化认证 token，只存 `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_API_KEY`
  的 SHA-256 指纹前 8 位用于区分同地址不同订阅
- **中英文**，跟随系统语言；Meter Face 主题（暖石墨 + 琥珀信号色）

## v0.3 新特性

- **Flight Recorder 会话飞行记录仪**：
  - 主看板会话平滑下钻；
  - 逐次 API 往返吞吐脉冲谱带与实算/缓存命中对比；
  - 突刺（Spike）科学数据驱动归因与子链（sidechain）隔离；
  - LIVE 实时会话联动与增量自动刷新。
- **多数据源扩展（Multi-Engine Ingestion）**：
  - 完整并行接入 OpenAI Codex CLI 日志与 `config.toml` 配置；
  - Fast State Replay 极速状态重放，杜绝断点续读时的空模型与脏模型；
  - 四维互斥 Token 归一化与命名空间去重主键；
  - 会话列表与飞行记录仪行内极简工控徽标（`[Codex]` / `[Claude]`）。

## 安装

v0.1 尚未发布。发布后从 [GitHub Releases](https://github.com/johnhorse/token-watcher/releases)
下载：macOS `.dmg`（universal）/ Windows `.msi`。

> 未签名说明：macOS 首次打开需右键 →「打开」（Gatekeeper）；Windows SmartScreen
> 点「更多信息」→「仍要运行」。代码签名视社区反馈后置。

## 本地构建

要求：Rust stable（≥ 1.90）、Node.js ≥ 20、npm。

```bash
npm --prefix ui install            # 安装前端依赖
npm --prefix ui exec tauri dev     # 开发模式（热更新）
npm --prefix ui exec tauri build   # 产出安装包（构建前会自动执行 ui 的 npm run build）
```

> Tauri CLI 必须从仓库根目录运行（它在根目录下定位 `src-tauri/tauri.conf.json`）；
> `beforeBuildCommand` 的 `npm run build` 由 CLI 在 `ui/` 目录下执行。

数据层验证工具（开发用，不入安装包）：

```bash
cd src-tauri
cargo run --example tw_ingest # 全量解析 → SQLite，打印今日总量
cargo test                  # 单元 + fixture 集成测试
```

## 文档

- [设计文档 v1.1（权威）](docs/designs/token-watcher-design-v1.1.md) —— 2026-10-19
  office hours 批准 + 工程评审 D14-D23 修正案写回
- [Claude Code JSONL 数据源契约](docs/data-source/claude-code-jsonl.md)
- [线框图](docs/designs/assets/token-watcher-wireframe-20261019.png)

> **字体说明**：UI 声明了 Archivo（Latin）与 Fragment Mono（数字）字体名并回退
> 系统字体栈（PingFang SC / Microsoft YaHei UI 等），正式字体文件尚未打包——
> 当前视觉为系统栈渲染，待图标与字体设计定稿后随安装包分发。

## License

MIT
