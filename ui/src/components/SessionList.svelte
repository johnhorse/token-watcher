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
</script>

<section class="section">
  <h2>{t("sessions.title")}</h2>
  <ul>
    {#each sessions as session (session.session_id)}
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
              {session.engine === "codex" ? "Codex" : "Claude"}
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
</style>
