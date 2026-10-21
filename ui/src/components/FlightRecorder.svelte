<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { t } from "../i18n";
  import {
    formatCompact,
    formatCost,
    formatDuration,
    formatTimeOfDay,
  } from "../format";
  import type {
    UiAdapter,
    SessionFlightRecorder,
    PulsePoint,
    FlightRecordEvent,
  } from "../data";

  let {
    sessionId,
    adapter,
    onBack,
  }: {
    sessionId: string;
    adapter: UiAdapter;
    onBack: () => void;
  } = $props();

  function engineLabel(engine?: string): string {
    switch (engine) {
      case "codex": return "Codex";
      case "cline": return "Cline";
      case "roo": return "Roo";
      case "aider": return "Aider";
      case "cursor": return "Cursor";
      default: return "Claude";
    }
  }

  let recorder = $state<SessionFlightRecorder | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let selectedIndex = $state<number | null>(null);
  let hoveredPoint = $state<PulsePoint | null>(null);

  let unsubs: Array<() => void> = [];

  async function loadData() {
    try {
      const data = await adapter.getSessionFlightRecorder(sessionId);
      recorder = data;
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      onBack();
    }
  }

  function selectTurn(index: number) {
    selectedIndex = index;
    const el = document.getElementById(`turn-${index}`);
    if (el) {
      el.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }
  }

  onMount(() => {
    window.addEventListener("keydown", handleKeydown);
    void loadData();

    if (adapter.onSessionFlightUpdated) {
      const unsub = adapter.onSessionFlightUpdated((payload) => {
        if (payload.session_id === sessionId) {
          void loadData();
        }
      });
      unsubs.push(unsub);
    }
  });

  onDestroy(() => {
    window.removeEventListener("keydown", handleKeydown);
    for (const u of unsubs) u();
  });

  let maxPulseTokens = $derived(
    recorder && recorder.pulse_strip.length > 0
      ? Math.max(...recorder.pulse_strip.map((p) => p.total_tokens), 1)
      : 1,
  );
</script>

