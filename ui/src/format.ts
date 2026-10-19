// T6 —— 数字格式化：大数字保留千分位，分布/卡片用紧凑单位。

export function formatExact(value: number): string {
  return Math.round(value).toLocaleString("en-US");
}

export function formatCompact(value: number): string {
  const abs = Math.abs(value);
  const trim = (v: number) => (Math.round(v * 10) / 10).toString();
  if (abs >= 1e9) return `${trim(value / 1e9)}B`;
  if (abs >= 1e6) return `${trim(value / 1e6)}M`;
  if (abs >= 1e3) return `${trim(value / 1e3)}K`;
  return String(Math.round(value));
}

export function formatPercent(value: number): string {
  return `${Math.round(value * 10) / 10}`.replace(/\.0$/, "");
}
