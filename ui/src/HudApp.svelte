<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { createAdapter, type HudSummary, type UiAdapter } from "./data";
  import { formatCompact, formatExact } from "./format";

  const adapter: UiAdapter = createAdapter();

  let summary = $state<HudSummary>({
    total_today: 0,
    rate_per_min: 0,
    hud_locked: false,
  });

  let isPulsing = $state(false);
  let pulseTimeout: ReturnType<typeof setTimeout> | undefined;

  let rateText = $derived.by(() => {
    if (summary.rate_per_min <= 0) return "-";
    return `${formatCompact(summary.rate_per_min)}/m`;
  });

  let totalText = $derived(formatExact(summary.total_today));

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

  // P1-3 加固：优雅处理拖拽与单击主面板的事件冲突
  let pointerDownTime = 0;
  let pointerDownX = 0;
  let pointerDownY = 0;

  function handlePointerDown(e: PointerEvent): void {
    pointerDownTime = Date.now();
    pointerDownX = e.screenX;
    pointerDownY = e.screenY;
  }

  async function handlePointerUp(e: PointerEvent): Promise<void> {
    const elapsed = Date.now() - pointerDownTime;
    const dist = Math.hypot(e.screenX - pointerDownX, e.screenY - pointerDownY);

    // 位移 <= 3px 且按压时间 < 300ms 判定为常规单击，唤起主面板
    if (elapsed < 300 && dist <= 3) {
      try {
        await adapter.showMainWindow();
      } catch (err) {
        console.error("showMainWindow failed", err);
      }
      return;
    }

    // 若发生了位移（拖拽），获取当前窗口位置并持久化保存
    if (dist > 3) {
      if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
        try {
          const pos = await getCurrentWebviewWindow().outerPosition();
          await adapter.saveHudPosition(Math.round(pos.x), Math.round(pos.y));
        } catch (err) {
          console.warn("Failed to persist HUD position", err);
        }
      }
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

    // 定期刷新衰减速率（30 秒一次）
    const decayTimer = setInterval(() => {
      void loadSummary();
    }, 30_000);

    return () => {
      offPulse();
      offDirty();
      clearInterval(decayTimer);
      if (pulseTimeout) clearTimeout(pulseTimeout);
    };
  });
</script>

<main
  class="hud-pill"
  class:pulsing={isPulsing}
  class:locked={summary.hud_locked}
  data-tauri-drag-region
  onpointerdown={handlePointerDown}
  onpointerup={handlePointerUp}
>
  <div class="hud-section rate-section">
    <span class="hud-icon">⚡️</span>
    <span class="hud-rate-val">{rateText}</span>
  </div>

  <div class="hud-divider"></div>

  <div class="hud-section total-section">
    <span class="hud-total-val">{totalText}</span>
  </div>
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
    border-color: rgba(245, 158, 11, 0.45);
    background: rgba(26, 28, 34, 0.94);
  }

  .hud-section {
    display: flex;
    align-items: center;
    pointer-events: none;
  }

  .rate-section {
    gap: 3px;
    min-width: 58px;
  }

  .hud-icon {
    font-size: 11px;
    line-height: 1;
    filter: drop-shadow(0 0 4px rgba(245, 158, 11, 0.3));
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
    justify-content: flex-end;
    flex: 1;
  }

  .hud-total-val {
    font-size: 12px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    color: #f59e0b;
    letter-spacing: -0.3px;
  }
</style>
