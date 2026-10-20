// T6 —— 轻量 i18n：自研字典 + navigator.language 检测（zh* → zh，否则 en）。
// URL ?lang=zh|en 仅供开发/自查覆盖。

export type Lang = "zh" | "en";

const zh = {
  "range.today": "今天",
  "range.week": "7 天",
  "range.month": "30 天",
  "range.all": "全部",

  "total.today": "今日 tokens",
  "total.week": "7 天 tokens",
  "total.month": "30 天 tokens",
  "total.all": "全部 tokens",

  "breakdown.input": "输入",
  "breakdown.output": "输出",
  "breakdown.cacheRead": "缓存读",
  "breakdown.cacheWrite": "缓存写",
  "cost.estimated": "估算花费",
  "cost.estimateTooltip": "基于公网模型基准价估算",
  "cost.unpricedNote": "含 {n} 未定价 Token",

  "activity.title": "实时活动 · 最近 1 小时",
  "activity.metric": "Reported tokens/s",
  "activity.live": "LIVE",
  "activity.idle": "空闲",
  "activity.empty": "最近 1 小时无到达",
  "activity.aria": "到达脉冲折线，报告 tokens/s，最近 1 小时",

  "models.title": "模型分布",

  "profiles.title": "订阅档案",
  "profiles.inferred": "推断 · 低置信",
  "profiles.manual": "手动",
  "profiles.unknown": "未知",
  "profiles.quotaUsed": "已用 {pct}%",
  "profiles.quotaOf": "{used} / {total}",
  "profiles.reset": "重置 {date} · 剩 {days} 天",
  "profiles.resetToday": "今天为重置日 · 手动清零",
  "profiles.resetPast": "已过重置日 · 手动清零",
  "profiles.noQuota": "未配置额度",
  "profiles.edit": "编辑",
  "profiles.nameLabel": "档案名称",
  "profiles.totalLabel": "总额度",
  "profiles.usedLabel": "已用额度",
  "profiles.resetLabel": "重置日期",
  "profiles.save": "保存",
  "profiles.cancel": "取消",
  "profiles.saving": "保存中...",
  "profiles.tokenHint": "如 100M、500K 或纯数字",
  "profiles.invalidNumber": "格式无效",
  "profiles.noLimit": "不设限",
  "profiles.saveError": "保存失败",

  "sessions.title": "会话",
  "sessions.active": "活跃",

  "scan.title": "首次扫描",
  "scan.detail": "{done} / {total} 文件 · {pct}%",
  "scan.emptyTitle": "未找到 ~/.claude 数据",
  "scan.emptyHint": "使用 Claude Code 后，用量会出现在这里。",

  "health.root": "无法读取 ~/.claude 目录 · 数据可能不完整",
  "health.skipped": "跳过 {n} 文件",
  "health.readonly": "数据库只读 · 显示已有数据，实时更新暂停",
  "health.unavailable": "数据暂时不可用 · 将在下次更新时重试",

  "proxy.title": "本地代理",
  "proxy.running": "已就绪 127.0.0.1:{port}",
  "proxy.stopped": "未运行",
  "proxy.upstream": "上游: {url}",
  "proxy.copyCmd": "复制命令",
  "proxy.copied": "已复制",
  "proxy.inject": "接管 settings.json",
  "proxy.restore": "还原 settings.json",
  "proxy.injectedBadge": "已接管",
} as const;

export type MessageKey = keyof typeof zh;

const en: Record<MessageKey, string> = {
  "range.today": "Today",
  "range.week": "7d",
  "range.month": "30d",
  "range.all": "All",

  "total.today": "Tokens today",
  "total.week": "7-day tokens",
  "total.month": "30-day tokens",
  "total.all": "All tokens",

  "breakdown.input": "in",
  "breakdown.output": "out",
  "breakdown.cacheRead": "cache read",
  "breakdown.cacheWrite": "cache write",
  "cost.estimated": "Est. Cost",
  "cost.estimateTooltip": "Estimated based on public benchmark pricing",
  "cost.unpricedNote": "{n} unpriced tokens",

  "activity.title": "Live activity · last hour",
  "activity.metric": "Reported tokens/s",
  "activity.live": "LIVE",
  "activity.idle": "Idle",
  "activity.empty": "No arrivals in the last hour",
  "activity.aria": "Arrival pulse chart, reported tokens per second, last hour",

  "models.title": "Model distribution",

  "profiles.title": "Subscriptions",
  "profiles.inferred": "Inferred · low confidence",
  "profiles.manual": "Manual",
  "profiles.unknown": "Unknown",
  "profiles.quotaUsed": "Used {pct}%",
  "profiles.quotaOf": "{used} / {total}",
  "profiles.reset": "Resets {date} · {days}d left",
  "profiles.resetToday": "Reset day · clear manually",
  "profiles.resetPast": "Past reset date · clear manually",
  "profiles.noQuota": "No quota set",
  "profiles.edit": "Edit",
  "profiles.nameLabel": "Name",
  "profiles.totalLabel": "Total quota",
  "profiles.usedLabel": "Used quota",
  "profiles.resetLabel": "Reset date",
  "profiles.save": "Save",
  "profiles.cancel": "Cancel",
  "profiles.saving": "Saving...",
  "profiles.tokenHint": "e.g. 100M, 500K or raw number",
  "profiles.invalidNumber": "Invalid format",
  "profiles.noLimit": "No limit",
  "profiles.saveError": "Save failed",

  "sessions.title": "Sessions",
  "sessions.active": "Active",

  "scan.title": "First scan",
  "scan.detail": "{done} / {total} files · {pct}%",
  "scan.emptyTitle": "No ~/.claude data found",
  "scan.emptyHint": "Usage appears here after you use Claude Code.",

  "health.root": "Cannot read ~/.claude · data may be incomplete",
  "health.skipped": "{n} files skipped",
  "health.readonly":
    "Database read-only · showing stored data, live updates paused",
  "health.unavailable": "Data unavailable · will retry on next update",

  "proxy.title": "Local Proxy",
  "proxy.running": "Ready 127.0.0.1:{port}",
  "proxy.stopped": "Stopped",
  "proxy.upstream": "Upstream: {url}",
  "proxy.copyCmd": "Copy",
  "proxy.copied": "Copied",
  "proxy.inject": "Inject into settings.json",
  "proxy.restore": "Restore settings.json",
  "proxy.injectedBadge": "Injected",
};

const dict: Record<Lang, Record<MessageKey, string>> = { zh, en };

function detectLang(): Lang {
  const override = new URLSearchParams(window.location.search).get("lang");
  if (override === "zh" || override === "en") return override;
  return (navigator.language || "").toLowerCase().startsWith("zh") ? "zh" : "en";
}

export const lang: Lang = detectLang();

document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";

export function t(
  key: MessageKey,
  params?: Record<string, string | number>,
): string {
  let text = dict[lang][key];
  if (params) {
    for (const [name, value] of Object.entries(params)) {
      text = text.replaceAll(`{${name}}`, String(value));
    }
  }
  return text;
}
