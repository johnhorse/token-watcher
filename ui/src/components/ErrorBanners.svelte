<script lang="ts">
  // T6 —— D17 三档错误态里的两条横幅：中性色文案，无红绿评判。
  // 「跳过 N 文件」角标由 App 放在总量行右侧。
  import { t } from "../i18n";
  import type { DataHealth } from "../data";

  let {
    health,
    unavailable = false,
  }: { health: DataHealth; unavailable?: boolean } = $props();
</script>

{#if unavailable || health.root_unreadable || health.db_readonly}
  <div class="banners">
    {#if unavailable}
      <div class="banner">{t("health.unavailable")}</div>
    {/if}
    {#if health.root_unreadable}
      <div class="banner">{t("health.root")}</div>
    {/if}
    {#if health.db_readonly}
      <div class="banner">{t("health.readonly")}</div>
    {/if}
  </div>
{/if}

<style>
  .banners {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-bottom: 12px;
  }
  .banner {
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    padding: 7px 10px;
    font-size: 10.5px;
    line-height: 1.35;
    color: var(--text-2);
  }
</style>
