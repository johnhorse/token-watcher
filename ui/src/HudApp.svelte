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

  let modelText = $derived.by(() => {
    const raw = summary.current_model || "";
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
  });

  let cacheRateText = $derived(`${Math.round(summary.cache_rate)}%`);
  let inputText = $derived(formatCompact(summary.input_tokens));
  let outputText = $derived(formatCompact(summary.output_tokens));

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
      const targetWidth = Math.max(180, Math.min(520, 180 + (count - 2) * 70));
      void adapter.resizeHud(targetWidth, 34);
    } else {
      void adapter.resizeHud(320, 390);
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

      {#if dashboard}
        <div class="card-breakdown">
          <span>{t("breakdown.input")} {formatCompact(dashboard.totals.input)}</span>
          <span>{t("breakdown.output")} {formatCompact(dashboard.totals.output)}</span>
          <span>{t("breakdown.cacheRead")} {formatCompact(dashboard.totals.cache_read)}</span>
          <span>{t("breakdown.cacheWrite")} {formatCompact(dashboard.totals.cache_creation)}</span>
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
    class:pulsing={isPulsing}
    class:locked={summary.hud_locked}
    data-mode="capsule"
    data-hud="true"
    onpointerdown={handlePointerDown}
    onpointermove={handlePointerMove}
    onpointerup={handlePointerUp}
    oncontextmenu={handleContextMenu}
    title={`会话: ${formatExact(summary.session_total)} | 今日: ${formatExact(summary.total_today)} tokens`}
  >
    {#each activeMetrics as metric, index}
      {#if index > 0}
        <div class="hud-divider"></div>
      {/if}

      {#if metric === "current_rate"}
        <div class="hud-section rate-section" title="当前输出生成速率">
          <span class="hud-icon">⚡️</span>
          <span class="hud-rate-val">{rateText}</span>
        </div>
      {:else if metric === "session_total"}
        <div class="hud-section total-section" title="激活会话总 Token">
          <span class="hud-total-val">{sessionTotalText}</span>
        </div>
      {:else if metric === "current_model"}
        <div class="hud-section model-section" title={`当前模型: ${summary.current_model}`}>
          <span class="hud-icon">🧠</span>
          <span class="hud-model-val">{modelText}</span>
        </div>
      {:else if metric === "cache_rate"}
        <div class="hud-section cache-section" title="会话缓存命中率">
          <span class="hud-icon">💾</span>
          <span class="hud-cache-val">{cacheRateText}</span>
        </div>
      {:else if metric === "input_tokens"}
        <div class="hud-section input-section" title="输入 Token 数">
          <span class="hud-icon">📥</span>
          <span class="hud-val">{inputText}</span>
        </div>
      {:else if metric === "output_tokens"}
        <div class="hud-section output-section" title="输出 Token 数">
          <span class="hud-icon">📤</span>
          <span class="hud-val">{outputText}</span>
        </div>
      {/if}
    {/each}
  </main>
{/if}

<style>
  :global(html),
  :global(body),
  :global(#app) {
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
    background: rgba(22, 24, 29, 0.90);
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
    background: rgba(26, 28, 34, 0.94);
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

  /* ---------------- 卡片小组件 (Widget) ---------------- */
  .hud-card {
    width: 100vw;
    height: 100vh;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    padding: 12px 14px 14px;
    border-radius: 12px;
    background: color-mix(in srgb, var(--bg) 92%, #000 8%);
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

  .card-breakdown {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px 10px;
    font-size: 10.5px;
    color: var(--text-2);
    font-family: var(--font-num);
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
