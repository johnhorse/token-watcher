<script lang="ts">
  // T6 —— 订阅档案卡：推断标签恒显示；配额条纯手动语义；Unknown 卡置灰、无配额条。
  import { formatCompact, formatPercent } from "../format";
  import { t } from "../i18n";
  import type { ProfileCard } from "../data";

  let { profiles }: { profiles: ProfileCard[] } = $props();

  type ResetInfo = {
    key: "profiles.reset" | "profiles.resetToday" | "profiles.resetPast";
    params: Record<string, string | number>;
  };

  function percent(profile: ProfileCard): number | null {
    const quota = profile.quota;
    if (
      !quota ||
      quota.total === null ||
      quota.used === null ||
      quota.total <= 0
    ) {
      return null;
    }
    return Math.min(100, Math.max(0, (quota.used / quota.total) * 100));
  }

  function resetInfo(profile: ProfileCard): ResetInfo | null {
    const raw = profile.quota?.reset_date;
    if (!raw) return null;
    const date = new Date(raw.length <= 10 ? `${raw}T00:00:00` : raw);
    if (Number.isNaN(date.getTime())) return null;
    const days = Math.ceil((date.getTime() - Date.now()) / 86_400_000);
    if (days > 0) {
      return {
        key: "profiles.reset",
        params: { date: raw.slice(0, 10), days },
      };
    }
    return days === 0
      ? { key: "profiles.resetToday", params: {} }
      : { key: "profiles.resetPast", params: {} };
  }
</script>

<section class="section">
  <h2>{t("profiles.title")}</h2>
  <ul>
    {#each profiles as profile (profile.profile_id ?? "unknown")}
      {@const unknown = profile.profile_id === null}
      {@const pct = unknown ? null : percent(profile)}
      {@const reset = unknown ? null : resetInfo(profile)}
      {@const name = unknown ? t("profiles.unknown") : profile.display_name}
      <li class="card" class:unknown>
        <div class="head">
          <span class="name" title={name}>{name}</span>
          <span class="tokens">{formatCompact(profile.tokens.total)}</span>
        </div>
        <div class="meta">
          <span class="endpoint" title={profile.endpoint ?? undefined}
            >{profile.endpoint ?? ""}</span
          >
          <span class="tags">
            {#if profile.inferred}
              <span class="tag">{t("profiles.inferred")}</span>
            {/if}
            {#if profile.quota}
              <span class="tag">{t("profiles.manual")}</span>
            {/if}
          </span>
        </div>
        {#if !unknown && pct !== null}
          <div class="bar">
            <span style:width={`${pct}%`}></span>
          </div>
          <div class="caption">
            <span>{t("profiles.quotaUsed", { pct: formatPercent(pct) })}</span>
            <span
              >{t("profiles.quotaOf", {
                used: formatCompact(profile.quota?.used ?? 0),
                total: formatCompact(profile.quota?.total ?? 0),
              })}</span
            >
            {#if reset}
              <span>{t(reset.key, reset.params)}</span>
            {/if}
          </div>
        {:else if !unknown}
          <div class="caption">
            <span>{t("profiles.noQuota")}</span>
            {#if reset}
              <span>{t(reset.key, reset.params)}</span>
            {/if}
          </div>
        {/if}
      </li>
    {/each}
  </ul>
</section>

<style>
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .card {
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 9px 11px 10px;
  }
  .card.unknown {
    opacity: 0.58;
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
  }
  .name {
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tokens {
    font-family: var(--font-num);
    font-size: 11px;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  .meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-top: 3px;
    min-height: 14px;
  }
  .endpoint {
    font-family: var(--font-num);
    font-size: 9px;
    color: var(--text-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tags {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
  }
  .tag {
    font-family: var(--font-num);
    font-size: 8.5px;
    letter-spacing: 0.02em;
    color: var(--text-2);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    padding: 1px 4px;
    white-space: nowrap;
  }
  .bar {
    height: 3px;
    background: var(--track);
    margin: 8px 0 5px;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--text-2);
  }
  .caption {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 10px;
    font-family: var(--font-num);
    font-size: 9.5px;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
</style>
