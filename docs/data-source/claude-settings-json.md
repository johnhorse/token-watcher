# Claude Code settings.json 订阅归因契约

> truth-source for T2（attr）。基于 2026-10-19 本机 probe（脱敏）+ 设计 v1.1
> 「订阅档案识别规则」节（D20/D23/R2-3 已定案）。

## 文件

`~/.claude/settings.json`（JSON 对象）。相关结构：

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://api.example.com",
    "ANTHROPIC_AUTH_TOKEN": "sk-…",
    "...": "其他键与归因无关（含 CC Switch 的 ANTHROPIC_DEFAULT_*_MODEL 映射）"
  }
}
```

- 鉴权键取 `ANTHROPIC_AUTH_TOKEN` **或** `ANTHROPIC_API_KEY` 中已设置者（R2-3）；
  两者都在时优先 `ANTHROPIC_AUTH_TOKEN`
- `env` 缺失、`ANTHROPIC_BASE_URL` 缺失或不可解析 → 该时刻无归因信息（Unknown 时段）
- `settings.local.json` 本机存在但 env 为空；v0.1 只监听 `settings.json`
  （CC Switch 写全局 settings.json），local 文件合并语义记 Open Question
- 文件可能被原子写（写临时文件 + rename）：watcher 必须处理 rename 事件；
  瞬时坏 JSON（读到半截）不得产生时间线事件，保持上一个已知态

## 归一化与指纹（永不持久化原文）

- `endpoint` = 规范化的 `scheme://host:port`（URL 解析后重组；默认端口可省略；
  path 差异不区分——提示但不拆档案）
- `credential_fingerprint` = 鉴权键值的 SHA-256 前 8 位 hex；无鉴权键 → NULL

## 档案身份（D23 定案）

- **档案身份 = (endpoint, credential_fingerprint) 组合**。首次见到新组合 →
  分配新的持久档案 ID；再次见到同一组合 → 复用同一档案
- 同 endpoint 不同指纹交替出现 = 两个档案（各自的 timeline 区间）——这是推断
  的诚实边界：无法区分「凭据轮换」与「同址双订阅」，宁可显示两个真实档案
  也不错误合并
- `display_name` 默认从 host 生成：去 `api.` 前缀、首段首字母大写
  （`api.deepseek.com` → `DeepSeek`）；`api.anthropic.com` 特判「Anthropic 官方」；
  `127.0.0.1`/`localhost` → 「本地代理 (port)」；用户可改

## 时间线语义（D20 诚实降级）

- timeline 半开区间 `[start_ms, end_ms)`，切换事件关闭旧区间、开启新区间
- **切换宽限期**（默认 300s，与激活会话阈值同源，进 OQ4 按体感调）：切换时刻
  起宽限期内到达的 usage 归 **Unknown**（正在运行的会话可能仍用旧中转——
  归因不可信就归 Unknown，宁缺勿错）；宽限期后归新档案
- 装工具前的全部历史（timeline 为空之前的时段）→ 全部 Unknown
- Unknown 的实现 = `usage_records.profile_id IS NULL`（不设 Unknown 档案行）
- 手动修正 = 调整 timeline 区间 + 回填 UPDATE 受影响 usage_records（store 层
  提供回填函数，修正 UI 在 T6 接线）
