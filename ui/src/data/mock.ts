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
  ProxyStatus,
  SessionFlightRecorder,
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
  [1, { quota_type: "api", unit: "usd", total: 50.0, used: 12.35, reset_date: plusDays(14), updated_at: Date.now(), sync_status: "ok", sync_error: null }],
  [2, { quota_type: "manual", unit: "tokens", total: 3_000_000, used: 1_240_000, reset_date: null, updated_at: null, sync_status: null, sync_error: null }],
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
      cost: {
        total: Math.round(tokens.total * 0.000005 * 100) / 100,
        coverage: 1.0,
        is_complete: true,
        unpriced_tokens: 0,
      },
    };
  });

  const unknownTotal = Math.max(1, totals.total - used);
  const unknownTokens = splitTotal(unknownTotal);
  known.push({
    profile_id: null,
    display_name: "Unknown",
    endpoint: null,
    inferred: true,
    tokens: unknownTokens,
    quota: null,
    cost: {
      total: Math.round(unknownTokens.total * 0.000004 * 100) / 100,
      coverage: 1.0,
      is_complete: true,
      unpriced_tokens: 0,
    },
  });

  return known.sort((a, b) => b.tokens.total - a.tokens.total);
}

function buildDashboard(range: Range): Dashboard {
  const totals = buildTotals(range);
  const delta = liveTotal();

  const models = BASE_MODELS[range].map((row, index) => {
    const tokens = row.tokens + (index === 0 ? delta : 0);
    const unitRate = index === 0 ? 0.000015 : index === 1 ? 0.000003 : 0.0000008;
    return {
      ...row,
      tokens,
      cost: Math.round(tokens * unitRate * 100) / 100,
    };
  });

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
    engine: index === 1 ? "codex" : index === 2 ? "cline" : index === 3 ? "aider" : "claude",
  }));
  const now = Date.now();
  const windowPulses = pulses
    .filter((pulse) => pulse.t_ms >= now - 3_600_000)
    .map((pulse) => ({ ...pulse }));

  const totalCost = Math.round(totals.total * 0.0000052 * 100) / 100;
  return {
    range,
    totals,
    cost: {
      total: totalCost,
      coverage: 1.0,
      is_complete: true,
      unpriced_tokens: 0,
    },
    pulses: windowPulses,
    models,
    profiles: buildProfiles(range, totals),
    sessions,
  };
}

