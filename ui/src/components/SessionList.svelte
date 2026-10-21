<script lang="ts">
  // T6 —— 会话列表：活跃会话用琥珀点（状态记号，不是图标）。
  import { formatCompact } from "../format";
  import { t } from "../i18n";
  import type { SessionRow } from "../data";

  let {
    sessions,
    onSelect,
  }: {
    sessions: SessionRow[];
    onSelect?: (sessionId: string) => void;
  } = $props();

  function engineLabel(engine?: string): string {
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
  let searchQuery = $state("");
  let filteredSessions = $derived.by(() => {
    const q = searchQuery.trim().toLowerCase();
    if (!q) return sessions;
    return sessions.filter((s) =>
      s.title.toLowerCase().includes(q) ||
      s.session_id.toLowerCase().includes(q) ||
      (s.engine && s.engine.toLowerCase().includes(q))
    );
  });
</script>

<section class="section">
  <div class="section-header">
    <h2>{t("sessions.title")}</h2>
    <div class="search-box">
      <span class="search-icon" aria-hidden="true">⌕</span>
      <input
        type="text"
        bind:value={searchQuery}
        placeholder={t("sessions.search")}
        aria-label={t("sessions.search")}
      />
      {#if searchQuery}
        <button
          type="button"
          class="clear-btn"
          onclick={() => (searchQuery = "")}
          aria-label="清除搜索"
        >×</button>
      {/if}
    </div>
  </div>
  <ul>
    {#if filteredSessions.length === 0}
      <li class="no-match">{t("sessions.noMatches")}</li>
    {:else}
      {#each filteredSessions as session (session.session_id)}
        <li>
          <button
            type="button"
            class="session-btn"
            onclick={() => onSelect?.(session.session_id)}
            title="{session.title} · {t('flight.title')}"
          >
            <span class="dot" class:on={session.active} aria-hidden="true"></span>
            <span class="title">{session.title}</span>
            {#if session.engine}
              <span class="engine-badge {session.engine}">
                {engineLabel(session.engine)}
              </span>
            {/if}
            {#if session.active}
              <span class="sr">{t("sessions.active")}</span>
            {/if}
            <span class="total">{formatCompact(session.total)}</span>
            <span class="chevron" aria-hidden="true">›</span>
          </button>
        </li>
      {/each}
    {/if}
  </ul>
</section>

<style>
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    margin: 2px 0;
  }
  .session-btn {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    padding: 5px 6px;
    margin: 0 -6px;
    border: none;
    background: transparent;
    border-radius: 4px;
    text-align: left;
    cursor: pointer;
    color: inherit;
    font: inherit;
    transition: background 120ms ease;
  }
  .session-btn:hover {
    background: var(--bg-1);
  }
  .session-btn:focus-visible {
    outline: 1px solid var(--accent);
  }
  .dot {
    width: 4px;
    height: 4px;
    border: 1px solid transparent;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .dot.on {
    background: var(--amber);
    border-color: var(--amber);
  }
  .title {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12.5px;
  }
  .engine-badge {
    font-family: var(--font-num);
    font-size: 8.5px;
    line-height: 1.1;
    padding: 1px 3.5px;
    border-radius: var(--radius);
    border: 1px solid var(--line);
    color: var(--text-2);
    background: var(--bg-1);
    letter-spacing: 0.2px;
    text-transform: uppercase;
    flex-shrink: 0;
  }
  .engine-badge.codex {
    color: color-mix(in srgb, #38bdf8 70%, var(--text-2));
    border-color: color-mix(in srgb, #38bdf8 30%, var(--line));
  }
  .engine-badge.claude {
    color: color-mix(in srgb, var(--accent) 70%, var(--text-2));
    border-color: color-mix(in srgb, var(--accent) 30%, var(--line));
  }
  .engine-badge.cline {
    color: color-mix(in srgb, #60a5fa 70%, var(--text-2));
    border-color: color-mix(in srgb, #60a5fa 30%, var(--line));
  }
  .engine-badge.roo {
    color: color-mix(in srgb, #fb923c 70%, var(--text-2));
    border-color: color-mix(in srgb, #fb923c 30%, var(--line));
  }
  .engine-badge.aider {
    color: color-mix(in srgb, #a78bfa 70%, var(--text-2));
    border-color: color-mix(in srgb, #a78bfa 30%, var(--line));
  }
  .engine-badge.cursor {
    color: color-mix(in srgb, #94a3b8 70%, var(--text-2));
    border-color: color-mix(in srgb, #94a3b8 30%, var(--line));
  }
  .engine-badge.opencode {
    color: color-mix(in srgb, #10b981 70%, var(--text-2));
    border-color: color-mix(in srgb, #10b981 30%, var(--line));
  }
  .engine-badge.continue {
    color: color-mix(in srgb, #06b6d4 70%, var(--text-2));
    border-color: color-mix(in srgb, #06b6d4 30%, var(--line));
  }
  .engine-badge.omp {
    color: color-mix(in srgb, #eab308 70%, var(--text-2));
    border-color: color-mix(in srgb, #eab308 30%, var(--line));
  }
  .session-btn:hover .engine-badge {
    border-color: var(--line-strong);
    color: var(--text-1);
  }
  .total {
    font-family: var(--font-num);
    font-size: 11px;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  .chevron {
    font-family: var(--font-num);
    font-size: 13px;
    color: var(--text-2);
    opacity: 0.5;
    padding-left: 2px;
  }
  .session-btn:hover .chevron {
    opacity: 1;
    color: var(--accent);
  }
  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 2px;
  }
  .search-box {
    display: flex;
    align-items: center;
    gap: 4px;
    background: var(--bg-1);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    padding: 1px 6px;
    transition: border-color var(--dur) var(--ease);
  }
  .search-box:focus-within {
    border-color: var(--amber);
  }
  .search-icon {
    font-size: 11px;
    color: var(--text-2);
    line-height: 1;
  }
  .search-box input {
    border: none;
    background: transparent;
    font-family: var(--font-num);
    font-size: 10px;
    color: var(--text);
    outline: none;
    width: 80px;
    transition: width var(--dur) var(--ease);
  }
  .search-box input:focus {
    width: 105px;
  }
  .clear-btn {
    border: none;
    background: transparent;
    color: var(--text-2);
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
    padding: 0 1px;
  }
  .clear-btn:hover {
    color: var(--text);
  }
  .no-match {
    font-size: 11px;
    color: var(--text-2);
    padding: 10px 0;
    text-align: center;
    font-style: italic;
  }
</style>
