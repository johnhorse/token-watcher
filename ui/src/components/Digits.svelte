<script lang="ts">
  // T6 —— 大数字逐位 120ms 上滑（机械寄存器，非滚轮）；burst 最多 3 步 × 80ms 中间值。
  import { onDestroy, untrack } from "svelte";
  import { formatCompact, formatExact } from "../format";
  import { prefersReducedMotion } from "../motion";

  let { value, compact = true }: { value: number; compact?: boolean } = $props();

  const reduced = prefersReducedMotion();
  const STEP_MS = 80;
  const ROLL_MS = 120;

  type Cell = { ch: string; prev: string | null; ver: number; digit: boolean };

  function formatText(v: number): string {
    if (compact) {
      return formatCompact(v);
    }
    return formatExact(v);
  }

  let display = $state(untrack(() => Math.round(value)));
  let roll = $state(ROLL_MS);
  let cells = $state<Cell[]>(
    untrack(() => toCells(formatText(display))),
  );
  let target = $derived(Math.round(value));

  let prevText = untrack(() => formatText(display));
  let version = 0;
  let timers: ReturnType<typeof setTimeout>[] = [];

  function toCells(text: string): Cell[] {
    return [...text].map((ch) => ({
      ch,
      prev: null,
      ver: 0,
      digit: /\d/.test(ch),
    }));
  }

  function clearTimers(): void {
    for (const timer of timers) clearTimeout(timer);
    timers = [];
  }

  function apply(next: number, duration: number): void {
    const text = formatText(next);
    const previous = prevText;
    cells = [...text].map((ch, index) => {
      const before = previous[index] ?? null;
      const changed = before !== ch;
      return {
        ch,
        prev: changed ? before : null,
        ver: changed ? ++version : 0,
        digit: /\d/.test(ch),
      };
    });
    prevText = text;
    display = next;
    roll = duration;
  }

  $effect(() => {
    const _c = compact;
    untrack(() => {
      clearTimers();
      apply(display, 0);
    });
  });

  $effect(() => {
    const goal = target;
    const from = untrack(() => display);
    if (goal === from) return;
    clearTimers();
    if (reduced) {
      apply(goal, ROLL_MS);
      return;
    }
    const delta = goal - from;
    // 小幅增量（实时累计）走 burst；range 切换等大幅跳变直接到位，只做逐位滑动。
    const burst = Math.abs(delta) <= Math.max(50_000, Math.abs(from) * 0.05);
    if (!burst) {
      apply(goal, ROLL_MS);
      return;
    }
    for (let step = 1; step <= 3; step += 1) {
      const next = step === 3 ? goal : Math.round(from + delta * (step / 3));
      timers.push(setTimeout(() => apply(next, STEP_MS), STEP_MS * step));
    }
  });

  onDestroy(clearTimers);
</script>

<span class="digits" style={`--roll: ${roll}ms`}>
  <span class="sr">{formatText(target)}</span>
  {#each cells as cell, index (index + ":" + cell.ver)}
    {#if cell.digit && cell.prev !== null}
      <span class="cell" aria-hidden="true">
        <span class="prev">{cell.prev}</span>
        <span class="cur">{cell.ch}</span>
      </span>
    {:else}
      <span class="cell plain" aria-hidden="true">{cell.ch}</span>
    {/if}
  {/each}
</span>

<style>
  .digits {
    display: inline-flex;
    align-items: flex-start;
    font-family: var(--font-num);
    font-variant-numeric: tabular-nums;
    line-height: 1.15;
    white-space: nowrap;
  }
  .cell {
    position: relative;
    display: inline-block;
    height: 1.15em;
    overflow: hidden;
  }
  .plain {
    display: block;
    height: 100%;
  }
  .cur {
    display: block;
    height: 100%;
    animation: digit-in var(--roll) steps(2, end) both;
  }
  .prev {
    position: absolute;
    inset: 0;
    animation: digit-out var(--roll) steps(2, end) both;
  }
  @keyframes digit-in {
    from {
      transform: translateY(100%);
    }
    to {
      transform: translateY(0);
    }
  }
  @keyframes digit-out {
    from {
      transform: translateY(0);
    }
    to {
      transform: translateY(-100%);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .cur,
    .prev {
      animation: none;
    }
  }
</style>
