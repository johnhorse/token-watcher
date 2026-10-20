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
    getDashboard: (range: Range) =>
      invoke<Dashboard>("get_dashboard", { range }),

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