function buildMockFlightRecorder(sessionId: string, limit?: number): SessionFlightRecorder {
  const now = Date.now();
  const startTime = now - 3600 * 1000 * 1.5;
  const totalTurns = 28;
  const turns = [];
  const pulseStrip = [];

  let inputSum = 0;
  let outputSum = 0;
  let cacheReadSum = 0;
  let cacheCreationSum = 0;
  let costSum = 0;

  const models = ["claude-3-7-sonnet-20250219", "claude-3-5-haiku-20241022"];
  let prevMainModel: string | null = null;

  for (let i = 1; i <= totalTurns; i++) {
    const isSidechain = i === 8 || i === 9;
    const model = isSidechain ? models[1] : (i > 20 ? models[1] : models[0]);
    const tMs = startTime + (i * 180 * 1000);

    const isSpike1 = i === 14;
    const isSpike2 = i === 4;
    const isSpike = isSpike1 || isSpike2;

    const input = isSpike1 ? 38500 : (isSidechain ? 400 : Math.round(800 + (i * 37) % 600));
    const output = Math.round(300 + (i * 41) % 500);
    const cacheCreation = isSpike2 ? 24000 : 0;
    const cacheRead = isSpike1 ? 6000 : Math.round(3500 + (i * 53) % 2000);
    const total = input + output + cacheRead + cacheCreation;

    let spikeReason: string | null = null;
    if (isSpike1) spikeReason = "未缓存输入主导";
    else if (isSpike2) spikeReason = "缓存写入/上下文填充";

    const modelSwitched = !isSidechain && prevMainModel !== null && prevMainModel !== model;
    if (!isSidechain) prevMainModel = model;

    inputSum += input;
    outputSum += output;
    cacheReadSum += cacheRead;
    cacheCreationSum += cacheCreation;

    const turnCost = model.includes("sonnet")
      ? (input * 0.000003 + output * 0.000015 + cacheRead * 0.0000003 + cacheCreation * 0.00000375)
      : (input * 0.0000008 + output * 0.000004 + cacheRead * 0.00000008);
    costSum += turnCost;

    pulseStrip.push({
      index: i,
      timestamp_ms: tMs,
      total_tokens: total,
      cache_read_tokens: cacheRead,
      is_sidechain: isSidechain,
      is_spike: isSpike,
    });

    turns.push({
      request_index: i,
      record_id: `rec-${sessionId}-${i}`,
      timestamp_ms: tMs,
      input_tokens: input,
      output_tokens: output,
      cache_read_tokens: cacheRead,
      cache_creation_tokens: cacheCreation,
      total_tokens: total,
      model,
      entrypoint: "cli",
      is_sidechain: isSidechain,
      profile_id: 1,
      attribution_source: "proxy",
      cost: {
        total: Math.round(turnCost * 1000) / 1000,
        coverage: 1.0,
        is_complete: true,
        unpriced_tokens: 0,
      },
      model_switched: modelSwitched,
      profile_switched: false,
      is_spike: isSpike,
      spike_reason: spikeReason,
    });
  }

  const totalTokens = inputSum + outputSum + cacheReadSum + cacheCreationSum;
  const maxEvents = limit ?? 200;
  const eventsTruncated = turns.length > maxEvents;
  const events = eventsTruncated ? turns.slice(-maxEvents) : turns;

  return {
    session_id: sessionId,
    title: sessionId.replace("mock-session-", "project-"),
    cwd: "/Volumes/Data/projects/token-watcher",
    cc_version: "1.0.8",
    engine: sessionId.includes("codex") || sessionId.includes("2") ? "codex" : "claude",
    start_time_ms: startTime,
    duration_seconds: Math.round((now - 30_000 - startTime) / 1000),
    is_active: sessionId === "mock-session-1",
    total_requests: totalTurns,
    totals: {
      input: inputSum,
      output: outputSum,
      cache_read: cacheReadSum,
      cache_creation: cacheCreationSum,
      total: totalTokens,
    },
    cost: {
      total: Math.round(costSum * 100) / 100,
      coverage: 1.0,
      is_complete: true,
      unpriced_tokens: 0,
    },
    cache_hit_ratio: (inputSum + cacheReadSum) > 0 ? cacheReadSum / (inputSum + cacheReadSum) : 0,
    models_used: [
      {
        model: "claude-3-7-sonnet-20250219",
        tokens: {
          input: Math.round(inputSum * 0.75),
          output: Math.round(outputSum * 0.75),
          cache_read: Math.round(cacheReadSum * 0.75),
          cache_creation: cacheCreationSum,
          total: Math.round(totalTokens * 0.78),
        },
        cost: {
          total: Math.round(costSum * 0.85 * 100) / 100,
          coverage: 1.0,
          is_complete: true,
          unpriced_tokens: 0,
        },
        share: 0.78,
      },
      {
        model: "claude-3-5-haiku-20241022",
        tokens: {
          input: Math.round(inputSum * 0.25),
          output: Math.round(outputSum * 0.25),
          cache_read: Math.round(cacheReadSum * 0.25),
          cache_creation: 0,
          total: Math.round(totalTokens * 0.22),
        },
        cost: {
          total: Math.round(costSum * 0.15 * 100) / 100,
          coverage: 1.0,
          is_complete: true,
          unpriced_tokens: 0,
        },
        share: 0.22,
      },
    ],
    profiles_used: [
      {
        profile_id: 1,
        display_name: "Anthropic Direct (proxy)",
        tokens: {
          input: inputSum,
          output: outputSum,
          cache_read: cacheReadSum,
          cache_creation: cacheCreationSum,
          total: totalTokens,
        },
        cost: {
          total: Math.round(costSum * 100) / 100,
          coverage: 1.0,
          is_complete: true,
          unpriced_tokens: 0,
        },
      },
    ],
    pulse_strip: pulseStrip,
    events,
    events_truncated: eventsTruncated,
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
let currentTheme = "meter-face";
try {
  const saved = typeof localStorage !== "undefined" ? localStorage.getItem("token_watcher_theme") : null;
  if (saved === "slate" || saved === "mint" || saved === "meter-face") {
    currentTheme = saved;
  }
} catch {}
const themeSubs = new Set<(theme: string) => void>();


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
      quotaOverrides.set(profileId, { ...quota, quota_type: "manual", unit: "tokens" });
      for (const cb of dirtySubs) cb();
    },

    async updateProfileQuotaConfig(profileId, config) {
      const isManual = config.quota_type === "manual";
      quotaOverrides.set(profileId, {
        quota_type: config.quota_type as 'manual' | 'api',
        unit: config.unit as 'usd' | 'tokens' | 'cny',
        total: isManual ? (config.manual_total ?? null) : 50.0,
        used: isManual ? (config.manual_used ?? null) : 12.35,
        reset_date: config.reset_date ?? null,
        updated_at: Date.now(),
        sync_status: isManual ? null : "ok",
        sync_error: null,
      });
      for (const cb of dirtySubs) cb();
    },

    async syncProfileQuota(profileId) {
      const current = quotaOverrides.get(profileId) ?? defaultQuotas.get(profileId);
      const updated: Quota = {
        quota_type: "api",
        unit: current?.unit ?? "usd",
        total: current?.total ?? 50.0,
        used: (current?.used ?? 10.0) + 1.25,
        reset_date: current?.reset_date ?? plusDays(10),
        updated_at: Date.now(),
        sync_status: "ok",
        sync_error: null,
      };
      quotaOverrides.set(profileId, updated);
      for (const cb of dirtySubs) cb();
      return updated;
    },
    async reassignUsage() {
      // mock 无归因时间线，保留接口形状
    },
    async getProxyStatus() {
      return {
        running: true,
        port: 4040,
        upstream: "https://api.anthropic.com",
        is_injected: false,
        export_cmd: "export ANTHROPIC_BASE_URL=http://127.0.0.1:4040",
      };
    },

    async injectProxySettings() {
      return {
        running: true,
        port: 4040,
        upstream: "https://api.anthropic.com",
        is_injected: true,
        export_cmd: "export ANTHROPIC_BASE_URL=http://127.0.0.1:4040",
      };
    },

    async restoreProxySettings() {
      return {
        running: true,
        port: 4040,
        upstream: "https://api.anthropic.com",
        is_injected: false,
        export_cmd: "export ANTHROPIC_BASE_URL=http://127.0.0.1:4040",
      };
    },
    async getHudSummary() {
      return {
        total_today: 359420 + liveTotal(),
        rate_per_min: 3600,
        current_rate: 68.5,
        session_total: 48500,
        current_model: "claude-sonnet",
        cache_rate: 88.2,
        input_tokens: 12400,
        output_tokens: 3200,
        hud_metrics: ["current_rate", "session_total", "cache_rate"],
        hud_locked: false,
      };
    },
    async setHudVisible() {},
    async setHudLocked() {},
    async saveHudPosition() {},
    async showMainWindow() {},
    async moveHudBy() {},
    async saveCurrentHudPosition() {},
    async getTheme() {
      return currentTheme;
    },
    async setTheme(theme: string) {
      currentTheme = theme;
      try {
        if (typeof localStorage !== "undefined") {
          localStorage.setItem("token_watcher_theme", theme);
        }
      } catch {}
      for (const cb of themeSubs) cb(theme);
    },
    onThemeChanged(cb) {
      themeSubs.add(cb);
      return () => themeSubs.delete(cb);
    },
    async getSessionFlightRecorder(sessionId: string, limit?: number) {
      return buildMockFlightRecorder(sessionId, limit);
    },
    onSessionFlightUpdated() {
      return () => {};
    },
    async showHudContextMenu() {},
    async toggleHudMetric(metric: string) {
      return ["current_rate", "session_total", "cache_rate"];
    },
    async resizeHud() {},
    onHudUpdated(cb) {
      return () => {};
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
    onPricingUpdated() {
      return () => {};
    },

    setVisible(next) {
      visible = next;
      if (next) schedulePulse(900);
      else if (pulseTimer) clearTimeout(pulseTimer);
    },
  };
}
