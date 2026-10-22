<script lang="ts">
  import { onMount } from "svelte";
  import {
    createAdapter,
    type Dashboard,
    type HudMode,
    type HudSummary,
    type UiAdapter,
    type UsagePulse,
  } from "./data";
  import { formatCompact, formatCost, formatCostPrecise, formatExact } from "./format";
  import { t, type MessageKey } from "./i18n";
  import Digits from "./components/Digits.svelte";
  import Chart from "./components/Chart.svelte";
  import ModelsBar from "./components/ModelsBar.svelte";

  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("data-hud", "true");
    try {
      const saved = localStorage.getItem("token_watcher_theme");
      if (saved === "slate" || saved === "mint" || saved === "meter-face") {
        document.documentElement.setAttribute("data-theme", saved);
      }
    } catch {}
  }

  const adapter: UiAdapter = createAdapter();

  let hudMode = $state<HudMode>("widget");

  let summary = $state<HudSummary>({
    total_today: 0,
    rate_per_min: 0,
    current_rate: 0,
    session_total: 0,
    current_model: "—",
    cache_rate: 0,
    input_tokens: 0,
    output_tokens: 0,
    hud_metrics: ["current_rate", "session_total", "cache_rate"],
    hud_locked: false,
  });

  let dashboard = $state<Dashboard | null>(null);
  let pulses = $state<UsagePulse[]>([]);
  let compactDigits = $state(false);

  let isPulsing = $state(false);
  let pulseTimeout: ReturnType<typeof setTimeout> | undefined;

  let activeMetrics = $derived(
    summary.hud_metrics && summary.hud_metrics.length >= 2
      ? summary.hud_metrics
      : ["current_rate", "session_total", "cache_rate"]
  );

  let rateText = $derived.by(() => {
    if (summary.current_rate <= 0) return "—";
    return `${Math.round(summary.current_rate)}/s`;
  });

  let sessionTotalText = $derived(
    formatCompact(summary.session_total > 0 ? summary.session_total : summary.total_today)
  );

  function cleanModelName(raw?: string): string {
    if (!raw || raw === "—") return "—";
    if (raw.includes("sonnet-3-7") || raw.includes("3.7-sonnet")) return "Sonnet 3.7";
    if (raw.includes("sonnet-3-5") || raw.includes("3.5-sonnet")) return "Sonnet 3.5";
    if (raw.includes("sonnet")) return "Sonnet";
    if (raw.includes("opus")) return "Opus";
    if (raw.includes("haiku")) return "Haiku";
    if (raw.includes("gpt-4o-mini")) return "4o-mini";
    if (raw.includes("gpt-4o")) return "GPT-4o";
    if (raw.includes("o1")) return "o1";
    if (raw.includes("o3")) return "o3";
    if (raw.includes("deepseek")) return "DeepSeek";
    if (raw.includes("glm")) return "GLM";
    return raw.split("-")[0];
  }

  let modelText = $derived(cleanModelName(summary.current_model));

  let activeSessions = $derived(
    summary.active_sessions && summary.active_sessions.length > 0
      ? summary.active_sessions.slice(0, summary.hud_active_sessions_count ?? 3)
      : []
  );
  let cacheRateText = $derived(`${Math.round(summary.cache_rate)}%`);
  let inputText = $derived(
    formatCompact(
      summary.input_tokens > 0
        ? summary.input_tokens
        : dashboard
          ? dashboard.totals.input
          : 0
    )
  );
  let outputText = $derived(
    formatCompact(
      summary.output_tokens > 0
        ? summary.output_tokens
        : dashboard
          ? dashboard.totals.output
          : 0
    )
  );

  const THEME_NAMES: Record<string, MessageKey> = {
    "meter-face": "theme.meter",
    slate: "theme.slate",
    mint: "theme.mint",
  };

  const THEME_CYCLE: Record<string, string> = {
    "meter-face": "slate",
    slate: "mint",
    mint: "meter-face",
  };

  let theme = $state<string>("meter-face");
  let themeLabel = $derived(t(THEME_NAMES[theme] ?? "theme.meter"));

  async function cycleTheme(e?: MouseEvent) {
    if (e) e.stopPropagation();
    const prev = theme;
    const next = THEME_CYCLE[prev] ?? "meter-face";
    theme = next;
    document.documentElement.setAttribute("data-theme", next);
    try {
      localStorage.setItem("token_watcher_theme", next);
    } catch {}
    try {
      await adapter.setTheme(next);
    } catch (err) {
      console.warn("setTheme failed", err);
    }
  }

  async function toggleHudMode(e?: MouseEvent) {
    if (e) e.stopPropagation();
    const nextMode: HudMode = hudMode === "widget" ? "capsule" : "widget";
    hudMode = nextMode;
    try {
      await adapter.setHudMode(nextMode);
    } catch (err) {
      console.warn("setHudMode failed", err);
    }
  }

  $effect(() => {
    if (hudMode === "capsule") {
      const count = activeMetrics.length;
      const sessionsCount = activeSessions.length;
      const targetWidth = sessionsCount > 1
        ? Math.max(240, Math.min(520, 180 + (count - 2) * 70))
        : Math.max(180, Math.min(520, 180 + (count - 2) * 70));
      const targetHeight = sessionsCount > 1 ? 34 + (sessionsCount - 1) * 28 : 34;
      void adapter.resizeHud(targetWidth, targetHeight);
    } else {
      const sessionsCount = activeSessions.length;
      const targetHeight = sessionsCount > 1 ? 390 + 36 + sessionsCount * 24 : 390;
      void adapter.resizeHud(320, targetHeight);
    }
  });

  async function loadSummary(): Promise<void> {
    try {
      const data = await adapter.getHudSummary();
      summary = data;
    } catch (e) {
      console.error("Failed to load HUD summary", e);
    }
  }

  async function loadDashboard(): Promise<void> {
    try {
      const data = await adapter.getDashboard("today", null);
      dashboard = data;
      pulses = data.pulses ?? [];
    } catch (e) {
      console.error("Failed to load HUD dashboard", e);
    }
  }

  function triggerPulseAnimation(): void {
    isPulsing = true;
    if (pulseTimeout) clearTimeout(pulseTimeout);
    pulseTimeout = setTimeout(() => {
      isPulsing = false;
    }, 400);
  }

  // 拖拽手柄及整体平移处理
  let lastScreenX = 0;
  let lastScreenY = 0;
  let isPointerDown = false;
  let hasDragged = false;
  let pointerDownTime = 0;
  let downDist = 0;

  function handlePointerDown(e: PointerEvent): void {
    if (summary.hud_locked || e.button !== 0) return;
    // 如果点击在交互元素上（button, input 等），不触发拖拽
    const target = e.target as HTMLElement | null;
    if (target && target.closest("button, a, input, select, textarea")) {
      return;
    }
    isPointerDown = true;
    hasDragged = false;
    downDist = 0;
    pointerDownTime = Date.now();
    lastScreenX = e.screenX;
    lastScreenY = e.screenY;
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {}
  }

  function handlePointerMove(e: PointerEvent): void {
    if (!isPointerDown || summary.hud_locked) return;
    const dx = e.screenX - lastScreenX;
    const dy = e.screenY - lastScreenY;
    lastScreenX = e.screenX;
    lastScreenY = e.screenY;
    downDist += Math.hypot(dx, dy);

    if (downDist > 3) {
      hasDragged = true;
      void adapter.moveHudBy(dx, dy);
    }
  }

  async function handlePointerUp(e: PointerEvent): Promise<void> {
    if (!isPointerDown) return;
    isPointerDown = false;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {}

    if (hasDragged) {
      void adapter.saveCurrentHudPosition();
      return;
    }

    if (summary.hud_locked) return;

    // 如果是胶囊条，单击可打开主窗口；如果是在卡片模式，由专用按钮打开
    if (hudMode === "capsule") {
      const elapsed = Date.now() - pointerDownTime;
      if (elapsed < 400 && downDist <= 3) {
        try {
          await adapter.showMainWindow();
        } catch (err) {
          console.error("showMainWindow failed", err);
        }
      }
    }
  }

  async function handleContextMenu(e: MouseEvent): Promise<void> {
    e.preventDefault();
    try {
      await adapter.showHudContextMenu();
    } catch (err) {
      console.warn("showHudContextMenu failed", err);
    }
  }

  onMount(() => {
    void (async () => {
      try {
        const mode = await adapter.getHudMode();
        if (mode === "widget" || mode === "capsule") {
          hudMode = mode;
        }
      } catch (err) {
        console.warn("getHudMode failed", err);
      }
    })();

    void loadSummary();
    void loadDashboard();

    const offPulse = adapter.onPulse((pulse) => {
      const pulseTotal = pulse.input + pulse.output + pulse.cache_read + pulse.cache_creation;
      summary.total_today += pulseTotal;
      if (dashboard) {
        dashboard.totals.input += pulse.input;
        dashboard.totals.output += pulse.output;
        dashboard.totals.cache_read += pulse.cache_read;
        dashboard.totals.cache_creation += pulse.cache_creation;
        dashboard.totals.total += pulseTotal;
      }
      pulses = [...pulses, pulse];
      triggerPulseAnimation();
      void loadSummary();
      void loadDashboard();
    });

    const offDirty = adapter.onDashboardDirty(() => {
      void loadSummary();
      void loadDashboard();
    });

    const offHudUpdated = adapter.onHudUpdated
      ? adapter.onHudUpdated(() => {
          void loadSummary();
          void loadDashboard();
        })
      : () => {};

    const offHudMode = adapter.onHudModeChanged
      ? adapter.onHudModeChanged((nextMode) => {
          if (nextMode === "widget" || nextMode === "capsule") {
            hudMode = nextMode;
          }
        })
      : () => {};

    const offTheme = adapter.onThemeChanged((nextTheme) => {
      if (nextTheme === "slate" || nextTheme === "mint" || nextTheme === "meter-face") {
        theme = nextTheme;
        document.documentElement.setAttribute("data-theme", nextTheme);
        try {
          localStorage.setItem("token_watcher_theme", nextTheme);
        } catch {}
      }
    });

    void (async () => {
      try {
        const serverTheme = await adapter.getTheme();
        if (serverTheme === "slate" || serverTheme === "mint" || serverTheme === "meter-face") {
          theme = serverTheme;
          document.documentElement.setAttribute("data-theme", serverTheme);
          try {
            localStorage.setItem("token_watcher_theme", serverTheme);
          } catch {}
        }
      } catch (err) {
        console.warn("Failed to get theme in HUD", err);
      }
    })();

    // 定期刷新衰减速率（30 秒一次）
    const decayTimer = setInterval(() => {
      void loadSummary();
    }, 30_000);

    return () => {
      offPulse();
      offDirty();
      offHudUpdated();
      offHudMode();
      offTheme();
      clearInterval(decayTimer);
      if (pulseTimeout) clearTimeout(pulseTimeout);
    };
  });
