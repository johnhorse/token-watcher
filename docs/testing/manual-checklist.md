# token-watcher v0.1 手验清单（D18）

> 自动化覆盖：`cd src-tauri && cargo test`（T1/T2/T3+T4/T5/T7/T8）。
> 本清单只列 **UI/平台行为**——无法被 cargo test 覆盖的验收项。
> SC3（重启不双计）、SC4（删源留史）已由 t1/t8 自动化覆盖，不在此重复；
> SC8 的写者互斥已由 t34 自动化覆盖，单实例插件行为仍按本文末尾手验。

记录约定：每项执行后填 `✅ 通过 / ❌ 失败 / ⚠️ 阻塞`，附平台、日期、执行人、备注。

| 平台 | 状态 |
|---|---|
| macOS（开发机） | 本清单主执行环境 |
| Windows | 标有「Windows」的条目待 CI 或真机执行（标记：CI/后续） |

## 0. 通用准备

### 0.1 启动应用

在仓库根目录：

```bash
npm --prefix ui install
npm --prefix ui exec tauri dev
```

首次编译较慢；后续热更新。应用启动后应看到悬浮窗（380×560）与菜单栏/托盘图标。

### 0.2 造数脚本（追加一条合成 usage）

在另一个终端执行（macOS；Windows 用 PowerShell 等价写法，标 CI/后续）：

```bash
TW_DIR="$HOME/.claude/projects/tw-manual"
mkdir -p "$TW_DIR"
UUID=$(uuidgen | tr 'A-Z' 'a-z')
TS=$(date -u +%Y-%m-%dT%H:%M:%S.000Z)
cat >> "$TW_DIR/manual.jsonl" <<EOF
{"type":"assistant","uuid":"$UUID","session_id":"manual-session","sessionId":"manual-session","timestamp":"$TS","message":{"model":"manual-test","usage":{"input_tokens":1000,"output_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}
EOF
```

该记录：input 1000 / output 100，总量 +1100。改动数值可测不同指标。
注意：`tw-manual/manual.jsonl` 是真实数据源文件，应用会永久入库（SC4 语义）；
如需完全重置见 0.3。

### 0.3 数据重置（可选，仅开发机）

```bash
# 先退出应用（托盘菜单「退出」）
rm -f "$HOME/.claude/projects/tw-manual/manual.jsonl"
rm -f "$HOME/.token-watcher/db.sqlite3"   # 会清空全部历史，谨慎
```

### 0.4 平台注记

- macOS：菜单栏 title 为文本数字；左键点击图标弹出菜单；tooltip 显示完整数字。
- Windows：托盘为静态图标 + tooltip 数字；**左键直接显示/聚焦悬浮窗**，菜单走右键。
  相关条目统一标「Windows（CI/后续）」。

---

## 1. SC1 — 悬浮窗实时性（usage 落盘后 <2s 可见）

**前置**：应用运行、悬浮窗可见（今天 range）、数据目录可读。

**步骤**
1. 记下大数字与四项 breakdown 当前值。
2. 执行 0.2 造数脚本追加一条记录，同时开始计时。
3. 观察悬浮窗大数字、breakdown、折线。

**期望**
- ≤2s 内大数字 +1100，breakdown「输入 +1000 / 输出 +100」。
- 折线出现一个新到达脉冲；无需手动刷新或切换 chips。
- 大数字滚动动画（若开启 reduced-motion 则直接跳变，见 SC6）。

**对应 SC**：SC1（兼 D16 完成后的无缝切换）

---

## 2. SC2 — 托盘刷新（用量变化后 ≤60s 反映）

**前置**：托盘指标为「总 Token」（见第 7 节切换方法）；应用运行。

**步骤**
1. 记录菜单栏 title / tooltip 数值。
2. 执行 0.2 造数脚本。
3. 计时观察菜单栏数字变化。

**期望**
- ≤60s 内 title（紧凑数字）与 tooltip（完整数字）都更新；正常应 <1s
  （数据线程落库后立即通知托盘，60s 只是兜底）。
- 数值 = 悬浮窗对应当前指标的数字（SC7 同源）。

**Windows（CI/后续）**：tooltip 同节奏更新；无文本 title。

**对应 SC**：SC2

---

## 3. SC6 — 空闲 CPU≈0、折线冻结、reduced-motion

**前置**：应用空闲（无 Claude Code 活动）、悬浮窗可见。

**步骤**
1. 打开「活动监视器」→ 搜索 token-watcher → 观察 60s CPU。
2. 追加一条记录产生脉冲，然后静置 30s。
3. 采样两次进程 CPU 时间（间隔 60s）：
   ```bash
   ps -o pid,pcpu,time,comm -p "$(pgrep -x token-watcher)"
   ```
4. 开启 macOS 系统设置 → 辅助功能 → 显示 → **减弱动态效果**，重启应用后追加一条记录。

