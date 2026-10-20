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
