# Claude Code JSONL 数据源契约

> truth-source for T1（parse/ingest/store）。基于 2026-10-19 本机 live probe
> （362 个文件抽样验证）+ 工程评审 D22 更新语义定案。
> Schema 变更风险（Open Question 2）：Claude Code 版本升级可能改字段——解析器
> 必须容错：缺字段跳过并计数，未知字段忽略，绝不因 schema 变化 panic。

## 文件布局

```
~/.claude/projects/<project-dir>/<sessionId>.jsonl
```

- `<project-dir>` = 会话工作目录路径的转写（`/` → `-`），如 `-Volumes-Data-projects-token-watcher`
- `<sessionId>.jsonl` 文件名即会话 ID（UUID）
- 目录可能不存在、为空、含嵌套子目录——枚举必须递归且容忍任意一种

## 记录筛选

每行一个 JSON 对象，`type` 种类很多（user / assistant / attachment / last-prompt /
mode / permission-mode / file-history-snapshot / cost-state / …）。**只有
`type == "assistant"` 且带 `message.usage` 的行是用量记录**，其余全部忽略
（不报错、不计坏行）。probe 确认：user 行从不带 usage。

## 字段映射（assistant 用量记录）

| 契约字段 | JSONL 来源 | 说明 |
|---|---|---|
| record_id | `message.id` →（缺失回退）`requestRef` →（再回退）`uuid` | **逻辑身份（2026-10-19 修订）**：一次 assistant 回复按 content block 写多行、每行复制整轮 usage，行级 uuid 会按块数放大计量（实测虚高 ~2.4 倍）；message.id 是一次 API 调用的身份，多块共享 |
| session_id | `session_id`，缺失回退 `sessionId` | 两字段并存同值（新旧 schema），优先前者 |
| timestamp | `timestamp` | ISO-8601 UTC（`2026-10-19T10:03:45.883Z`），解析失败则跳过该行 |
| input_tokens | `message.usage.input_tokens` | 直读，四项之一 |
| output_tokens | `message.usage.output_tokens` | 直读，四项之一 |
| cache_read_tokens | `message.usage.cache_read_input_tokens` | 直读，四项之一 |
| cache_creation_tokens | `message.usage.cache_creation_input_tokens` | 直读，四项之一 |
| model | `message.model` | 实际服务模型（中转映射后的真名，如 `glm-5.3`）。注意 `requestedModel` 是请求名（如 `claude-opus-5[1m]`），CC Switch 场景两者不同——模型维度一律用 `message.model` |
| entrypoint | `entrypoint` | 程序维度（`cli` 等），缺失置空串 |
| is_sidechain | `isSidechain` | subagent 记录标记，true 也计入（属同一 session 的消耗） |
| cwd | `cwd` | 会话工作目录（快照），可缺失 |
| cc_version | `version` | Claude Code 版本（如 `2.1.303`），诊断用 |

`message.usage` 里其余键（`output_tokens_details` / `cache_creation.ephemeral_*` /
`service_tier` / `speed` …）v0.1 不采集。

四项判定（T1 实测定案，2026-10-19 修订）：**四项中至少一项存在且为非负整数
→ 入库，缺失项按 0 补**；四项均缺失，或任一「存在」的字段非数字 / 为负 → 坏行。
依据：真实数据中 team/subagent 占位记录（约 364 文件中 27 个、2675 条）只带
input/output 无 cache 字段，属合法观测——若判坏行，D17 的「跳过 N 行」信号会被
这些占位行主导，制造假警报。cache 缺失 = 无缓存计费，补 0 语义正确。

## 更新语义定案（D22，2026-10-19 修订）

- **逻辑记录身份 = `message.id`（回退 `requestRef`，再回退 `uuid`）**
  （单列主键，跨文件去重；uuid 仅为老 schema 兜底——它每行唯一、
  无法折叠同一次调用的多个 content block）
- **版本优先级 = 后到者胜**：JSONL 只追加，同 uuid 再出现时，文件中位置靠后的行
  是更新态；跨文件（resume 场景）按导入顺序后者胜
- **UPSERT（REPLACE 语义），绝不累加**：同 uuid 重放不产生第二次计量——这是
  SC3「重启不双计」的机制保证
- resume 会话可能产生新文件；因 uuid 全局去重，无论 Claude Code 如何切文件都不会双计

## 行级三态（parse 层）

1. **完整行**：JSON parse 成功且字段映射有效 → 产出记录
2. **不完整尾行**：文件末尾 JSON parse 失败——写入进行中的瞬态。缓冲该行字节，
   下次增量读取时拼接重试（checkpoint 记在最后一个完整行末尾）
3. **坏行**：中间位置的 parse 失败或字段无效 → 跳过 + 计数（D17 跳过计数暴露给 UI）

## checkpoint 约定

每文件 `(byte_offset, mtime, size)` 三元组。恢复时 size < 记录 offset 或 mtime
回退 → 判定截断/轮转 → 该文件从 0 重扫（uuid UPSERT 保证幂等）。
