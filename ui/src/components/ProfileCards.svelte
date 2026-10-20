<script lang="ts">
  // T6 —— 订阅档案卡：推断标签恒显示；自动/手动双模配额；诚实透支呈现；刷新按钮与编辑模态窗。
  import {
    formatCompact,
    formatExact,
    formatPercent,
    formatCurrency,
    parseTokenInput,
    parseAmountInput,
    formatCost,
    formatCostPrecise,
  } from "../format";
  import { t } from "../i18n";
  import type { ProfileCard } from "../data";

  let {
    profiles,
    onUpdateProfile,
    onSyncQuota,
  }: {
    profiles: ProfileCard[];
    onUpdateProfile?: (
      profileId: number,
      displayName: string,
      quota: {
        quota_type?: string;
        unit?: string;
        api_url?: string | null;
        api_key?: string | null;
        total: number | null;
        used: number | null;
        reset_date: string | null;
      },
    ) => Promise<void>;
    onSyncQuota?: (profileId: number) => Promise<unknown>;
  } = $props();

  let editingId = $state<number | null>(null);
  let editName = $state("");
  let editMode = $state<"manual" | "api">("manual");
  let editUnit = $state<"usd" | "tokens" | "cny">("usd");
  let editApiUrl = $state("");
  let editApiKey = $state("");
  let editTotalStr = $state("");
  let editUsedStr = $state("");
  let editResetDate = $state("");
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  // 同步加载状态跟踪
  let syncingIds = $state<Record<number, boolean>>({});

  let parsedTotal = $derived(
    editUnit === "tokens" ? parseTokenInput(editTotalStr) : parseAmountInput(editTotalStr),
  );
  let parsedUsed = $derived(
    editUnit === "tokens" ? parseTokenInput(editUsedStr) : parseAmountInput(editUsedStr),
  );
  let canSave = $derived(
    editName.trim().length > 0 &&
      (editMode === "api" || (!parsedTotal.error && !parsedUsed.error)),
  );

  function startEdit(profile: ProfileCard) {
    if (profile.profile_id === null) return;
    editingId = profile.profile_id;
    editName = profile.display_name;
    editMode = profile.quota?.quota_type === "api" ? "api" : "manual";
    editUnit = profile.quota?.unit === "cny" ? "cny" : profile.quota?.unit === "tokens" ? "tokens" : "usd";
    editApiUrl = "";
    editApiKey = "";
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
        quota_type: editMode,
        unit: editUnit,
        api_url: editMode === "api" && editApiUrl.trim() ? editApiUrl.trim() : null,
        api_key: editMode === "api" && editApiKey.trim() ? editApiKey.trim() : null,
        total: editMode === "manual" ? parsedTotal.value : null,
        used: editMode === "manual" ? parsedUsed.value : null,
        reset_date: editResetDate || null,
      });
      editingId = null;
    } catch (err) {
      saveError = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }

  async function triggerSync(profileId: number) {
    if (!onSyncQuota || syncingIds[profileId]) return;
    syncingIds = { ...syncingIds, [profileId]: true };
    try {
      await onSyncQuota(profileId);
    } catch (err) {
      console.error("Quota sync failed:", err);
    } finally {
      syncingIds = { ...syncingIds, [profileId]: false };
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
    // 允许透支，不封顶 100%
    return Math.max(0, (quota.used / quota.total) * 100);
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

  function formatQuotaValue(value: number | null | undefined, unit: string | undefined): string {
    if (value === null || value === undefined) return "—";
    if (unit === "tokens") {
      return formatCompact(value);
    }
    return formatCurrency(value, unit === "cny" ? "cny" : "usd");
  }
</script>

<section class="section">
  <h2>{t("profiles.title")}</h2>
  <ul>
    {#each profiles as profile (profile.profile_id ?? "unknown")}
      {@const unknown = profile.profile_id === null}
      {@const pct = unknown ? null : percent(profile)}
      {@const isOverdraft = pct !== null && pct > 100}
      {@const reset = unknown ? null : resetInfo(profile)}
      {@const name = unknown ? t("profiles.unknown") : profile.display_name}
      {@const quota = profile.quota}
      {@const isApi = quota?.quota_type === "api"}
      {@const isSyncing = profile.profile_id !== null && !!syncingIds[profile.profile_id]}
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
            {#if isApi}
              <span class="tag api">
                {t("profiles.autoApi")} {quota?.unit === "cny" ? "¥" : "$"}
              </span>
              {#if profile.profile_id !== null}
                <button
                  type="button"
                  class="refresh-icon-btn"
                  class:rotating={isSyncing}
                  disabled={isSyncing}
                  title={quota?.sync_status === "error"
                    ? (quota?.sync_error ?? t("profiles.refreshTooltip"))
                    : quota?.sync_status === "inactive"
                    ? t("profiles.syncInactive")
                    : t("profiles.refreshTooltip")}
                  onclick={() => triggerSync(profile.profile_id!)}
                >
                  {#if quota?.sync_status === "error"}
                    <span class="err-icon">!</span>
                  {:else if quota?.sync_status === "inactive"}
                    <span class="inactive-icon">◌</span>
                  {:else}
                    ↻
                  {/if}
                </button>
              {/if}
            {:else if quota}
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

            <!-- 配额模式与单位切换 -->
            <div class="grid-2">
              <div class="field">
                <label for="mode-{profile.profile_id}">{t("profiles.modeLabel")}</label>
                <select id="mode-{profile.profile_id}" bind:value={editMode} class="select-input">
                  <option value="manual">{t("profiles.modeManual")}</option>
                  <option value="api">{t("profiles.modeApi")}</option>
                </select>
              </div>
              <div class="field">
                <label for="unit-{profile.profile_id}">{t("profiles.unitLabel")}</label>
                <select id="unit-{profile.profile_id}" bind:value={editUnit} class="select-input">
                  <option value="usd">{t("profiles.unitUsd")}</option>
                  <option value="cny">{t("profiles.unitCny")}</option>
                  <option value="tokens">{t("profiles.unitTokens")}</option>
                </select>
              </div>
            </div>

            {#if editMode === "api"}
              <!-- 自动配额专属输入项 -->
              <div class="field">
                <label for="api-url-{profile.profile_id}">{t("profiles.apiUrlLabel")}</label>
                <input
                  id="api-url-{profile.profile_id}"
                  type="text"
                  bind:value={editApiUrl}
                  placeholder={t("profiles.apiUrlPlaceholder")}
                />
              </div>
              <div class="field">
                <label for="api-key-{profile.profile_id}">{t("profiles.apiKeyLabel")}</label>
                <input
                  id="api-key-{profile.profile_id}"
                  type="password"
                  bind:value={editApiKey}
                  placeholder={t("profiles.apiKeyPlaceholder")}
                />
              </div>
            {:else}
              <!-- 手动模式专属输入项 -->
              <div class="grid-2">
                <div class="field">
                  <div class="label-row">
                    <label for="total-{profile.profile_id}"
                      >{t("profiles.totalLabel")}</label
                    >
                    {#if parsedTotal.value !== null}
                      <span class="preview"
                        >{editUnit === "tokens" ? formatExact(parsedTotal.value) : formatCurrency(parsedTotal.value, editUnit)}</span
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
                    placeholder={editUnit === "tokens" ? t("profiles.tokenHint") : t("profiles.amountHint")}
                  />
                </div>
                <div class="field">
                  <div class="label-row">
                    <label for="used-{profile.profile_id}"
                      >{t("profiles.usedLabel")}</label
                    >
                    {#if parsedUsed.value !== null}
                      <span class="preview">
                        {editUnit === "tokens" ? formatExact(parsedUsed.value) : formatCurrency(parsedUsed.value, editUnit)}
                      </span>
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
                    placeholder={editUnit === "tokens" ? t("profiles.tokenHint") : t("profiles.amountHint")}
                  />
                </div>
              </div>
            {/if}

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
          <div class="bar" class:overdraft={isOverdraft}>
            <span style:width={`${Math.min(100, pct)}%`} class:overdraft-span={isOverdraft}></span>
          </div>
          <div class="caption">
            {#if isOverdraft}
              <span class="overdraft-text">
                {t("profiles.quotaHonestOverdraft", {
                  used: formatQuotaValue(quota?.used, quota?.unit),
                  total: formatQuotaValue(quota?.total, quota?.unit),
                  pct: formatPercent(pct),
                  overdraft: formatQuotaValue((quota?.used ?? 0) - (quota?.total ?? 0), quota?.unit),
                })}
              </span>
            {:else}
              <span>
                {t("profiles.quotaHonestRemaining", {
                  used: formatQuotaValue(quota?.used, quota?.unit),
                  total: formatQuotaValue(quota?.total, quota?.unit),
                  pct: formatPercent(pct),
                  remaining: formatQuotaValue((quota?.total ?? 0) - (quota?.used ?? 0), quota?.unit),
                })}
              </span>
            {/if}
            {#if reset}
              <span>{t(reset.key, reset.params)}</span>
            {/if}
          </div>
        {:else if !unknown && quota?.used != null && (quota?.total == null || quota?.total <= 0)}
          <!-- 无上限额度展示 -->
          <div class="caption">
            <span>
              {t("profiles.quotaUnlimited", {
                used: formatQuotaValue(quota?.used, quota?.unit),
              })}
            </span>
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
    align-items: center;
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
  .tag.api {
    color: var(--accent);
    border-color: var(--accent);
    font-weight: 500;
  }
  .refresh-icon-btn {
    background: transparent;
    border: none;
    color: var(--text-2);
    cursor: pointer;
    font-size: 11px;
    padding: 0 2px;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    transition: transform 0.2s ease, color 0.15s ease;
  }
  .refresh-icon-btn:hover:not(:disabled) {
    color: var(--text-1);
  }
  .refresh-icon-btn.rotating {
    animation: spin 1s linear infinite;
    cursor: not-allowed;
    opacity: 0.7;
  }
  @keyframes spin {
    from {
      transform: rotate(0deg);
    }
    to {
      transform: rotate(360deg);
    }
  }
  .err-icon {
    color: #ef4444;
    font-weight: bold;
  }
  .inactive-icon {
    color: var(--text-2);
    opacity: 0.6;
  }
  .bar {
    height: 4px;
    background: var(--track);
    margin: 8px 0 5px;
    border-radius: 2px;
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--text-2);
  }
  /* Meter Face 诚实透支呈现：45° 斜线纹理与醒目色 */
  .bar.overdraft {
    background: repeating-linear-gradient(
      45deg,
      #dc2626,
      #dc2626 4px,
      #991b1b 4px,
      #991b1b 8px
    );
  }
  .bar.overdraft span.overdraft-span {
    background: transparent;
  }
  .overdraft-text {
    color: #ef4444 !important;
    font-weight: 500;
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
    font-size: 10px;
    padding: 1px 6px;
    cursor: pointer;
  }
  .edit-btn:hover {
    color: var(--text-1);
    border-color: var(--text-2);
  }
  .edit-form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 10px;
    padding-top: 8px;
    border-top: 1px solid var(--line);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field label {
    font-size: 10px;
    color: var(--text-2);
  }
  .field input,
  .select-input {
    background: var(--bg-1, rgba(0, 0, 0, 0.03));
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    color: var(--text-1);
    font-family: var(--font-num);
    font-size: 11px;
    padding: 4px 6px;
  }
  .field input:focus,
  .select-input:focus {
    outline: none;
    border-color: var(--accent);
  }
  .grid-2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .label-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  .preview {
    font-family: var(--font-num);
    font-size: 9.5px;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  .preview.error {
    color: #ef4444;
  }
  .preview.hint {
    color: var(--text-2);
    opacity: 0.6;
  }
  .error-msg {
    font-size: 10.5px;
    color: #ef4444;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
    margin-top: 2px;
  }
  .btn {
    border-radius: var(--radius);
    font-size: 10.5px;
    padding: 3px 10px;
    cursor: pointer;
    border: 1px solid transparent;
  }
  .btn.primary {
    background: var(--text-1);
    color: var(--bg);
  }
  .btn.primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .btn.secondary {
    background: transparent;
    border-color: var(--line-strong);
    color: var(--text-2);
  }
  .btn.secondary:hover {
    color: var(--text-1);
  }
</style>
