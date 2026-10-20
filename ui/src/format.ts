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
export function formatCost(value: number | null | undefined): string {
  if (value === null || value === undefined || isNaN(value)) return "—";
  if (value === 0) return "$0.00";
  if (value >= 10.0) {
    return `$${value.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
  }
  if (value >= 0.01) {
    return `$${value.toFixed(2)}`;
  }
  return "<$0.01";
}

export function formatCostPrecise(value: number | null | undefined): string {
  if (value === null || value === undefined || isNaN(value)) return "—";
  return `$${value.toFixed(4)}`;
}


export function parseTokenInput(input: string): {
  value: number | null;
  error?: string;
} {
  const trimmed = input.trim().replace(/[,_\s]/g, "");
  if (!trimmed) return { value: null };
  const match = trimmed.match(/^(\d+(?:\.\d+)?)([kKmMbB])?$/);
  if (!match) return { value: null, error: "invalid" };
  const num = parseFloat(match[1]);
  if (isNaN(num) || num < 0) return { value: null, error: "invalid" };
  const unit = match[2]?.toUpperCase();
  let multiplier = 1;
  if (unit === "K") multiplier = 1_000;
  else if (unit === "M") multiplier = 1_000_000;
  else if (unit === "B") multiplier = 1_000_000_000;
  return { value: Math.round(num * multiplier) };
}