<div class="flight-recorder">
  <header class="header">
    <button type="button" class="back-btn" onclick={onBack}>
      {t("flight.back")}
    </button>
    <div class="header-right">
      {#if recorder?.is_active}
        <span class="live-badge" title="Active session">
          <span class="live-dot" aria-hidden="true"></span>
          {t("flight.live")}
        </span>
      {/if}
    </div>
  </header>

  {#if loading}
    <div class="empty-box">
      <span class="muted">Loading flight recorder...</span>
    </div>
  {:else if error}
    <div class="empty-box error">
      <span>{error}</span>
    </div>
  {:else if !recorder}
    <div class="empty-box">
      <span>{t("flight.notFound")}</span>
    </div>
  {:else}
    <!-- 会话标题与路径 -->
    <div class="session-meta">
      <div class="session-title-row">
        <h1 class="session-title">{recorder.title}</h1>
        {#if recorder.engine}
          <span class="engine-badge {recorder.engine}">
            {engineLabel(recorder.engine)}
          </span>
        {/if}
      </div>
      {#if recorder.cwd}
        <div class="session-cwd" title={recorder.cwd}>{recorder.cwd}</div>
      {/if}
    </div>

    <!-- 宏观指标卡片 -->
    <section class="overview-card">
      <div class="card-row time-row">
        <span class="time-range">
          {formatTimeOfDay(recorder.start_time_ms)} ~ {formatTimeOfDay(recorder.end_time_ms)}
        </span>
        <span class="duration-badge">
          {formatDuration(recorder.duration_seconds)}
        </span>
      </div>

      <div class="card-row metrics-grid">
        <div class="metric-item">
          <span class="metric-label">{t("breakdown.input")}</span>
          <span class="metric-val">{formatCompact(recorder.totals.input)}</span>
        </div>
        <div class="metric-item">
          <span class="metric-label">{t("breakdown.output")}</span>
          <span class="metric-val">{formatCompact(recorder.totals.output)}</span>
        </div>
        <div class="metric-item">
          <span class="metric-label">{t("breakdown.cacheRead")}</span>
          <span class="metric-val highlight">{formatCompact(recorder.totals.cache_read)}</span>
        </div>
        <div class="metric-item">
          <span class="metric-label">{t("breakdown.cacheWrite")}</span>
          <span class="metric-val">{formatCompact(recorder.totals.cache_creation)}</span>
        </div>
      </div>

      <div class="card-row summary-row">
        <div class="summary-left">
          <span class="total-requests">
            {t("flight.requests", { n: recorder.total_requests })}
          </span>
          <span class="cache-rate" title="cache_read / (input + cache_read)">
            {t("flight.cacheHitRatio", { pct: (recorder.cache_hit_ratio * 100).toFixed(1) })}
          </span>
        </div>
        <div class="summary-right">
          <span class="cost-badge" class:unpriced={!recorder.cost.is_complete} title={t("cost.estimateTooltip")}>
            {formatCost(recorder.cost.total)}{#if !recorder.cost.is_complete}*{/if}
          </span>
        </div>
      </div>

      <!-- 模型与中转档案标签 -->
      {#if recorder.models_used.length > 0}
        <div class="models-chips">
          {#each recorder.models_used as slice}
            <span class="model-chip" title="{slice.model}: {formatCompact(slice.tokens.total)} tokens">
              <span class="chip-name">{slice.model.replace("claude-", "").replace("anthropic/", "")}</span>
              <span class="chip-share">{Math.round(slice.share * 100)}%</span>
            </span>
          {/each}
        </div>
      {/if}
    </section>

    <!-- 交互式时间脉冲谱带 -->
    {#if recorder.pulse_strip.length > 0}
      <section class="pulse-section">
        <div class="section-header">
          <h2>{t("flight.pulseStrip")}</h2>
          {#if hoveredPoint}
            <span class="pulse-hint">
              #{hoveredPoint.index} · {formatCompact(hoveredPoint.total_tokens)}
              {#if hoveredPoint.is_spike}⚠{/if}
            </span>
          {/if}
        </div>

        <div class="strip-container" role="group" aria-label="Pulse Strip">
          <div class="strip-track">
            {#each recorder.pulse_strip as p}
              {@const heightPct = Math.max(10, Math.round((p.total_tokens / maxPulseTokens) * 100))}
              {@const cachePct = p.total_tokens > 0 ? Math.round((p.cache_read_tokens / p.total_tokens) * 100) : 0}
              <button
                type="button"
                class="pulse-bar"
                class:selected={selectedIndex === p.index}
                class:spike={p.is_spike}
                class:sidechain={p.is_sidechain}
                style="height: {heightPct}%"
                onclick={() => selectTurn(p.index)}
                onmouseenter={() => (hoveredPoint = p)}
                onmouseleave={() => {
                  if (hoveredPoint === p) hoveredPoint = null;
                }}
                title="#{p.index} · {formatTimeOfDay(p.timestamp_ms)} · {formatCompact(p.total_tokens)} tokens"
              >
                {#if cachePct > 0}
                  <span class="cache-fill" style="height: {cachePct}%"></span>
                {/if}
              </button>
            {/each}
          </div>
        </div>
      </section>
    {/if}

    <!-- 逐次请求事件流 -->
    <section class="events-section">
      <div class="section-header">
        <h2>{t("flight.eventStream")}</h2>
        {#if recorder.events_truncated}
          <span class="trunc-hint">
            {t("flight.truncated", { n: recorder.events.length })}
          </span>
        {/if}
      </div>

      <div class="events-list">
        {#each recorder.events as evt (evt.record_id)}
          <div
            id="turn-{evt.request_index}"
            class="event-card"
            class:selected={selectedIndex === evt.request_index}
            class:spike={evt.is_spike}
            class:sidechain={evt.is_sidechain}
          >
            <div class="event-header">
              <div class="event-id-group">
                <span class="req-index">#{evt.request_index}</span>
                <span class="req-time">{formatTimeOfDay(evt.timestamp_ms)}</span>
                {#if evt.is_sidechain}
                  <span class="badge sidechain-badge">{t("flight.sidechain")}</span>
                {/if}
              </div>

              <div class="event-cost-group">
                <span
                  class="event-cost"
                  class:unpriced={!evt.cost.is_complete}
                  title={evt.cost.is_complete ? undefined : "Unpriced tokens"}
                >
                  {#if evt.cost.is_complete}
                    {formatCost(evt.cost.total)}
                  {:else}
                    $—*
                  {/if}
                </span>
              </div>
            </div>

            <!-- 特殊状态标识条：突刺或模型切换 -->
            {#if evt.is_spike || evt.model_switched}
              <div class="event-alerts">
                {#if evt.is_spike && evt.spike_reason}
                  <span class="alert-tag spike-tag">
                    ⚠ {t("flight.spike", { r: evt.spike_reason })}
                  </span>
                {/if}
                {#if evt.model_switched}
                  <span class="alert-tag switch-tag">
                    ⇄ {t("flight.modelSwitch", { m: evt.model.replace("claude-", "") })}
                  </span>
                {/if}
              </div>
            {/if}

            <div class="event-body">
              <span class="event-model" title={evt.model}>
                {evt.model.replace("claude-", "").replace("anthropic/", "")}
              </span>
              <div class="event-tokens">
                <span title="Total tokens" class="token-total">
                  {formatCompact(evt.total_tokens)}
                </span>
                <span class="token-split">
                  (in: {formatCompact(evt.input_tokens)} · out: {formatCompact(evt.output_tokens)}
                  {#if evt.cache_read_tokens > 0}
                    · cr: {formatCompact(evt.cache_read_tokens)}
                  {/if}
                  {#if evt.cache_creation_tokens > 0}
                    · cw: {formatCompact(evt.cache_creation_tokens)}
                  {/if})
                </span>
              </div>
            </div>
          </div>
        {/each}
      </div>
    </section>
  {/if}
</div>

<style>
  .flight-recorder {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding-bottom: 24px;
    animation: fadeIn 120ms ease;
  }

  @keyframes fadeIn {
    from {
      opacity: 0.6;
      transform: translateY(2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-bottom: 4px;
  }

  .back-btn {
    border: none;
    background: var(--bg-1);
    color: var(--text);
    padding: 4px 10px;
    border-radius: 12px;
    font-size: 11.5px;
    cursor: pointer;
    transition: background 120ms ease, color 120ms ease;
  }

  .back-btn:hover {
    background: var(--bg-2, var(--border));
    color: var(--accent);
  }

  .live-badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-family: var(--font-num);
    font-size: 10px;
    letter-spacing: 0.04em;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    padding: 2px 7px;
    border-radius: 10px;
  }

  .live-dot {
    width: 5px;
    height: 5px;
    background: var(--accent);
    border-radius: 50%;
    animation: pulseGlow 1.5s infinite ease-in-out;
  }

  @keyframes pulseGlow {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.4; transform: scale(0.85); }
  }

  .session-meta {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .session-title-row {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }

  .engine-badge {
    font-family: var(--font-num);
    font-size: 9px;
    line-height: 1.1;
    padding: 1.5px 5px;
    border-radius: var(--radius);
    border: 1px solid var(--line);
    color: var(--text-2);
    background: var(--bg-1);
    letter-spacing: 0.3px;
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

  .session-title {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .session-cwd {
    font-size: 11px;
    color: var(--text-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .overview-card {
    background: var(--bg-1);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 9px;
  }

  .card-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .time-row {
    font-size: 11.5px;
    color: var(--text-2);
  }

  .duration-badge {
    font-family: var(--font-num);
    background: var(--bg);
    border: 1px solid var(--border);
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 10.5px;
    color: var(--text);
  }

  .metrics-grid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 4px;
    padding: 6px 0;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }

  .metric-item {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .metric-label {
    font-size: 10px;
    color: var(--text-2);
  }

  .metric-val {
    font-family: var(--font-num);
    font-size: 12px;
    font-weight: 500;
    color: var(--text);
  }

  .metric-val.highlight {
    color: var(--accent);
  }

  .summary-row {
    font-size: 11px;
    color: var(--text-2);
  }

  .summary-left {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .cost-badge {
    font-family: var(--font-num);
    font-size: 13px;
    font-weight: 600;
    color: var(--accent);
  }

  .cost-badge.unpriced {
    color: var(--amber);
  }

  .models-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 2px;
  }

  .model-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 10px;
    background: var(--bg);
    border: 1px solid var(--border);
    padding: 2px 6px;
    border-radius: 4px;
  }

  .chip-name {
    color: var(--text);
  }

  .chip-share {
    font-family: var(--font-num);
    color: var(--text-2);
  }

  /* 脉冲谱带 */
  .pulse-section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .section-header h2 {
    margin: 0;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-2);
  }

  .pulse-hint, .trunc-hint {
    font-size: 10.5px;
    font-family: var(--font-num);
    color: var(--accent);
  }

  .strip-container {
    background: var(--bg-1);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
    height: 72px;
    overflow-x: auto;
    overflow-y: hidden;
  }

  .strip-track {
    display: flex;
    align-items: flex-end;
    gap: 3px;
    height: 100%;
    min-width: 100%;
  }

  .pulse-bar {
    position: relative;
    flex: 1 0 4px;
    max-width: 16px;
    min-width: 3px;
    border: none;
    background: color-mix(in srgb, var(--accent) 30%, transparent);
    border-radius: 2px 2px 0 0;
    padding: 0;
    cursor: pointer;
    transition: background 100ms ease;
  }

  .pulse-bar:hover,
  .pulse-bar.selected {
    background: var(--accent);
  }

  .pulse-bar.spike {
    background: color-mix(in srgb, var(--amber) 80%, transparent);
  }

  .pulse-bar.spike:hover,
  .pulse-bar.spike.selected {
    background: var(--amber);
  }

  .pulse-bar.sidechain {
    opacity: 0.55;
    border: 1px dashed var(--border);
  }

  .cache-fill {
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    background: color-mix(in srgb, var(--accent) 70%, white);
    opacity: 0.6;
    border-radius: 0 0 2px 2px;
  }

  /* 事件流 */
  .events-section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .events-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .event-card {
    background: var(--bg-1);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 7px 9px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    transition: border-color 120ms ease, background 120ms ease;
  }

  .event-card:hover {
    background: color-mix(in srgb, var(--bg-1) 85%, var(--accent));
  }

  .event-card.selected {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-1));
  }

  .event-card.spike {
    border-left: 3px solid var(--amber);
  }

  .event-card.sidechain {
    opacity: 0.85;
    border-style: dashed;
  }

  .event-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .event-id-group {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .req-index {
    font-family: var(--font-num);
    font-size: 11px;
    font-weight: 600;
    color: var(--text);
  }

  .req-time {
    font-size: 10.5px;
    color: var(--text-2);
  }

  .badge {
    font-size: 9.5px;
    padding: 1px 4px;
    border-radius: 3px;
  }

  .sidechain-badge {
    background: var(--bg);
    color: var(--text-2);
    border: 1px solid var(--border);
  }

  .event-cost {
    font-family: var(--font-num);
    font-size: 11.5px;
    font-weight: 500;
    color: var(--accent);
  }

  .event-alerts {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin: 1px 0;
  }

  .alert-tag {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 3px;
  }

  .spike-tag {
    background: color-mix(in srgb, var(--amber) 15%, transparent);
    color: var(--amber);
    font-weight: 500;
  }

  .switch-tag {
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    color: var(--accent);
  }

  .event-body {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 11px;
  }

  .event-model {
    color: var(--text);
    font-weight: 500;
  }

  .event-tokens {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }

  .token-total {
    font-family: var(--font-num);
    font-weight: 600;
    color: var(--text);
  }

  .token-split {
    font-family: var(--font-num);
    font-size: 10px;
    color: var(--text-2);
  }

  .empty-box {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 120px;
    font-size: 12px;
    color: var(--text-2);
  }

  .empty-box.error {
    color: #ef4444;
  }
</style>