**期望**
- 空闲 CPU ≈0%（60s 均值建议 ≤0.3%，没有周期性尖峰）。
- 无 coding 活动时无轮询：两次 `time` 采样差值接近 0，无周期性磁盘/网络活动。
- 脉冲 30s 后折线停止左漂、前导点变空心（冻结渲染）。
- reduced-motion 下：大数字无滚动动画、折线无脉冲动画，数值仍即时更新。

**对应 SC**：SC6（兼 v1.1 reduced-motion 要求）

---

## 4. SC7 — 口径一致性（托盘 vs 悬浮窗）

> 说明：托盘四选一 = 总 Token / 输入 / 缓存读取 / 输出；「缓存写」只在悬浮窗
> breakdown 出现，其聚合口径由 t5/t7 自动化覆盖。

**前置**：为便于精确比对，建议先按 0.3 重置数据；启动应用；执行 0.2 造数脚本
（input 1000 / output 100）。若有其他 Claude 活动，等其结束 ≥5 分钟。

**步骤**
1. 托盘选「总 Token」，对比 tooltip 完整数字与悬浮窗「今日 tokens」大数字。
2. 托盘切「输入 Token」，对比 tooltip 数字与悬浮窗 breakdown「输入」。
3. 托盘切「输出 Token」「缓存读取 Token」，重复对比。
4. 静置 ≥5 分钟（无新记录）后再看「总 Token」：此时应回退为今日累计，
   与悬浮窗今日大数字完全相等。

**期望**
- 每一步两个表面数字**完全相等**（不使用四舍五入的近似比较；悬浮窗 breakdown
  是 compact 显示，若数值 >999 请以「总 Token」或小数值场景核对精确值）。
- 静置 5 分钟后托盘从「当前会话总量」平滑回退到「今日累计」，数值不跳错。

**对应 SC**：SC7

---

## 5. SC9 — 首扫进度态与无缝切换（D16）

**前置**：应用退出。`~/.claude/projects` 下已有较多 jsonl（真实使用过的机器通常
足够；不足时可复制若干合成 jsonl 到 `~/.claude/projects/tw-manual/` 增加文件数）。

**步骤**
1. 启动应用，立即观察悬浮窗。
2. 观察骨架态文案、文件计数与百分比推进。
3. 首扫结束瞬间观察切换。
4. **空态**：退出应用 → 临时改名 `mv ~/.claude/projects ~/.claude/projects.bak`
   （先确保没有正在运行的 Claude Code 会话）→ 启动应用。
5. 恢复 `mv ~/.claude/projects.bak ~/.claude/projects`，重新启动应用。

**期望**
- 首扫期间：显示「首次扫描」骨架 +「done / total 文件 · pct%」，
  进度单调不回退（D16）。
- 完成瞬间：直接切到真实数字，布局同高、无闪跳；随后收到一次
  `dashboard-dirty` 全量水合（视觉上只更新一次）。
- 空态：显示「未找到 ~/.claude 数据」+「使用 Claude Code 后，用量会出现在这里。」
  **不是**错误横幅；恢复目录重启后正常显示数字。
- 若启动时窗口处于隐藏态：首扫完成后重新显示窗口应立即水合出数字（不空白）。

**对应 SC**：SC9（兼 D16 空态复用）

---

## 6. SC10 — 三档数据层失败可见且可恢复（D17）

### 6.1 第一档：根目录不可读

**前置**：应用退出；`~/.claude/projects` 存在。

**步骤**
1. `chmod 000 ~/.claude/projects`
2. 启动应用。
3. `chmod 700 ~/.claude/projects` 恢复权限。
4. 通过托盘「显示/隐藏悬浮窗」隐藏再显示窗口。

**期望**
- 启动后显示横幅「无法读取 ~/.claude 目录 · 数据可能不完整」，骨架态结束
  （不是永远停在 scanning）。
- 恢复权限并重新显示窗口后，横幅消失且数据自动补扫出现（`WindowShown`
  触发 quiet_scan，无轮询）。
- **务必恢复权限**；若终端已无法进入该目录，执行
  `chmod 700 ~/.claude/projects`（对父目录有权限即可）。

### 6.2 第二档：单文件跳过

**前置**：应用运行、悬浮窗可见。

**步骤**
1. 创建不可读文件：
   ```bash
   touch "$HOME/.claude/projects/tw-manual/broken.jsonl"
   chmod 000 "$HOME/.claude/projects/tw-manual/broken.jsonl"
   ```
2. 等待 1–2s（fs 事件触发增量 ingest）。
3. `chmod 600 .../broken.jsonl` 恢复。

**期望**
- 总量行右侧出现角标「跳过 1 文件」；其余文件与数字不受影响（D17 第二档）。
- 跳过计数是进程内累计，重启归零；恢复权限后本次运行不会自动减计数。
- 删除 `broken.jsonl` 后历史数据仍在（SC4）。

### 6.3 第三档：DB 只读降级

**前置**：应用退出；`~/.token-watcher/db.sqlite3` 存在。

