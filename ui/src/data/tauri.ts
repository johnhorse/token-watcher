// T6 —— Tauri adapter：按 docs/contracts/ui-data-contract.md 直连 Rust command/event。
// 接线时零改动：App 只依赖 data/index.ts 导出的 UiAdapter。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Dashboard,
  DataHealth,
  Range,
  ScanStatus,
  UiAdapter,
  Unsubscribe,
  UsagePulse,
  ProxyStatus,
  HudSummary,
  Quota,
  SessionFlightRecorder,
} from "./types";

function subscribe<T>(event: string, cb: (payload: T) => void): Unsubscribe {
  let unlisten: UnlistenFn | undefined;
  let cancelled = false;
  void listen<T>(event, (e) => cb(e.payload)).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

export function createTauriAdapter(): UiAdapter {
  return {
    getDashboard: (range: Range, engine?: string | null) =>
      invoke<Dashboard>("get_dashboard", { range, engine: engine || null }),

    getScanStatus: () => invoke<ScanStatus>("get_scan_status"),

    updateProfileDisplay: (profileId, displayName) =>
      invoke<void>("update_profile_display", { profileId, displayName }),

    updateProfileQuota: (profileId, quota) =>
      invoke<void>("update_profile_quota", {
        profileId,
        total: quota.total,
        used: quota.used,
        resetDate: quota.reset_date,
      }),

    updateProfileQuotaConfig: (profileId, config) =>
      invoke<void>("update_profile_quota_config", {
        profileId,
        quotaType: config.quota_type,
        unit: config.unit,
        apiUrl: config.api_url ?? null,
        apiKey: config.api_key ?? null,
        manualTotal: config.manual_total ?? null,
        manualUsed: config.manual_used ?? null,
        resetDate: config.reset_date ?? null,
      }),

    syncProfileQuota: (profileId) =>
      invoke<Quota>("sync_profile_quota", { profileId }),
    reassignUsage: (profileId, startMs, endMs) =>
      invoke<void>("reassign_usage", { profileId, startMs, endMs }),
    getProxyStatus: () => invoke<ProxyStatus>("get_proxy_status"),
    injectProxySettings: () => invoke<ProxyStatus>("inject_proxy_settings"),
    restoreProxySettings: () => invoke<ProxyStatus>("restore_proxy_settings"),
    getHudSummary: () => invoke<HudSummary>("get_hud_summary"),
    setHudVisible: (visible) => invoke<void>("set_hud_visible", { visible }),
    setHudLocked: (locked) => invoke<void>("set_hud_locked", { locked }),
    saveHudPosition: (x, y) => invoke<void>("save_hud_position", { x, y }),
    showMainWindow: () => invoke<void>("show_main_window"),
    moveHudBy: (dx, dy) => invoke<void>("move_hud_by", { dx, dy }),
    saveCurrentHudPosition: () => invoke<void>("save_current_hud_position"),
    getTheme: () => invoke<string>("get_theme"),
    setTheme: (theme: string) => invoke<void>("set_theme", { theme }),
    onThemeChanged: (cb) => subscribe<string>("theme-changed", cb),
    getSessionFlightRecorder: (sessionId: string, limit?: number) =>
      invoke<SessionFlightRecorder | null>("get_session_flight_recorder", {
        sessionId,
        limit: limit ?? null,
      }),
    onSessionFlightUpdated: (cb) => subscribe<{ session_id: string }>("session-flight-updated", cb),
    showHudContextMenu: () => invoke<void>("show_hud_context_menu"),
    toggleHudMetric: (metric: string) => invoke<string[]>("toggle_hud_metric", { metric }),
    resizeHud: (width: number) => invoke<void>("resize_hud", { width }),
    onHudUpdated: (cb) => subscribe<void>("hud-updated", () => cb()),


    onPulse: (cb) => subscribe<UsagePulse>("usage-pulse", cb),
    onScanProgress: (cb) => subscribe<ScanStatus>("scan-progress", cb),
    onDataHealth: (cb) => subscribe<DataHealth>("data-health", cb),
    onDashboardDirty: (cb) => subscribe<null>("dashboard-dirty", () => cb()),
    onPricingUpdated: (cb) => subscribe<void>("pricing-updated", () => cb()),

    setVisible() {
      // Rust 侧监听窗口可见性并自行暂停/恢复 emit（契约未定义对应 command）。
    },
  };
}
