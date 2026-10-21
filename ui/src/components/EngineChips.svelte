<script lang="ts">
  import { t } from "../i18n";
  import { formatCompact } from "../format";
  import type { EngineSlice } from "../data";

  let {
    selectedEngine = null,
    availableEngines = [],
    onselect,
  }: {
    selectedEngine: string | null;
    availableEngines: EngineSlice[];
    onselect: (engine: string | null) => void;
  } = $props();

  function engineLabel(engine: string): string {
    switch (engine) {
      case "codex": return "Codex";
      case "cline": return "Cline";
      case "roo": return "Roo";
      case "aider": return "Aider";
      case "cursor": return "Cursor";
      case "opencode": return "OpenCode";
      case "continue": return "Continue";
      case "omp": return "omp";
      default: return "Claude";
    }
  }

  const isAll = $derived(!selectedEngine || selectedEngine === "all");
</script>

{#if availableEngines && availableEngines.length > 0}
  <div class="engine-chips" role="group" aria-label="数据源筛选">
    <button
      type="button"
      class="chip-btn"
      class:active={isAll}
      aria-pressed={isAll}
      onclick={() => onselect(null)}
    >
      <span class="label">{t("engine.all")}</span>
    </button>
    {#each availableEngines as slice (slice.engine)}
      {@const active = selectedEngine === slice.engine}
      <button
        type="button"
        class="chip-btn {slice.engine}"
        class:active
        aria-pressed={active}
        onclick={() => onselect(slice.engine)}
        title="{engineLabel(slice.engine)} · {formatCompact(slice.tokens)}"
      >
        <span class="dot {slice.engine}" aria-hidden="true"></span>
        <span class="label">{engineLabel(slice.engine)}</span>
        <span class="count">{formatCompact(slice.tokens)}</span>
      </button>
    {/each}
  </div>
{/if}

<style>
  .engine-chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
    padding: 0 0 10px;
    margin-top: -2px;
  }
  .chip-btn {
    display: inline-flex;
    align-items: center;
    gap: 4.5px;
    font-family: var(--font-num);
    font-size: 10px;
    line-height: 1.2;
    color: var(--text-2);
    border: 1px solid var(--line-strong);
    background: var(--bg-1);
    border-radius: var(--radius);
    padding: 2px 7px;
    cursor: pointer;
    transition: all var(--dur) var(--ease);
    user-select: none;
  }
  .chip-btn:hover {
    color: var(--text);
    border-color: var(--line-stronger, var(--text-2));
  }
  .chip-btn.active {
    color: var(--text);
    border-color: var(--amber);
    background: var(--bg-2, #38342e);
  }
  .dot {
    width: 4.5px;
    height: 4.5px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--text-2);
  }
  .dot.claude { background: #e8a33d; }
  .dot.codex { background: #38bdf8; }
  .dot.cline { background: #60a5fa; }
  .dot.roo { background: #fb923c; }
  .dot.aider { background: #a855f7; }
  .dot.opencode { background: #10b981; }
  .dot.continue { background: #06b6d4; }
  .dot.omp { background: #eab308; }

  .label {
    letter-spacing: 0.02em;
  }
  .count {
    opacity: 0.65;
    font-size: 9px;
  }
  .chip-btn.active .count {
    opacity: 0.9;
  }
</style>