**步骤**
1. `mv ~/.token-watcher/db.sqlite3 /tmp/tw-db-backup.sqlite3`
2. `mkdir ~/.token-watcher/db.sqlite3`（让 SQLite 无法打开）
3. 启动应用。
4. `rmdir ~/.token-watcher/db.sqlite3 && mv /tmp/tw-db-backup.sqlite3 ~/.token-watcher/db.sqlite3`
5. 等待 ≤10s（DB 重试间隔）。

**期望**
- 启动后显示横幅「数据库只读 · 显示已有数据，实时更新暂停」，骨架态结束。
- 恢复数据库文件后 ≤10s 横幅消失，数据自动恢复并补扫断档期间的变更
  （try_reopen_store + quiet_scan）。
- 恢复期间不崩溃、不需要重启应用。

**对应 SC**：SC10（兼 D17 三档）

---

## 7. 托盘菜单手验（T5 遗留 ⚠️ 项）

**前置**：应用运行。macOS 左键点菜单栏图标；Windows 右键点托盘图标（左键直接开窗）。

**步骤与期望**

1. **切指标**
   - 菜单含四个可勾选项：总 Token / 输入 Token / 缓存读取 Token / 输出 Token。
   - 逐项点击：勾选标记移动；title 与 tooltip 立即按新指标刷新；数字与悬浮窗
     对应项一致（SC7）。
   - 退出并重启：设置持久化（`~/.token-watcher/settings.json` 中
     `"tray_metric"` 为所选值）。
2. **显示/隐藏悬浮窗**
   - 点击「显示/隐藏悬浮窗」：窗口隐藏，托盘图标仍在；再次点击：窗口显示并聚焦。
   - 隐藏期间新增用量：重新显示后数字已完成水合（不丢事件、不重复计数）。
3. **退出**
   - 点击「退出」：应用进程退出、托盘图标消失；写者锁释放——立即重新启动应成功。
4. **文案/i18n**
   - 中文系统显示中文菜单；英文系统显示英文（托盘 i18n 随 T5 一起验收；以当前
     i18n 字典为准）。
5. **Windows（CI/后续）**
   - 左键点击直接显示/聚焦悬浮窗；右键弹菜单；tooltip 数字与 60s 刷新节奏；
     静态图标（无动态 title）。

**对应 SC**：SC2/SC7/SC8（单实例部分）/T5 手验遗留

---

## 8. 双平台收尾项

| 项 | macOS | Windows |
|---|---|---|
| 构建 + 启动 | 本地手验 | CI/后续（GitHub Actions matrix） |
| 悬浮窗布局 380×560、不缩放 | 本地手验 | CI/后续 |
| 托盘点击/菜单/退出 | 本地手验 | CI/后续 |
| 双开：无第二托盘，第二实例唤起主窗并退出（SC8） | `open -n` 启动第二个实例验证 | CI/后续 |
| 系统语言切换后的 i18n | 本地手验（改系统语言或 `?lang=en`） | CI/后续 |

---

## 9. 附加：chips 切换四区块全局过滤（Test Plan Key Interaction）

**前置**：应用运行、已有跨今天/7 天/30 天/更早的数据。

**步骤**
1. 依次点击「今天 / 7 天 / 30 天 / 全部」四个 chips。
2. 每次切换后观察：大数字标签与数值、模型分布、订阅档案、会话列表。
3. 快速连续点击多个 chips（模拟竞态）。

**期望**
- 大数字标签随 range 变化（今日 tokens / 7 天 tokens / 30 天 tokens / 全部 tokens）。
- 模型、档案、会话三块随之**全局过滤**；折线固定最近 1 小时，不随 chips 变化。
- 有数据时数值满足 today ≤ 7 天 ≤ 30 天 ≤ 全部。
- 快速连点只呈现最后一次选择的响应，不被旧响应覆盖（UI 有请求序号保护）。

**对应**：Test Plan Key Interactions（chips 全局过滤）

---

## 10. 执行记录

| # | 项目 | 平台 | 结果 | 执行人 | 日期 | 备注 |
|---|---|---|---|---|---|---|
| 1 | SC1 悬浮窗实时性 | macOS |  |  |  |  |
| 2 | SC2 托盘刷新 | macOS |  |  |  |  |
| 3 | SC6 空闲 CPU / 冻结 / reduced-motion | macOS |  |  |  |  |
| 4 | SC7 口径一致性 | macOS |  |  |  |  |
| 5 | SC9 首扫进度 / 空态 | macOS |  |  |  |  |
| 6 | SC10.1 根目录不可读 | macOS |  |  |  |  |
| 7 | SC10.2 单文件跳过 | macOS |  |  |  |  |
| 8 | SC10.3 DB 只读降级 | macOS |  |  |  |  |
| 9 | 托盘切指标 / 显示隐藏 / 退出 | macOS |  |  |  |  |
| 10 | 双开唤起主窗（SC8） | macOS |  |  |  |  |
| 11 | 上述平台项 | Windows | CI/后续 |  |  |  |
