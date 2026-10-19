<script lang="ts">
  // T6 —— 到达脉冲折线：最近 1 小时，口径 Reported tokens/s。
  // 每个脉冲 = 一次 usage 落库；脉冲间平线 = 未报告用量（不是零消耗）。
  // 空闲 30s 冻结窗口、前导点变空心（笔抬起）；reduced-motion 下不滑窗、不闪点。
  import { t } from "../i18n";
  import { prefersReducedMotion } from "../motion";
  import type { UsagePulse } from "../data";

  let { pulses }: { pulses: UsagePulse[] } = $props();

  const WINDOW_MS = 3_600_000;
  const IDLE_MS = 30_000;
  const W = 340;
  const H = 92;
  const TOP = 8;
  const LEFT = 2;
  const RIGHT = 4;
  const BASE_Y = H - 6;

  const reduced = prefersReducedMotion();

  let now = $state(Date.now());
  let flashAt = $state<number | null>(null);
  let previousLast = 0;

  let sorted = $derived([...pulses].sort((a, b) => a.t_ms - b.t_ms));
  let lastT = $derived(sorted.length ? sorted[sorted.length - 1].t_ms : 0);
  let idle = $derived(lastT === 0 || now - lastT >= IDLE_MS);

  $effect(() => {
    const anchor = lastT;
    if (!anchor) return;
    const timer = setInterval(() => {
      now = Date.now();
      if (Date.now() - anchor >= IDLE_MS) clearInterval(timer);
    }, 1000);
    return () => clearInterval(timer);
  });

  $effect(() => {
    const anchor = lastT;
    if (anchor > 0 && previousLast > 0 && anchor !== previousLast && !reduced) {
      flashAt = anchor;
      setTimeout(() => {
        if (flashAt === anchor) flashAt = null;
      }, 240);
    }
    previousLast = anchor;
  });

  // 空闲时窗口冻结在「最后脉冲 + 30s」；reduced-motion 下窗口锚定最后脉冲（不滑窗）。
  let end = $derived(
    reduced
      ? lastT || now
      : idle
        ? lastT
          ? lastT + IDLE_MS
          : now
        : now,
  );

  let geometry = $derived.by(() => {
    const windowStart = end - WINDOW_MS;
    const visible = sorted.filter(
      (pulse) => pulse.t_ms >= windowStart && pulse.t_ms <= end,
    );
    const totals = visible.map(
      (pulse) =>
        pulse.input + pulse.output + pulse.cache_read + pulse.cache_creation,
    );
    const max = Math.max(1, ...totals);
    const plotW = W - LEFT - RIGHT;
    const xOf = (ms: number) =>
      LEFT + ((ms - windowStart) / WINDOW_MS) * plotW;
    const yOf = (tokens: number) => BASE_Y - (tokens / max) * (BASE_Y - TOP);

    const points: { x: number; y: number }[] = [{ x: LEFT, y: BASE_Y }];
    for (let i = 0; i < visible.length; i += 1) {
      const pulse = visible[i];
      const previous = i > 0 ? visible[i - 1].t_ms : windowStart;
      const next =
        i < visible.length - 1 ? visible[i + 1].t_ms : end;
      const half = Math.min(
        Math.max(Math.min(pulse.t_ms - previous, next - pulse.t_ms) / 2, 20_000),
        120_000,
      );
      points.push(
        { x: Math.max(LEFT, xOf(pulse.t_ms - half)), y: BASE_Y },
        { x: xOf(pulse.t_ms), y: yOf(totals[i]) },
        {
          x: Math.min(W - RIGHT, xOf(pulse.t_ms + half)),
          y: BASE_Y,
        },
      );
    }
    points.push({ x: W - RIGHT, y: BASE_Y });

    const lastIndex = visible.length - 1;
    return {
      path: points
        .map(
          (point, index) =>
            `${index === 0 ? "M" : "L"}${point.x.toFixed(1)},${point.y.toFixed(1)}`,
        )
        .join(" "),
      last:
        lastIndex >= 0
          ? {
              x: xOf(visible[lastIndex].t_ms),
              y: yOf(totals[lastIndex]),
            }
          : null,
      hasData: visible.length > 0,
    };
  });
</script>

<section class="section">
  <header class="row">
    <h2>{t("activity.title")}</h2>
    <span class="status" class:on={!idle}>
      <span class="dot" aria-hidden="true"></span>
      {idle ? t("activity.idle") : t("activity.live")}
    </span>
    <span class="metric">{t("activity.metric")}</span>
  </header>
  <div class="body">
    <svg viewBox={`0 0 ${W} ${H}`} role="img" aria-label={t("activity.aria")}>
      <line class="baseline" x1="0" y1={BASE_Y} x2={W} y2={BASE_Y}></line>
      <path class="line" d={geometry.path}></path>
      {#if geometry.last}
        {#if flashAt !== null}
          <circle
            class="flash"
            cx={geometry.last.x}
            cy={geometry.last.y}
            r="6"
          ></circle>
        {/if}
        <circle
          class="leading"
          class:hollow={idle}
          cx={geometry.last.x}
          cy={geometry.last.y}
          r="2.6"
        ></circle>
      {/if}
    </svg>
    {#if !geometry.hasData}
      <p class="empty">{t("activity.empty")}</p>
    {/if}
  </div>
</section>

<style>
  .row {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  h2 {
    flex: 1;
    margin: 0;
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-family: var(--font-num);
    font-size: 9px;
    letter-spacing: 0.04em;
    color: var(--text-2);
  }
  .status .dot {
    width: 4px;
    height: 4px;
    border: 1px solid var(--amber);
    border-radius: 50%;
  }
  .status.on {
    color: var(--amber);
  }
  .status.on .dot {
    background: var(--amber);
  }
  .metric {
    font-family: var(--font-num);
    font-size: 9.5px;
    color: var(--text-2);
    white-space: nowrap;
  }
  .body {
    position: relative;
    margin-top: 8px;
  }
  svg {
    display: block;
    width: 100%;
    height: auto;
    overflow: visible;
  }
  .baseline {
    stroke: var(--line);
    stroke-width: 1;
  }
  .line {
    fill: none;
    stroke: var(--amber);
    stroke-width: 1.4;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .leading {
    fill: var(--amber);
    stroke: var(--amber);
    stroke-width: 1;
  }
  .leading.hollow {
    fill: var(--bg);
  }
  .flash {
    fill: var(--amber);
    opacity: 0;
    animation: flash 240ms linear both;
  }
  @keyframes flash {
    from {
      opacity: 0.35;
    }
    to {
      opacity: 0;
    }
  }
  .empty {
    position: absolute;
    inset: 0;
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 10.5px;
    color: var(--text-2);
    pointer-events: none;
  }
</style>
