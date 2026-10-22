<script lang="ts">
  import { t } from "../i18n";
  import { formatCompact } from "../format";

  let {
    thinkingShare = null,
    thoughtsTokens = 0,
    outputTokens = 0,
  }: {
    thinkingShare?: number | null;
    thoughtsTokens?: number;
    outputTokens?: number;
  } = $props();

  const pct = $derived(
    thinkingShare != null ? Math.round(thinkingShare * 1000) / 10 : null
  );
  const visibleTokens = $derived(
    Math.max(0, outputTokens - (thoughtsTokens ?? 0))
  );
</script>

{#if thoughtsTokens && thoughtsTokens > 0}
  <div class="iceberg-card">
    <div class="iceberg-header">
      <div class="title-wrap">
        <span class="iceberg-icon" aria-hidden="true">🧊</span>
        <span class="iceberg-title">{t("iceberg.title")}</span>
      </div>
      {#if pct !== null}
        <span class="share-badge">{t("iceberg.share")}: {pct}%</span>
      {:else}
        <span class="share-badge na">N/A</span>
      {/if}
    </div>

    <div class="iceberg-bar-track" aria-hidden="true">
      <div
        class="iceberg-bar-deep"
        style="width: {pct !== null ? Math.min(100, pct) : 0}%"
      ></div>
    </div>

    <div class="iceberg-stats">
      <span class="stat-item surface">
        <span class="stat-dot surface" aria-hidden="true"></span>
        <span class="stat-label">{t("iceberg.surface")}:</span>
        <span class="stat-val">{formatCompact(visibleTokens)}</span>
      </span>
      <span class="stat-item deep">
        <span class="stat-dot deep" aria-hidden="true"></span>
        <span class="stat-label">{t("iceberg.deep")}:</span>
        <span class="stat-val">{formatCompact(thoughtsTokens)}</span>
      </span>
    </div>
  </div>
{/if}

<style>
  .iceberg-card {
    background: var(--bg-1);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 10px 12px;
    margin: 8px 0 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    position: relative;
    overflow: hidden;
  }
  .iceberg-card::before {
    content: "";
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 2px;
    background: linear-gradient(90deg, #38bdf8, #818cf8, #c084fc);
  }
  .iceberg-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .title-wrap {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .iceberg-icon {
    font-size: 13px;
    line-height: 1;
  }
  .iceberg-title {
    font-family: var(--font-num);
    font-size: 11px;
    font-weight: 600;
    color: var(--text-1);
    letter-spacing: 0.02em;
  }
  .share-badge {
    font-family: var(--font-num);
    font-size: 10px;
    font-weight: 600;
    color: #38bdf8;
    background: color-mix(in srgb, #38bdf8 15%, transparent);
    border: 1px solid color-mix(in srgb, #38bdf8 30%, transparent);
    border-radius: 4px;
    padding: 1.5px 6px;
    font-variant-numeric: tabular-nums;
  }
  .share-badge.na {
    color: var(--text-2);
    background: var(--bg-2);
    border-color: var(--line);
  }
  .iceberg-bar-track {
    width: 100%;
    height: 6px;
    background: color-mix(in srgb, var(--accent) 25%, var(--bg-2));
    border-radius: 3px;
    overflow: hidden;
    display: flex;
  }
  .iceberg-bar-deep {
    height: 100%;
    background: linear-gradient(90deg, #38bdf8, #a855f7);
    border-radius: 3px;
    transition: width var(--dur) var(--ease);
  }
  .iceberg-stats {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-family: var(--font-num);
    font-size: 10px;
    line-height: 1.2;
    color: var(--text-2);
  }
  .stat-item {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .stat-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
  }
  .stat-dot.surface {
    background: var(--accent);
  }
  .stat-dot.deep {
    background: #38bdf8;
  }
  .stat-val {
    color: var(--text-1);
    font-weight: 500;
  }
</style>
