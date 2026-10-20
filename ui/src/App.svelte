<script lang="ts">
  // T6 —— 悬浮窗主组件：启动状态机（首扫/空态/就绪）+ 五区块布局 + 水合/防抖。
  import { onMount } from "svelte";
  import {
    createAdapter,
    type Dashboard,
    type DataHealth,
    type Range,
    type ScanStatus,
    type UiAdapter,
    type UsagePulse,
  } from "./data";
  import { t, type MessageKey } from "./i18n";
  import { formatCompact, formatCost, formatCostPrecise } from "./format";
  import Digits from "./components/Digits.svelte";
  import RangeChips from "./components/RangeChips.svelte";
  import Chart from "./components/Chart.svelte";
  import ModelsBar from "./components/ModelsBar.svelte";
  import ProfileCards from "./components/ProfileCards.svelte";
  import SessionList from "./components/SessionList.svelte";
  import ScanProgress from "./components/ScanProgress.svelte";
  import ErrorBanners from "./components/ErrorBanners.svelte";
  import ProxyBar from "./components/ProxyBar.svelte";

  const adapter: UiAdapter = createAdapter();

  const TOTAL_LABEL: Record<Range, MessageKey> = {
    today: "total.today",
    week: "total.week",
    month: "total.month",
    all: "total.all",
  };

  let phase = $state<"scanning" | "empty" | "ready">("scanning");
  let scan = $state<ScanStatus>({
    phase: "scanning",
    files_done: 0,
    files_total: 0,
    percent: 0,
  });
  let dashboard = $state<Dashboard | null>(null);
  let range = $state<Range>("today");
  let pulses = $state<UsagePulse[]>([]);
  let health = $state<DataHealth>({
    root_unreadable: false,
    skipped_files: 0,
    db_readonly: false,
  });
  let fetchFailed = $state(false);

  let loadSeq = 0;
  let dirtyTimer: ReturnType<typeof setTimeout> | undefined;

  let totalLabel = $derived(t(TOTAL_LABEL[range]));

  function mergePulses(incoming: UsagePulse[]): void {
    const byTime = new Map<number, UsagePulse>();
    for (const pulse of [...pulses, ...incoming]) byTime.set(pulse.t_ms, pulse);
    const cutoff = Date.now() - 3_600_000;
    pulses = [...byTime.values()]
      .filter((pulse) => pulse.t_ms >= cutoff)
      .sort((a, b) => a.t_ms - b.t_ms);
  }

  async function loadNow(): Promise<void> {
    const seq = ++loadSeq;
    const requested = range;
    try {
      const next = await adapter.getDashboard(requested);
      if (seq !== loadSeq) return; // 期间发起了新请求（chips 切换等），丢弃旧响应
      fetchFailed = false;
      dashboard = next;
      mergePulses(next.pulses);
      if (phase !== "ready") phase = "ready";
    } catch (error) {
      console.error("get_dashboard failed", error);
      fetchFailed = true;
    }
  }

  let hasPendingLoad = false;

  function scheduleLoad(): void {
    if (typeof document !== "undefined" && document.hidden) {
      hasPendingLoad = true;
      return;
    }
    if (dirtyTimer) clearTimeout(dirtyTimer);
    dirtyTimer = setTimeout(() => {
      dirtyTimer = undefined;
      void loadNow();
    }, 500);
  }

  function setRange(next: Range): void {
    if (next === range) return;
    range = next;
    if (dirtyTimer) {
      clearTimeout(dirtyTimer);
      dirtyTimer = undefined;
    }
    void loadNow();
  }

  onMount(() => {
    const offPulse = adapter.onPulse((pulse) => mergePulses([pulse]));
    const offDirty = adapter.onDashboardDirty(scheduleLoad);
    const offPricing = adapter.onPricingUpdated?.(scheduleLoad);
    const offScan = adapter.onScanProgress((status) => {
      scan = status;
      if (status.phase === "scanning") {
        if (phase !== "ready") phase = "scanning";
      } else if (status.phase === "empty") {
        phase = "empty";
      } else {
        void loadNow();
      }
    });
    const offHealth = adapter.onDataHealth((next) => {
      health = next;
    });

    const onFocusOrVisible = () => {
      const visible = !document.hidden;
      adapter.setVisible(visible);
      if (visible) {
        hasPendingLoad = false;
        void loadNow(); // 重新显示或聚焦：全量水合一次，再续接增量推送
      }
    };
    document.addEventListener("visibilitychange", onFocusOrVisible);
    window.addEventListener("focus", onFocusOrVisible);

    void (async () => {
      try {
        const status = await adapter.getScanStatus();
        scan = status;
        health = {
          root_unreadable: status.root_unreadable,
          skipped_files: status.skipped_files,
          db_readonly: status.db_readonly,
        };
        if (status.phase === "empty") {
          phase = "empty";
        } else if (status.phase === "done") {
          await loadNow();
        } else if (phase !== "ready") {
          phase = "scanning";
        }
      } catch (error) {
        console.error("get_scan_status failed", error);
        fetchFailed = true;
        await loadNow();
      }
    })();

    return () => {
      offPulse();
      offDirty();
      offPricing?.();
      offScan();
      offHealth();
      document.removeEventListener("visibilitychange", onFocusOrVisible);
      window.removeEventListener("focus", onFocusOrVisible);
      if (dirtyTimer) clearTimeout(dirtyTimer);
    };
  });

  async function handleUpdateProfile(
    profileId: number,
    displayName: string,
    quota: {
      total: number | null;
      used: number | null;
      reset_date: string | null;
    },
  ) {
    await adapter.updateProfileDisplay(profileId, displayName);
    await adapter.updateProfileQuota(profileId, quota);
  }
