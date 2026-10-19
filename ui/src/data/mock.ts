// T6 —— mock adapter：纯浏览器开发/自查数据源，形状与 ui-data-contract.md 一致。
// 开发开关（仅 mock）：?health=root,skip,readonly 触发三档错误态；
// ?pulses=off 停止脉冲（用于验证 30s 空闲冻结）；?scan=empty 进入空态骨架。

import { lang } from "../i18n";
import type {
  Dashboard,
  DataHealth,
  ModelRow,
  ProfileCard,
  Quota,
  Range,
  ScanStatus,
  SessionRow,
  TokenTotals,
  UiAdapter,
  UsagePulse,
} from "./types";

const params = new URLSearchParams(window.location.search);
const PULSES_ENABLED = params.get("pulses") !== "off";
const EMPTY_SCAN = params.get("scan") === "empty";
const HEALTH_FLAGS = (params.get("health") ?? "").split(",");
// ?fail=fetch：前两次 get_dashboard 失败（首拉 + 500ms dirty 重试），
// 停脉冲（?pulses=off）时横幅保持到下一次事件触发恢复。
let remainingFailures = params.get("fail") === "fetch" ? 2 : 0;

const INITIAL_HEALTH: DataHealth = {
  root_unreadable: HEALTH_FLAGS.includes("root"),
  skipped_files: HEALTH_FLAGS.includes("skip") ? 2 : 0,
  db_readonly: HEALTH_FLAGS.includes("readonly"),
};

const TOTAL_FILES = 364;

const BASE_TOTALS: Record<Range, TokenTotals> = {
  today: {
    input: 8_412_000,
    output: 2_108_000,
    cache_read: 1_812_000,
    cache_creation: 105_209,
    total: 12_437_209,
  },
  week: {
    input: 52_940_000,
    output: 13_260_000,
    cache_read: 11_400_000,
    cache_creation: 660_000,
    total: 78_260_000,
  },
  month: {
    input: 168_400_000,
    output: 42_600_000,
    cache_read: 37_200_000,
    cache_creation: 2_140_000,
    total: 250_340_000,
  },
  all: {
    input: 226_800_000,
    output: 57_400_000,
    cache_read: 50_100_000,
    cache_creation: 2_880_000,
    total: 337_180_000,
  },
};

const BASE_MODELS: Record<Range, ModelRow[]> = {
  today: [
    { model: "claude-opus-5", tokens: 7_100_000 },
    { model: "claude-sonnet-5", tokens: 4_200_000 },
    { model: "claude-haiku-5", tokens: 1_137_209 },
  ],
  week: [
    { model: "claude-opus-5", tokens: 44_800_000 },
    { model: "claude-sonnet-5", tokens: 26_100_000 },
    { model: "claude-haiku-5", tokens: 7_360_000 },
  ],
  month: [
    { model: "claude-opus-5", tokens: 143_200_000 },
    { model: "claude-sonnet-5", tokens: 84_700_000 },
    { model: "claude-haiku-5", tokens: 22_440_000 },
  ],
  all: [
    { model: "claude-opus-5", tokens: 192_800_000 },
    { model: "claude-sonnet-5", tokens: 114_300_000 },
    { model: "claude-haiku-5", tokens: 30_080_000 },
  ],
};

const SESSION_SCALE: Record<Range, number> = {
  today: 1,
  week: 6.3,
  month: 20.1,
  all: 27.1,
};

function splitPulse(total: number, tMs: number): UsagePulse {
  const output = Math.round(total * 0.12);
  const cache_read = Math.round(total * 0.18);
  const cache_creation = Math.round(total * 0.02);
  const input = total - output - cache_read - cache_creation;
  return {
    t_ms: tMs,
    input: Math.max(0, input),
    output,
    cache_read,
    cache_creation,
  };
}

function splitTotal(total: number): TokenTotals {
  const output = Math.round(total * 0.1694);
  const cache_read = Math.round(total * 0.1457);
  const cache_creation = Math.round(total * 0.00846);
  const input = Math.max(0, total - output - cache_read - cache_creation);
  return { input, output, cache_read, cache_creation, total };
}

function seededPulses(): UsagePulse[] {
  const now = Date.now();
  const rows: [number, number][] = [
    [58, 180_000],
    [51, 96_000],
    [44, 260_000],
    [36, 140_000],
    [29, 320_000],
    [22, 110_000],
    [15, 210_000],
    [8, 90_000],
    [0.2, 240_000],
  ];
  return rows.map(([minutesAgo, total]) =>
    splitPulse(total, now - minutesAgo * 60_000),
  );
}

