<script lang="ts">
  // T6 —— 订阅档案卡：推断标签恒显示；配额条纯手动语义；Unknown 卡置灰、无配额条；卡片内展开编辑。
  import {
    formatCompact,
    formatExact,
    formatPercent,
    parseTokenInput,
    formatCost,
    formatCostPrecise,
  } from "../format";
  import { t } from "../i18n";
  import type { ProfileCard } from "../data";

  let {
    profiles,
    onUpdateProfile,
  }: {
    profiles: ProfileCard[];
    onUpdateProfile?: (
      profileId: number,
      displayName: string,
      quota: {
        total: number | null;
        used: number | null;
        reset_date: string | null;
      },
    ) => Promise<void>;
  } = $props();

  let editingId = $state<number | null>(null);
  let editName = $state("");
  let editTotalStr = $state("");
  let editUsedStr = $state("");
  let editResetDate = $state("");
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  let parsedTotal = $derived(parseTokenInput(editTotalStr));
  let parsedUsed = $derived(parseTokenInput(editUsedStr));
  let canSave = $derived(
    editName.trim().length > 0 && !parsedTotal.error && !parsedUsed.error,
  );

  function startEdit(profile: ProfileCard) {
    if (profile.profile_id === null) return;
    editingId = profile.profile_id;
    editName = profile.display_name;
    editTotalStr =
      profile.quota?.total != null ? String(profile.quota.total) : "";
    editUsedStr = profile.quota?.used != null ? String(profile.quota.used) : "";
    editResetDate = profile.quota?.reset_date
      ? profile.quota.reset_date.slice(0, 10)
      : "";
    saveError = null;
  }

  function cancelEdit() {
    editingId = null;
    saveError = null;
  }

  async function handleSave(profileId: number) {
    if (!canSave || saving || !onUpdateProfile) return;
    saving = true;
    saveError = null;
    try {
      await onUpdateProfile(profileId, editName.trim(), {
        total: parsedTotal.value,
        used: parsedUsed.value,
        reset_date: editResetDate || null,
      });
      editingId = null;
    } catch (err) {
      saveError = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }

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
          <div class="head-right">
            {#if profile.cost}
              <span
                class="cost"
                title={`${t("cost.estimateTooltip")} · ${formatCostPrecise(profile.cost.total)}`}
              >
                {formatCost(profile.cost.total)}
                {#if !profile.cost.is_complete}
                  <span
                    class="unpriced-mark"
                    title={t("cost.unpricedNote", {
                      n: formatCompact(profile.cost.unpriced_tokens),
                    })}>*</span
                  >
                {/if}
              </span>
            {/if}
            <span class="tokens">{formatCompact(profile.tokens.total)}</span>
            {#if !unknown}
              <button
                type="button"
                class="edit-btn"
                onclick={() =>
                  editingId === profile.profile_id
                    ? cancelEdit()
                    : startEdit(profile)}
              >
                {editingId === profile.profile_id
                  ? t("profiles.cancel")
                  : t("profiles.edit")}
              </button>
            {/if}
          </div>
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
        {#if editingId === profile.profile_id}
          <form
            class="edit-form"
            onsubmit={(e) => {
              e.preventDefault();
              void handleSave(profile.profile_id!);
            }}
          >
            <div class="field">
              <label for="name-{profile.profile_id}"
                >{t("profiles.nameLabel")}</label
              >
              <input
                id="name-{profile.profile_id}"
                type="text"
                bind:value={editName}
                required
              />
            </div>
            <div class="grid-2">
              <div class="field">
                <div class="label-row">
                  <label for="total-{profile.profile_id}"
                    >{t("profiles.totalLabel")}</label
                  >
                  {#if parsedTotal.value !== null}
                    <span class="preview"
                      >{formatExact(parsedTotal.value)}</span
                    >
                  {:else if parsedTotal.error}
                    <span class="preview error"
                      >{t("profiles.invalidNumber")}</span
                    >
                  {:else}
                    <span class="preview hint">{t("profiles.noLimit")}</span>
                  {/if}
                </div>
                <input
                  id="total-{profile.profile_id}"
                  type="text"
                  bind:value={editTotalStr}
                  placeholder={t("profiles.tokenHint")}
                />
              </div>
              <div class="field">
                <div class="label-row">
                  <label for="used-{profile.profile_id}"
                    >{t("profiles.usedLabel")}</label
                  >
                  {#if parsedUsed.value !== null}
                    <span class="preview">{formatExact(parsedUsed.value)}</span>
                  {:else if parsedUsed.error}
                    <span class="preview error"
                      >{t("profiles.invalidNumber")}</span
                    >
                  {/if}
                </div>
                <input
                  id="used-{profile.profile_id}"
                  type="text"
                  bind:value={editUsedStr}
                  placeholder={t("profiles.tokenHint")}
                />
              </div>
            </div>
            <div class="field">
              <label for="reset-{profile.profile_id}"
                >{t("profiles.resetLabel")}</label
              >
              <input
                id="reset-{profile.profile_id}"
                type="date"
                bind:value={editResetDate}
              />
            </div>
            {#if saveError}
              <div class="error-msg">{saveError}</div>
            {/if}
            <div class="actions">
              <button
                type="button"
                class="btn secondary"
                onclick={cancelEdit}
                disabled={saving}
              >
                {t("profiles.cancel")}
              </button>
              <button
                type="submit"
                class="btn primary"
                disabled={saving || !canSave}
              >
                {saving ? t("profiles.saving") : t("profiles.save")}
              </button>
            </div>
          </form>
        {:else if !unknown && pct !== null}
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
  .cost {
    font-family: var(--font-num);
    font-size: 11px;
    color: var(--text-1);
    font-variant-numeric: tabular-nums;
  }
  .unpriced-mark {
    color: var(--accent);
    font-weight: bold;
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
  .head-right {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }
  .edit-btn {
    background: transparent;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    color: var(--text-2);
    font-size: 9px;
    font-family: var(--font-ui);
    padding: 1px 5px;
    cursor: pointer;
    line-height: 1.3;
  }
  .edit-btn:hover {
    color: var(--text);
    border-color: var(--text-2);
  }
  .edit-form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 10px;
    padding-top: 8px;
    border-top: 1px dashed var(--line);
  }
  .grid-2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .label-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 4px;
  }
  label {
    font-size: 10px;
    color: var(--text-2);
  }
  .preview {
    font-family: var(--font-num);
    font-size: 9.5px;
    color: var(--amber);
  }
  .preview.error {
    color: #e06c75;
  }
  .preview.hint {
    color: var(--text-2);
  }
  input[type="text"],
  input[type="date"] {
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    color: var(--text);
    font-family: var(--font-num);
    font-size: 11px;
    padding: 4px 6px;
    box-sizing: border-box;
    width: 100%;
  }
  input[type="text"]:focus,
  input[type="date"]:focus {
    outline: 1px solid var(--amber);
    border-color: var(--amber);
  }
  .error-msg {
    color: #e06c75;
    font-size: 10px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
    margin-top: 4px;
  }
  .btn {
    font-family: var(--font-ui);
    font-size: 10.5px;
    padding: 3px 10px;
    border-radius: var(--radius);
    cursor: pointer;
  }
  .btn.secondary {
    background: transparent;
    color: var(--text-2);
    border: 1px solid var(--line-strong);
  }
  .btn.secondary:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--text-2);
  }
  .btn.primary {
    background: var(--amber);
    color: #1a1815;
    font-weight: 500;
    border: 1px solid var(--amber);
  }
  .btn.primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
