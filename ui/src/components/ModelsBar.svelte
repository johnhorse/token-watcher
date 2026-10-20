<script lang="ts">
  // T6 —— 模型分布横条（实心 = 实测；无象形图标，只有发丝线/进度条）。
  import { formatCompact, formatCost, formatCostPrecise } from "../format";
  import { t } from "../i18n";
  import type { ModelRow } from "../data";

  let { models }: { models: ModelRow[] } = $props();

  let max = $derived(Math.max(1, ...models.map((row) => row.tokens)));
</script>

<section class="section">
  <h2>{t("models.title")}</h2>
  <ul>
    {#each models as row (row.model)}
      <li>
        <div class="row">
          <span class="name" title={row.model}>{row.model}</span>
          <div class="values">
            {#if row.cost !== undefined && row.cost !== null}
              <span class="cost" title={formatCostPrecise(row.cost)}>
                {formatCost(row.cost)}
              </span>
            {/if}
            <span class="value">{formatCompact(row.tokens)}</span>
          </div>
        </div>
        <div class="bar">
          <span style:width={`${(row.tokens / max) * 100}%`}></span>
        </div>
      </li>
    {/each}
  </ul>
</section>

<style>
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    margin-bottom: 9px;
  }
  li:last-child {
    margin-bottom: 0;
  }
  .row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
    margin-bottom: 4px;
  }
  .name {
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .values {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .cost {
    font-family: var(--font-num);
    font-size: 10.5px;
    color: var(--text-1);
    font-variant-numeric: tabular-nums;
  }
  .value {
    font-family: var(--font-num);
    font-size: 11px;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  .bar {
    height: 3px;
    background: var(--track);
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--text-2);
  }
</style>
