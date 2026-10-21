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
    if (visible.length === 0) {
      return {
        linePath: "",
        areaPath: "",
        last: null,
        hasData: false,
      };
    }

    const totals = visible.map(
      (pulse) =>
        pulse.input + pulse.output + pulse.cache_read + pulse.cache_creation,
    );
    const plotW = W - LEFT - RIGHT;

    // 高斯核平滑采样：64 个时域采样点覆盖 1 小时窗口，sigma = 90s
    const M = 64;
    const dt = WINDOW_MS / (M - 1);
    const sigma = 90_000;
    const twoSigmaSq = 2 * sigma * sigma;

    const rawValues = new Float64Array(M);
    let maxVal = 1;

    for (let i = 0; i < M; i += 1) {
      const t = windowStart + i * dt;
      let sum = 0;
      for (let j = 0; j < visible.length; j += 1) {
        const diff = t - visible[j].t_ms;
        if (Math.abs(diff) < 4 * sigma) {
          sum += totals[j] * Math.exp(-(diff * diff) / twoSigmaSq);
        }
      }
      rawValues[i] = sum;
      if (sum > maxVal) maxVal = sum;
    }

    const points: { x: number; y: number }[] = [];
    for (let i = 0; i < M; i += 1) {
      const x = LEFT + (i / (M - 1)) * plotW;
      const y = BASE_Y - (rawValues[i] / maxVal) * (BASE_Y - TOP);
      points.push({ x, y });
    }

    // 采用 Catmull-Rom 样条拟合为连续三次贝塞尔曲线 (C 指令)，保证一阶导数连续无突兀折角
    const pathParts = [`M${points[0].x.toFixed(1)},${points[0].y.toFixed(1)}`];
    for (let i = 0; i < points.length - 1; i += 1) {
      const p0 = i > 0 ? points[i - 1] : points[i];
      const p1 = points[i];
      const p2 = points[i + 1];
      const p3 = i + 2 < points.length ? points[i + 2] : p2;

      const cp1x = p1.x + (p2.x - p0.x) / 6;
      let cp1y = p1.y + (p2.y - p0.y) / 6;
      const cp2x = p2.x - (p3.x - p1.x) / 6;
      let cp2y = p2.y - (p3.y - p1.y) / 6;

      cp1y = Math.min(BASE_Y, Math.max(TOP, cp1y));
      cp2y = Math.min(BASE_Y, Math.max(TOP, cp2y));

      pathParts.push(
        `C${cp1x.toFixed(1)},${cp1y.toFixed(1)} ${cp2x.toFixed(1)},${cp2y.toFixed(1)} ${p2.x.toFixed(1)},${p2.y.toFixed(1)}`,
      );
    }

    const linePath = pathParts.join(" ");
    const lastPoint = points[points.length - 1];
    const firstPoint = points[0];
    const areaPath = `${linePath} L${lastPoint.x.toFixed(1)},${BASE_Y.toFixed(1)} L${firstPoint.x.toFixed(1)},${BASE_Y.toFixed(1)} Z`;

    // 引导点定位到最近脉冲的高斯波峰坐标
    const lastIdx = visible.length - 1;
    const lastPulse = visible[lastIdx];
    const lastX = LEFT + ((lastPulse.t_ms - windowStart) / WINDOW_MS) * plotW;
    let lastIntensity = 0;
    for (let j = 0; j < visible.length; j += 1) {
      const diff = lastPulse.t_ms - visible[j].t_ms;
      if (Math.abs(diff) < 4 * sigma) {
        lastIntensity += totals[j] * Math.exp(-(diff * diff) / twoSigmaSq);
      }
    }
    const lastY = BASE_Y - (lastIntensity / maxVal) * (BASE_Y - TOP);

    return {
      linePath,
      areaPath,
      last: { x: lastX, y: lastY },
      hasData: true,
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
      <defs>
        <linearGradient id="trend-fill" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stop-color="var(--amber)" stop-opacity="0.22" />
          <stop offset="70%" stop-color="var(--amber)" stop-opacity="0.04" />
          <stop offset="100%" stop-color="var(--amber)" stop-opacity="0" />
        </linearGradient>
      </defs>
      <line class="baseline" x1="0" y1={BASE_Y} x2={W} y2={BASE_Y}></line>
      {#if geometry.hasData}
        <path class="area" d={geometry.areaPath} fill="url(#trend-fill)"></path>
        <path class="line" d={geometry.linePath}></path>
      {/if}
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
  .area {
    pointer-events: none;
  }
  .line {
    fill: none;
    stroke: var(--amber);
    stroke-width: 1.6;
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
