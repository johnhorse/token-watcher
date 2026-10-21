<script lang="ts">
  import { onMount } from "svelte";
  import { createAdapter, type HudSummary, type UiAdapter } from "./data";
  import { formatCompact, formatExact } from "./format";

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

  $effect(() => {
    const count = activeMetrics.length;
    const targetWidth = Math.max(180, Math.min(520, 180 + (count - 2) * 70));
    void adapter.resizeHud(targetWidth);
  });

  async function loadSummary(): Promise<void> {
    try {
      const data = await adapter.getHudSummary();
      summary = data;
    } catch (e) {
      console.error("Failed to load HUD summary", e);
    }
  }

  function triggerPulseAnimation(): void {
    isPulsing = true;
    if (pulseTimeout) clearTimeout(pulseTimeout);
    pulseTimeout = setTimeout(() => {
      isPulsing = false;
    }, 400);
  }
  // 拖拽与单击主面板的区分处理（基于 Rust moveHudBy 与 setPointerCapture）
  let lastScreenX = 0;
  let lastScreenY = 0;
  let isPointerDown = false;
  let hasDragged = false;
  let pointerDownTime = 0;
  let downDist = 0;

  function handlePointerDown(e: PointerEvent): void {
    if (summary.hud_locked || e.button !== 0) return;
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

    const elapsed = Date.now() - pointerDownTime;
    if (elapsed < 400 && downDist <= 3) {
      try {
        await adapter.showMainWindow();
      } catch (err) {
        console.error("showMainWindow failed", err);
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
    void loadSummary();

    const offPulse = adapter.onPulse((pulse) => {
      const pulseTotal = pulse.input + pulse.output + pulse.cache_read + pulse.cache_creation;
      summary.total_today += pulseTotal;
      triggerPulseAnimation();
      void loadSummary();
    });

    const offDirty = adapter.onDashboardDirty(() => {
      void loadSummary();
    });

    const offHudUpdated = adapter.onHudUpdated ? adapter.onHudUpdated(() => {
      void loadSummary();
    }) : () => {};
    const offTheme = adapter.onThemeChanged((theme) => {
      if (theme === "slate" || theme === "mint" || theme === "meter-face") {
        document.documentElement.setAttribute("data-theme", theme);
        try {
          localStorage.setItem("token_watcher_theme", theme);
        } catch {}
      }
    });

    void (async () => {
      try {
        const serverTheme = await adapter.getTheme();
        if (serverTheme === "slate" || serverTheme === "mint" || serverTheme === "meter-face") {
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
      clearInterval(decayTimer);
      if (pulseTimeout) clearTimeout(pulseTimeout);
    };
  });
</script>

<main
  class="hud-pill"
  class:pulsing={isPulsing}
  class:locked={summary.hud_locked}
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

<style>
  :global(html),
  :global(body) {
    margin: 0;
    padding: 0;
    width: 100vw;
    height: 100vh;
    overflow: hidden;
    background: transparent !important;
    user-select: none;
    -webkit-user-select: none;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace, sans-serif;
  }

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
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.45);
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
</style>