</script>
<div
  class="hud-app"
  class:is-widget={hudMode === "widget"}
  class:is-capsule={hudMode === "capsule"}
  style="--capsule-opacity: {summary.capsule_opacity ?? 0.9}; --widget-opacity: {summary.widget_opacity ?? 0.95};"
>
{#if hudMode === "widget"}
  <main
    class="hud-card"
    class:pulsing={isPulsing}
    class:locked={summary.hud_locked}
    data-mode="widget"
    data-hud="true"
    onpointerdown={handlePointerDown}
    onpointermove={handlePointerMove}
    onpointerup={handlePointerUp}
    oncontextmenu={handleContextMenu}
  >
    <!-- Header -->
    <header class="card-header">
      <div class="header-left">
        <span class="card-badge">{t("hud.todayLabel")}</span>
        {#if dashboard?.cost}
          <span class="card-cost" title={formatCostPrecise(dashboard.cost.total)}>
            {formatCost(dashboard.cost.total)}
          </span>
        {/if}
      </div>
      <div class="header-right">
        <button
          type="button"
          class="icon-btn"
          onclick={cycleTheme}
          title={t("theme.toggleTooltip", { theme: themeLabel })}
          aria-label={t("theme.toggleTooltip", { theme: themeLabel })}
        >
          <span aria-hidden="true">◓</span>
        </button>
        <button
          type="button"
          class="icon-btn"
          onclick={toggleHudMode}
          title={t("hud.switchToCapsule")}
          aria-label={t("hud.switchToCapsule")}
        >
          <span aria-hidden="true">▫️</span>
        </button>
        <div class="drag-handle" title={t("hud.dragHandle")} aria-hidden="true">
          <span>⋮⋮</span>
        </div>
      </div>
    </header>

    <!-- Totals -->
    <section class="card-totals">
      <div class="card-big">
        <button
          type="button"
          class="digits-btn"
          onclick={(e) => {
            e.stopPropagation();
            compactDigits = !compactDigits;
          }}
          title={compactDigits ? t("digits.toggleToExact") : t("digits.toggleToCompact")}
          aria-label={compactDigits ? t("digits.toggleToExact") : t("digits.toggleToCompact")}
        >
          <Digits
            value={dashboard ? dashboard.totals.total : summary.total_today}
            compact={compactDigits}
          />
          <span class="unit-badge" aria-hidden="true">
            {compactDigits ? t("digits.unitCompact") : t("digits.unitExact")}
          </span>
        </button>
      </div>

      <div class="card-metrics-grid">
        {#each activeMetrics as metric}
          {#if metric === "current_rate"}
            <div class="metric-cell" title={t("hud.tipRate")}>
              <span class="cell-label"><span class="cell-icon" aria-hidden="true">⚡️</span>{t("hud.metricRate")}</span>
              <span class="cell-val rate-val">{rateText}</span>
            </div>
          {:else if metric === "session_total"}
            <div class="metric-cell" title={t("hud.tipSession")}>
              <span class="cell-label">{t("hud.metricSession")}</span>
              <span class="cell-val total-val">{sessionTotalText}</span>
            </div>
          {:else if metric === "current_model"}
            <div class="metric-cell" title={t("hud.tipModel", { model: summary.current_model })}>
              <span class="cell-label"><span class="cell-icon" aria-hidden="true">🧠</span>{t("hud.metricModel")}</span>
              <span class="cell-val model-val">{modelText}</span>
            </div>
          {:else if metric === "cache_rate"}
            <div class="metric-cell" title={t("hud.tipCache")}>
              <span class="cell-label"><span class="cell-icon" aria-hidden="true">💾</span>{t("hud.metricCache")}</span>
              <span class="cell-val cache-val">{cacheRateText}</span>
            </div>
          {:else if metric === "input_tokens"}
            <div class="metric-cell" title={t("hud.tipInput")}>
              <span class="cell-label"><span class="cell-icon" aria-hidden="true">📥</span>{t("hud.metricInput")}</span>
              <span class="cell-val input-val">{inputText}</span>
            </div>
          {:else if metric === "output_tokens"}
            <div class="metric-cell" title={t("hud.tipOutput")}>
              <span class="cell-label"><span class="cell-icon" aria-hidden="true">📤</span>{t("hud.metricOutput")}</span>
              <span class="cell-val output-val">{outputText}</span>
            </div>
          {/if}
        {/each}
      </div>
      {#if activeSessions.length > 1}
        <div class="card-active-sessions">
          <div class="active-sessions-header">
            <span class="active-title">{t("hud.activeSessions")}</span>
            <span class="active-count">{activeSessions.length}</span>
          </div>
          <div class="active-sessions-list">
            {#each activeSessions as sess, idx}
              <div class="active-session-item">
                <span class="sess-idx">#{idx + 1}</span>
                <span class="sess-model" title={sess.model}>{cleanModelName(sess.model)}</span>
                <span class="sess-rate">{sess.current_rate > 0 ? `⚡️${Math.round(sess.current_rate)}/s` : "—"}</span>
                <span class="sess-total">{formatCompact(sess.session_total)}</span>
              </div>
            {/each}
          </div>
        </div>
      {/if}
    </section>

    <!-- Chart -->
    <div class="card-chart">
      <Chart {pulses} />
    </div>

    <!-- Models Top 3 -->
    {#if dashboard && dashboard.models.length > 0}
      <div class="card-models">
        <ModelsBar
          models={dashboard.models}
          limit={3}
          totalCount={dashboard.models.length}
          onMore={() => adapter.showMainWindow()}
        />
      </div>
    {/if}
  </main>
{:else}
  <main
    class="hud-pill"
    class:is-multi={activeSessions.length > 1}
    class:pulsing={isPulsing}
    class:locked={summary.hud_locked}
    data-mode="capsule"
    data-hud="true"
    onpointerdown={handlePointerDown}
    onpointermove={handlePointerMove}
    onpointerup={handlePointerUp}
    oncontextmenu={handleContextMenu}
    title={activeSessions.length > 1 ? `今日: ${formatExact(summary.total_today)} tokens · ${activeSessions.length} 条激活会话` : `会话: ${formatExact(summary.session_total)} | 今日: ${formatExact(summary.total_today)} tokens`}
  >
    {#if activeSessions.length > 1}
      <div class="multi-sessions-list">
        {#each activeSessions as sess, idx}
          <div class="session-row-pill" title={`会话: ${sess.session_id} | 累计: ${formatExact(sess.session_total)} tokens`}>
            <span class="session-index">#{idx + 1}</span>
            <div class="session-metric rate-metric">
              <span class="hud-icon" aria-hidden="true">⚡️</span>
              <span class="hud-rate-val">{sess.current_rate > 0 ? `${Math.round(sess.current_rate)}/s` : "—"}</span>
            </div>
            <div class="session-divider"></div>
            <div class="session-metric total-metric">
              <span class="hud-total-val">{formatCompact(sess.session_total)}</span>
            </div>
            <div class="session-divider"></div>
            <div class="session-metric model-metric">
              <span class="hud-icon" aria-hidden="true">🧠</span>
              <span class="hud-model-val">{cleanModelName(sess.model)}</span>
            </div>
          </div>
        {/each}
      </div>
    {:else}
      {#each activeMetrics as metric, index}
        {#if index > 0}
          <div class="hud-divider"></div>
        {/if}

        {#if metric === "current_rate"}
          <div class="hud-section rate-section" title={t("hud.tipRate")}>
            <span class="hud-icon">⚡️</span>
            <span class="hud-rate-val">{rateText}</span>
          </div>
        {:else if metric === "session_total"}
          <div class="hud-section total-section" title={t("hud.tipSession")}>
            <span class="hud-total-val">{sessionTotalText}</span>
          </div>
        {:else if metric === "current_model"}
          <div class="hud-section model-section" title={t("hud.tipModel", { model: summary.current_model })}>
            <span class="hud-icon">🧠</span>
            <span class="hud-model-val">{modelText}</span>
          </div>
        {:else if metric === "cache_rate"}
          <div class="hud-section cache-section" title={t("hud.tipCache")}>
            <span class="hud-icon">💾</span>
            <span class="hud-cache-val">{cacheRateText}</span>
          </div>
        {:else if metric === "input_tokens"}
          <div class="hud-section input-section" title={t("hud.tipInput")}>
            <span class="hud-icon">📥</span>
            <span class="hud-val">{inputText}</span>
          </div>
        {:else if metric === "output_tokens"}
          <div class="hud-section output-section" title={t("hud.tipOutput")}>
            <span class="hud-icon">📤</span>
            <span class="hud-val">{outputText}</span>
          </div>
        {/if}
      {/each}
    {/if}
  </main>
{/if}
</div>
<style>
  :global(html[data-hud]),
  :global(html[data-hud] body),
  :global(html[data-hud] #app) {
    margin: 0;
    padding: 0;
    width: 100vw;
    height: 100vh;
    overflow: hidden !important;
    background: transparent !important;
    background-color: transparent !important;
    user-select: none;
    -webkit-user-select: none;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace, sans-serif;
  }

  /* ---------------- 极简胶囊条 (Capsule) ---------------- */
  .hud-pill {
    width: 100vw;
    height: 100vh;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 12px;
    border-radius: 9999px;
    background: rgba(22, 24, 29, var(--capsule-opacity, 0.90));
    border: 1px solid rgba(255, 255, 255, 0.12);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.15);
    cursor: grab;
    transition: border-color 0.15s ease, background 0.15s ease;
  }

  .hud-pill:active {
    cursor: grabbing;
  }

  .hud-pill.locked {
    cursor: default;
    border-color: rgba(255, 255, 255, 0.08);
  }

  .hud-pill.pulsing {
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    background: rgba(26, 28, 34, var(--capsule-opacity, 0.90));
  }

  .hud-section {
    display: flex;
    align-items: center;
    pointer-events: none;
  }

  .rate-section,
  .model-section,
  .cache-section,
  .input-section,
  .output-section {
    gap: 3px;
    white-space: nowrap;
  }
  .hud-icon {
    font-size: 11px;
    line-height: 1;
    filter: drop-shadow(0 0 4px color-mix(in srgb, var(--accent) 30%, transparent));
  }

  .hud-rate-val {
    font-size: 11px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: #94a3b8;
    letter-spacing: -0.2px;
  }

  .hud-divider {
    width: 1px;
    height: 12px;
    background: rgba(255, 255, 255, 0.12);
    margin: 0 4px;
    pointer-events: none;
  }

  .total-section {
    justify-content: center;
    white-space: nowrap;
  }

  .hud-total-val {
    font-size: 12px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    color: var(--accent);
    letter-spacing: -0.3px;
  }

  .hud-val,
  .hud-cache-val,
  .hud-model-val {
    font-size: 11px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: #cbd5e1;
    letter-spacing: -0.2px;
  }

  .hud-cache-val {
    color: #6ee7b7;
  }

  .hud-model-val {
    color: #93c5fd;
  }

  /* 多行垂直平铺胶囊样式 */
  .hud-pill.is-multi {
    height: 100vh;
    padding: 3px 6px;
    border-radius: 12px;
    flex-direction: column;
    justify-content: center;
    gap: 3px;
  }

  .multi-sessions-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    box-sizing: border-box;
  }

  .session-row-pill {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    box-sizing: border-box;
    font-size: 11px;
    gap: 6px;
    padding: 2px 6px;
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.04);
  }

  .session-index {
    font-size: 9.5px;
    font-weight: 700;
    color: var(--accent);
    opacity: 0.9;
    font-family: var(--font-num);
    width: 14px;
    flex-shrink: 0;
  }

  .session-metric {
    display: flex;
    align-items: center;
    gap: 3px;
    white-space: nowrap;
  }

  .session-metric.model-metric {
    flex: 1;
    justify-content: flex-end;
    overflow: hidden;
  }

  .session-metric.model-metric .hud-model-val {
    max-width: 75px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .session-divider {
    width: 1px;
    height: 10px;
    background: rgba(255, 255, 255, 0.12);
    margin: 0 1px;
    flex-shrink: 0;
  }

  /* 小组件活跃会话微卡片 */
  .card-active-sessions {
    margin-top: 6px;
    padding: 6px 8px;
    background: var(--bg-1);
    border: 1px solid var(--line);
    border-radius: var(--radius);
  }
  .active-sessions-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 10px;
    color: var(--text-2);
    margin-bottom: 4px;
  }
  .active-count {
    font-family: var(--font-num);
    font-size: 9.5px;
    background: var(--bg-2);
    padding: 1px 4px;
    border-radius: 2px;
    color: var(--accent);
    font-weight: 600;
  }
  .active-sessions-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .active-session-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 10.5px;
    gap: 6px;
  }
  .sess-idx {
    font-size: 9px;
    font-weight: 700;
    color: var(--accent);
    width: 14px;
    flex-shrink: 0;
  }
  .sess-model {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-1);
    font-size: 10px;
  }
  .sess-rate {
    font-family: var(--font-num);
    color: #94a3b8;
    font-size: 10px;
  }
  .sess-total {
    font-family: var(--font-num);
    font-weight: 600;
    color: var(--text-2);
    font-size: 10px;
  }

  /* ---------------- 卡片小组件 (Widget) ---------------- */
  .hud-card {
    width: 100vw;
    height: 100vh;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    padding: 12px 14px 14px;
    border-radius: 12px;
    background: rgba(24, 26, 32, var(--widget-opacity, 0.95));
    border: 1px solid var(--line-strong);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08);
    cursor: default;
    overflow-y: auto;
    overflow-x: hidden;
    scrollbar-width: none;
    transition: border-color 0.15s ease, background 0.15s ease;
  }

  .hud-card::-webkit-scrollbar {
    display: none;
  }

  .hud-card.pulsing {
    border-color: color-mix(in srgb, var(--accent) 50%, var(--line-strong));
  }

  .hud-card.locked {
    border-color: var(--line);
  }

  .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .card-badge {
    font-size: 10px;
    font-weight: 600;
    padding: 1px 6px;
    background: var(--bg-2);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    color: var(--text-2);
    letter-spacing: 0.05em;
  }

  .card-cost {
    font-family: var(--font-num);
    font-size: 11px;
    font-weight: 500;
    color: var(--accent);
    font-variant-numeric: tabular-nums;
  }

  .header-right {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius);
    color: var(--text-2);
    cursor: pointer;
    font-size: 12px;
    transition: background 0.12s ease, color 0.12s ease;
  }

  .icon-btn:hover {
    background: var(--bg-1);
    color: var(--text-1);
  }

  .drag-handle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 22px;
    color: var(--text-2);
    cursor: grab;
    font-size: 11px;
    letter-spacing: 1px;
    opacity: 0.7;
    transition: opacity 0.12s ease;
  }

  .drag-handle:hover {
    opacity: 1;
    color: var(--text-1);
  }

  .drag-handle:active {
    cursor: grabbing;
  }

  .card-totals {
    margin-bottom: 10px;
  }

  .card-big {
    margin-bottom: 6px;
  }

  .digits-btn {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    text-align: left;
  }

  .unit-badge {
    font-family: var(--font-num);
    font-size: 10px;
    color: var(--text-2);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .card-metrics-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px 8px;
    margin-top: 4px;
  }

  .metric-cell {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 2px 6px;
    background: var(--bg-1);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    font-size: 10.5px;
    min-height: 20px;
    box-sizing: border-box;
  }

  .metric-cell:last-child:nth-child(odd) {
    grid-column: 1 / -1;
  }

  .cell-label {
    display: flex;
    align-items: center;
    gap: 3px;
    color: var(--text-2);
    font-size: 10px;
    white-space: nowrap;
  }

  .cell-icon {
    font-size: 10px;
    line-height: 1;
  }

  .cell-val {
    font-family: var(--font-num);
    font-size: 11px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--text-1);
    white-space: nowrap;
  }

  .cell-val.rate-val {
    color: #94a3b8;
  }

  .cell-val.total-val {
    color: var(--accent);
  }

  .cell-val.model-val {
    color: #93c5fd;
    max-width: 75px;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .cell-val.cache-val {
    color: #6ee7b7;
  }

  .cell-val.input-val {
    color: #cbd5e1;
  }

  .cell-val.output-val {
    color: #fca5a5;
  }

  .card-chart {
    border-top: 1px solid var(--line);
    padding-top: 8px;
    margin-bottom: 10px;
  }

  .card-chart :global(.section) {
    border-top: none;
    padding: 0;
  }

  .card-models {
    border-top: 1px solid var(--line);
    padding-top: 8px;
  }

  .card-models :global(.section) {
    border-top: none;
    padding: 0;
  }
</style>
