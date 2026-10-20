<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n";
  import type { ProxyStatus, UiAdapter } from "../data";

  let { adapter }: { adapter: UiAdapter } = $props();

  let proxy = $state<ProxyStatus>({
    running: false,
    port: 4040,
    upstream: "",
    is_injected: false,
    export_cmd: "export ANTHROPIC_BASE_URL=http://127.0.0.1:4040",
  });

  let copied = $state(false);
  let actionLoading = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  async function loadStatus() {
    try {
      proxy = await adapter.getProxyStatus();
    } catch (err) {
      console.warn("Failed to get proxy status", err);
    }
  }

  onMount(() => {
    loadStatus();
  });

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(proxy.export_cmd);
      copied = true;
      if (copyTimer) clearTimeout(copyTimer);
      copyTimer = setTimeout(() => {
        copied = false;
        copyTimer = undefined;
      }, 2000);
    } catch (err) {
      console.warn("Copy to clipboard failed", err);
    }
  }

  async function toggleInjection() {
    if (actionLoading) return;
    actionLoading = true;
    try {
      if (proxy.is_injected) {
        proxy = await adapter.restoreProxySettings();
      } else {
        proxy = await adapter.injectProxySettings();
      }
    } catch (err) {
      console.error("Proxy injection toggle failed", err);
    } finally {
      actionLoading = false;
    }
  }
</script>

<div class="proxy-bar">
  <div class="header-row">
    <div class="status-left">
      <span class="indicator" class:running={proxy.running}></span>
      <span class="title">
        {t("proxy.title")}
        <span class="port-num">127.0.0.1:{proxy.port}</span>
      </span>
      {#if proxy.is_injected}
        <span class="injected-tag">{t("proxy.injectedBadge")}</span>
      {/if}
    </div>
    <div class="actions">
      <button class="action-btn" onclick={handleCopy} title={proxy.export_cmd}>
        {copied ? t("proxy.copied") : t("proxy.copyCmd")}
      </button>
      <button
        class="action-btn"
        class:highlight={!proxy.is_injected}
        onclick={toggleInjection}
        disabled={actionLoading}
      >
        {proxy.is_injected ? t("proxy.restore") : t("proxy.inject")}
      </button>
    </div>
  </div>
  {#if proxy.upstream}
    <div class="meta-row">
      <span class="upstream-label">{t("proxy.upstream", { url: proxy.upstream })}</span>
    </div>
  {/if}
</div>

<style>
  .proxy-bar {
    background: var(--bg-1);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 8px 12px;
    margin: 12px 0;
    box-sizing: border-box;
  }
  .header-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .status-left {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .indicator {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text-2);
    flex-shrink: 0;
  }
  .indicator.running {
    background: #10b981; /* green pulse */
    box-shadow: 0 0 6px rgba(16, 185, 129, 0.4);
  }
  .title {
    font-size: 11.5px;
    font-weight: 500;
    color: var(--text-1);
    letter-spacing: 0.02em;
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .port-num {
    font-family: var(--font-num);
    font-size: 10.5px;
    color: var(--text-2);
    letter-spacing: 0;
  }
  .injected-tag {
    font-size: 9px;
    padding: 1px 4px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 30%, transparent);
    font-weight: 500;
    white-space: nowrap;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }
  .action-btn {
    appearance: none;
    background: var(--bg-2);
    border: 1px solid var(--line-strong);
    color: var(--text-2);
    font-size: 10px;
    padding: 3px 8px;
    border-radius: var(--radius);
    cursor: pointer;
    white-space: nowrap;
    transition: all 0.15s ease;
  }
  .action-btn:hover {
    color: var(--text-1);
    border-color: var(--text-2);
  }
  .action-btn.highlight {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }
  .action-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .meta-row {
    margin-top: 4px;
    font-size: 10px;
    color: var(--text-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .upstream-label {
    opacity: 0.85;
  }
</style>