function plusDays(days: number): string {
  const date = new Date();
  date.setDate(date.getDate() + days);
  return date.toISOString().slice(0, 10);
}

const pulseSubs = new Set<(pulse: UsagePulse) => void>();
const scanSubs = new Set<(status: ScanStatus) => void>();
const healthSubs = new Set<(health: DataHealth) => void>();
const dirtySubs = new Set<() => void>();

let pulses = seededPulses();
let liveDelta = { input: 0, output: 0, cache_read: 0, cache_creation: 0 };
let pulseCount = 0;
let visible = true;
let pulseTimer: ReturnType<typeof setTimeout> | undefined;

const scan: { current: ScanStatus } = {
  current: EMPTY_SCAN
    ? {
        phase: "empty",
        files_done: 0,
        files_total: 0,
        percent: 0,
        ...INITIAL_HEALTH,
      }
    : {
        phase: "scanning",
        files_done: 0,
        files_total: TOTAL_FILES,
        percent: 0,
        ...INITIAL_HEALTH,
      },
};

const displayNames = new Map<number, string>();
const quotaOverrides = new Map<number, Quota | null>();

const defaultQuotas = new Map<number, Quota | null>([
  [1, { total: 20_000_000, used: 15_600_000, reset_date: plusDays(14) }],
  [2, { total: 3_000_000, used: 1_240_000, reset_date: null }],
  [3, null],
]);

function defaultProfileName(id: number): string {
  if (id === 1) return lang === "zh" ? "Anthropic 官方" : "Anthropic Official";
  if (id === 2) return lang === "zh" ? "DeepSeek 中转" : "DeepSeek Relay";
  return lang === "zh" ? "Kimi 中转" : "Kimi Relay";
}

function liveTotal(): number {
  return (
    liveDelta.input +
    liveDelta.output +
    liveDelta.cache_read +
    liveDelta.cache_creation
  );
}

function buildTotals(range: Range): TokenTotals {
  const base = BASE_TOTALS[range];
  return {
    input: base.input + liveDelta.input,
    output: base.output + liveDelta.output,
    cache_read: base.cache_read + liveDelta.cache_read,
    cache_creation: base.cache_creation + liveDelta.cache_creation,
    total: base.total + liveTotal(),
  };
}

function buildProfiles(range: Range, totals: TokenTotals): ProfileCard[] {
  const defs = [
    { id: 1, share: 0.4439, endpoint: "https://api.anthropic.com" },
    { id: 2, share: 0.2509, endpoint: "https://api.deepseek.com" },
    { id: 3, share: 0.1737, endpoint: "https://api.moonshot.cn" },
  ];

  let used = 0;
  const known: ProfileCard[] = defs.map((def) => {
    const shared = Math.round(BASE_TOTALS[range].total * def.share);
    used += shared;
    const tokens = splitTotal(shared);
    if (def.id === 1) {
      tokens.input += liveDelta.input;
      tokens.output += liveDelta.output;
      tokens.cache_read += liveDelta.cache_read;
      tokens.cache_creation += liveDelta.cache_creation;
      tokens.total += liveTotal();
    }
    return {
      profile_id: def.id,
      display_name: displayNames.get(def.id) ?? defaultProfileName(def.id),
      endpoint: def.endpoint,
      inferred: true,
      tokens,
      quota: quotaOverrides.has(def.id)
        ? quotaOverrides.get(def.id)!
        : (defaultQuotas.get(def.id) ?? null),
    };
  });

  const unknownTotal = Math.max(1, totals.total - used);
  known.push({
    profile_id: null,
    display_name: "Unknown",
    endpoint: null,
    inferred: true,
    tokens: splitTotal(unknownTotal),
    quota: null,
  });

  return known.sort((a, b) => b.tokens.total - a.tokens.total);
}

