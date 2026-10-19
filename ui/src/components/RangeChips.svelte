<script lang="ts">
  // T6 —— 时段筛选 chips：一处切换全局过滤（折线固定最近 1 小时，不受影响）。
  import { t } from "../i18n";
  import type { Range } from "../data";

  let {
    value,
    onchange,
  }: { value: Range; onchange: (range: Range) => void } = $props();

  const ranges: Range[] = ["today", "week", "month", "all"];
</script>

<div class="chips" role="group">
  {#each ranges as range (range)}
    <button
      type="button"
      class:active={range === value}
      aria-pressed={range === value}
      onclick={() => onchange(range)}
    >
      {t(`range.${range}`)}
    </button>
  {/each}
</div>

<style>
  .chips {
    display: flex;
    gap: 6px;
    padding: 10px 0;
    border-top: 1px solid var(--line);
  }
  button {
    font-family: var(--font-num);
    font-size: 10.5px;
    color: var(--text-2);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    padding: 2px 9px;
    transition: border-color var(--dur) var(--ease), color var(--dur) var(--ease);
  }
  button:hover {
    color: var(--text);
  }
  button.active {
    color: var(--text);
    border-color: var(--amber);
  }
</style>
