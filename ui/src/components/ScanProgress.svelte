<script lang="ts">
  // T6 —— D16 首扫进度态：骨架布局 + 文件计数 + 百分比；empty 复用同一套骨架换文案。
  // 骨架块与真实内容同高，切换时大数字就位不闪跳。
  import { formatPercent } from "../format";
  import { t } from "../i18n";
  import type { ScanStatus } from "../data";

  let { status }: { status: ScanStatus } = $props();

  let empty = $derived(status.phase === "empty");
  let pct = $derived(Math.max(0, Math.min(100, status.percent)));
  let hasProgress = $derived(!empty && status.files_total > 0);
</script>

<div class="scan">
  <section class="totals">
    <div class="label-row">
      <span class="label"
        >{empty ? t("scan.emptyTitle") : t("scan.title")}</span
      >
      {#if hasProgress}
        <span class="detail"
          >{t("scan.detail", {
            done: status.files_done,
            total: status.files_total,
            pct: formatPercent(pct),
          })}</span
        >
      {/if}
    </div>
    <div class="sk num"></div>
    {#if empty}
      <p class="hint">{t("scan.emptyHint")}</p>
    {:else}
      <div class="progress"><span style:width={`${pct}%`}></span></div>
    {/if}
  </section>

  <section class="section chips">
    {#each [0, 1, 2, 3] as index (index)}
      <div class="sk chip"></div>
    {/each}
  </section>

  <section class="section">
    <div class="sk head"></div>
    <div class="sk chart"></div>
  </section>

  <section class="section">
    <div class="sk head"></div>
    {#each [0, 1, 2] as index (index)}
      <div class="model">
        <div class="sk model-line"></div>
        <div class="sk model-bar"></div>
      </div>
    {/each}
  </section>

  <section class="section">
    <div class="sk head"></div>
    {#each [0, 1, 2, 3] as index (index)}
      <div class="sk card"></div>
    {/each}
  </section>

  <section class="section">
    <div class="sk head"></div>
    {#each [0, 1, 2, 3] as index (index)}
      <div class="sk row"></div>
    {/each}
  </section>
</div>

<style>
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
  .detail {
    font-family: var(--font-num);
    font-size: 9.5px;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .progress {
    height: 2px;
    background: var(--track);
    margin: 10px 0 13px;
  }
  .progress span {
    display: block;
    height: 100%;
    background: var(--text-2);
    transition: width var(--dur) linear;
  }
  .hint {
    margin: 6px 0 9px;
    font-size: 10.5px;
    color: var(--text-2);
  }
  .sk {
    background: var(--skeleton);
    border-radius: var(--radius);
  }
  .num {
    height: 46px;
    width: 76%;
    margin: 4px 0 7px;
  }
  .chips {
    display: flex;
    gap: 6px;
  }
  .chip {
    width: 48px;
    height: 22px;
  }
  .head {
    height: 10px;
    width: 74px;
    margin-bottom: 12px;
  }
  .chart {
    height: 92px;
    margin-top: 8px;
  }
  .model {
    margin-bottom: 10px;
  }
  .model-line {
    height: 9px;
    width: 100%;
    margin-bottom: 4px;
  }
  .model-bar {
    height: 3px;
    width: 100%;
  }
  .card {
    height: 66px;
    margin-bottom: 8px;
  }
  .row {
    height: 14px;
    margin-bottom: 11px;
  }
</style>