function buildDashboard(range: Range): Dashboard {
  const totals = buildTotals(range);
  const delta = liveTotal();

  const models = BASE_MODELS[range].map((row, index) => ({
    ...row,
    tokens: row.tokens + (index === 0 ? delta : 0),
  }));

  const titles =
    lang === "zh"
      ? ["token-watcher 开发", "api-gateway 重构", "文档整理", "实验分支"]
      : ["token-watcher dev", "api-gateway refactor", "docs cleanup", "experiments"];
  const baseSessions = [3_200_000, 2_800_000, 900_000, 320_000];
  const lastActive = [60_000, 42 * 60_000, 3 * 3_600_000, 8 * 3_600_000];
  const sessions: SessionRow[] = titles.map((title, index) => ({
    session_id: `mock-session-${index + 1}`,
    title,
    last_active_ms: Date.now() - lastActive[index],
    total:
      Math.round(baseSessions[index] * SESSION_SCALE[range]) +
      (index === 0 ? delta : 0),
    active: index === 0,
  }));

  const now = Date.now();
  const windowPulses = pulses
    .filter((pulse) => pulse.t_ms >= now - 3_600_000)
    .map((pulse) => ({ ...pulse }));

  return {
    range,
    totals,
    pulses: windowPulses,
    models,
    profiles: buildProfiles(range, totals),
    sessions,
  };
}

function emitPulse(): void {
  const total = 8_000 + Math.floor(Math.random() * 52_000);
  const pulse = splitPulse(total, Date.now());
  pulses.push(pulse);
  const cutoff = Date.now() - 3_600_000;
  pulses = pulses.filter((item) => item.t_ms >= cutoff);
  liveDelta.input += pulse.input;
  liveDelta.output += pulse.output;
  liveDelta.cache_read += pulse.cache_read;
  liveDelta.cache_creation += pulse.cache_creation;
  pulseCount += 1;
  for (const cb of pulseSubs) cb(pulse);
  if (pulseCount % 3 === 0) {
    for (const cb of dirtySubs) cb();
  }
}

function schedulePulse(delay = 6500 + Math.random() * 3000): void {
  if (pulseTimer) clearTimeout(pulseTimer);
  if (!PULSES_ENABLED || !visible) return;
  pulseTimer = setTimeout(() => {
    emitPulse();
    schedulePulse();
  }, delay);
}

function tickScan(): void {
  if (scan.current.phase !== "scanning") return;
  const done = Math.min(
    scan.current.files_total,
    scan.current.files_done + 14 + Math.floor(Math.random() * 10),
  );
  if (done >= scan.current.files_total) {
    scan.current = {
      phase: "done",
      files_done: scan.current.files_total,
      files_total: scan.current.files_total,
      percent: 100,
      ...INITIAL_HEALTH,
    };
  } else {
    scan.current = {
      phase: "scanning",
      files_done: done,
      files_total: scan.current.files_total,
      percent: Math.round((done / scan.current.files_total) * 1000) / 10,
      ...INITIAL_HEALTH,
    };
  }
  for (const cb of scanSubs) cb({ ...scan.current });
  if (scan.current.phase === "scanning") {
    setTimeout(tickScan, 110 + Math.random() * 70);
  } else {
    for (const cb of dirtySubs) cb();
  }
}

let healthEmitted = false;

export function createMockAdapter(): UiAdapter {
  if (!EMPTY_SCAN) {
    setTimeout(tickScan, 400);
  }
  schedulePulse(1800);

  return {
    async getDashboard(range) {
      if (remainingFailures > 0) {
        remainingFailures -= 1;
        throw new Error("mock: get_dashboard failed");
      }
      return buildDashboard(range);
    },

    async getScanStatus() {
      return { ...scan.current };
    },

    async updateProfileDisplay(profileId, displayName) {
      displayNames.set(profileId, displayName);
      for (const cb of dirtySubs) cb();
    },

    async updateProfileQuota(profileId, quota) {
      quotaOverrides.set(profileId, { ...quota });
      for (const cb of dirtySubs) cb();
    },

    async reassignUsage() {
      // mock 无归因时间线，保留接口形状
    },

    onPulse(cb) {
      pulseSubs.add(cb);
      return () => pulseSubs.delete(cb);
    },

    onScanProgress(cb) {
      scanSubs.add(cb);
      return () => scanSubs.delete(cb);
    },

    onDataHealth(cb) {
      healthSubs.add(cb);
      if (!healthEmitted) {
        healthEmitted = true;
        queueMicrotask(() => cb({ ...INITIAL_HEALTH }));
      }
      return () => healthSubs.delete(cb);
    },

    onDashboardDirty(cb) {
      dirtySubs.add(cb);
      return () => dirtySubs.delete(cb);
    },

    setVisible(next) {
      visible = next;
      if (next) schedulePulse(900);
      else if (pulseTimer) clearTimeout(pulseTimer);
    },
  };
}
