<script lang="ts">
  // T6 —— 会话列表：活跃会话用琥珀点（状态记号，不是图标）。
  import { formatCompact } from "../format";
  import { t } from "../i18n";
  import type { SessionRow } from "../data";

  let { sessions }: { sessions: SessionRow[] } = $props();
</script>

<section class="section">
  <h2>{t("sessions.title")}</h2>
  <ul>
    {#each sessions as session (session.session_id)}
      <li>
        <span class="dot" class:on={session.active} aria-hidden="true"></span>
        <span class="title" title={session.title}>{session.title}</span>
        {#if session.active}
          <span class="sr">{t("sessions.active")}</span>
        {/if}
        <span class="total">{formatCompact(session.total)}</span>
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
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 5px 0;
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
</style>