</script>

<div class="app">
  <ErrorBanners {health} unavailable={fetchFailed} />

  {#if phase === "ready" && dashboard}
    <section class="totals">
      <div class="label-row">
        <span class="label">{totalLabel}</span>
        <div class="header-tags">
          {#if dashboard.cost}
            <span
              class="cost-badge"
              title={`${t("cost.estimateTooltip")} · ${formatCostPrecise(dashboard.cost.total)}`}
            >
              <span class="cost-pattern hatch" aria-hidden="true"></span>
              <span class="cost-label">{t("cost.estimated")}</span>
              <span class="cost-value">{formatCost(dashboard.cost.total)}</span>
              {#if !dashboard.cost.is_complete}
                <span
                  class="unpriced-mark"
                  title={t("cost.unpricedNote", {
                    n: formatCompact(dashboard.cost.unpriced_tokens),
                  })}>*</span
                >
              {/if}
            </span>
          {/if}
          {#if health.skipped_files > 0}
            <span class="badge"
              >{t("health.skipped", { n: health.skipped_files })}</span
            >
          {/if}
        </div>
      </div>
      <div class="big"><Digits value={dashboard.totals.total} /></div>
      <div class="breakdown">
        <span
          >{t("breakdown.input")}
          {formatCompact(dashboard.totals.input)}</span
        >
        <span
          >{t("breakdown.output")}
          {formatCompact(dashboard.totals.output)}</span
        >
        <span
          >{t("breakdown.cacheRead")}
          {formatCompact(dashboard.totals.cache_read)}</span
        >
        <span
          >{t("breakdown.cacheWrite")}
          {formatCompact(dashboard.totals.cache_creation)}</span
        >
      </div>
    </section>

    <RangeChips value={range} onchange={setRange} />
    <Chart {pulses} />
    <ModelsBar models={dashboard.models} />
    <ProxyBar {adapter} />
    <ProfileCards
      profiles={dashboard.profiles}
      onUpdateProfile={handleUpdateProfile}
    />
    <SessionList sessions={dashboard.sessions} />
  {:else}
    <ScanProgress status={scan} />
  {/if}
</div>

<style>
  .app {
    min-height: 100%;
    box-sizing: border-box;
    padding: 16px 18px 22px;
  }
  .totals {
    padding: 2px 0 12px;
  }
  .label-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    min-height: 16px;
  }
  .label {
    font-size: 11px;
    color: var(--text-2);
    letter-spacing: 0.03em;
  }
  .header-tags {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .cost-badge {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 6px;
    font-family: var(--font-num);
    font-size: 10px;
    color: var(--text-2);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    overflow: hidden;
    cursor: default;
  }
  .cost-pattern {
    position: absolute;
    inset: 0;
    opacity: 0.18;
    pointer-events: none;
  }
  .cost-label {
    font-size: 9px;
    opacity: 0.8;
    position: relative;
    z-index: 1;
  }
  .cost-value {
    font-weight: 500;
    color: var(--text-1);
    font-variant-numeric: tabular-nums;
    position: relative;
    z-index: 1;
  }
  .unpriced-mark {
    color: var(--accent);
    font-weight: bold;
    position: relative;
    z-index: 1;
  }
  .badge {
    font-family: var(--font-num);
    font-size: 9.5px;
    color: var(--text-2);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    padding: 1px 5px;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .big {
    font-size: 40px;
    line-height: 1.15;
    margin: 4px 0 7px;
    letter-spacing: -0.01em;
  }
  .breakdown {
    display: flex;
    flex-wrap: wrap;
    gap: 3px 10px;
    font-family: var(--font-num);
    font-size: 9.5px;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
</style>
