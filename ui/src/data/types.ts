// T6 —— 前端数据契约类型，字段名与 docs/contracts/ui-data-contract.md 一致。

export type Range = "today" | "week" | "month" | "all";

export interface TokenTotals {
  input: number;
  output: number;
  cache_read: number;
  cache_creation: number;
  total: number;
}

export interface UsagePulse {
  t_ms: number;
  input: number;
  output: number;
  cache_read: number;
  cache_creation: number;
}
export interface TokenCost {
  total: number;
  coverage: number;
  is_complete: boolean;
  unpriced_tokens: number;
}

export interface ModelRow {
  model: string;
  tokens: number;
  cost?: number | null;
}

export interface Quota {
  quota_type?: 'manual' | 'api';
  unit?: 'usd' | 'tokens' | 'cny';
  total: number | null;
  used: number | null;
  reset_date: string | null;
  updated_at?: number | null;
  sync_status?: 'ok' | 'error' | 'inactive' | null;
  sync_error?: string | null;
}

export interface ProfileCard {
  /** null = Unknown 虚拟卡（置灰、无配额条） */
  profile_id: number | null;
  display_name: string;
  endpoint: string | null;
  /** v0.1 恒 true：卡面显示「推断 · 低置信」 */
  inferred: boolean;
  tokens: TokenTotals;
  quota: Quota | null;
  cost?: TokenCost | null;
}

export interface SessionRow {
  session_id: string;
  title: string;
  last_active_ms: number;
  total: number;
  active: boolean;
}

export interface Dashboard {
  range: Range;
  totals: TokenTotals;
  cost: TokenCost;
  /** 固定最近 1 小时（与 range 无关） */
  pulses: UsagePulse[];
  models: ModelRow[];
  profiles: ProfileCard[];
  sessions: SessionRow[];
}

export interface ScanStatus {
  phase: "scanning" | "done" | "empty";
  files_done: number;
  files_total: number;
  percent: number;
  /** 与 data-health 同源快照：启动时用于初始化三档错误态。 */
  root_unreadable: boolean;
  skipped_files: number;
  db_readonly: boolean;
}

export interface DataHealth {
  root_unreadable: boolean;
  skipped_files: number;
  db_readonly: boolean;
}

export interface ProxyStatus {
  running: boolean;
  port: number;
  upstream: string;
  is_injected: boolean;
  export_cmd: string;
}

export interface HudSummary {
  total_today: number;
  rate_per_min: number;
  hud_locked: boolean;
}

export type Unsubscribe = () => void;

export interface UiAdapter {
  getDashboard(range: Range): Promise<Dashboard>;
  getScanStatus(): Promise<ScanStatus>;
  updateProfileDisplay(profileId: number, displayName: string): Promise<void>;
  updateProfileQuota(
    profileId: number,
    quota: { total: number | null; used: number | null; reset_date: string | null },
  ): Promise<void>;
  updateProfileQuotaConfig(
    profileId: number,
    config: {
      quota_type: string;
      unit: string;
      api_url?: string | null;
      api_key?: string | null;
      manual_total?: number | null;
      manual_used?: number | null;
      reset_date?: string | null;
    },
  ): Promise<void>;
  syncProfileQuota(profileId: number): Promise<Quota>;
  reassignUsage(
    profileId: number | null,
    startMs: number,
    endMs: number,
  ): Promise<void>;
  getProxyStatus(): Promise<ProxyStatus>;
  injectProxySettings(): Promise<ProxyStatus>;
  restoreProxySettings(): Promise<ProxyStatus>;
  getHudSummary(): Promise<HudSummary>;
  setHudVisible(visible: boolean): Promise<void>;
  setHudLocked(locked: boolean): Promise<void>;
  saveHudPosition(x: number, y: number): Promise<void>;
  showMainWindow(): Promise<void>;

  onPulse(cb: (pulse: UsagePulse) => void): Unsubscribe;
  onScanProgress(cb: (status: ScanStatus) => void): Unsubscribe;
  onDataHealth(cb: (health: DataHealth) => void): Unsubscribe;
  onDashboardDirty(cb: () => void): Unsubscribe;
  onPricingUpdated?: (cb: () => void) => Unsubscribe;
  /**
   * 窗口可见性通知。Tauri 侧 Rust 自己监听窗口事件暂停 emit（契约未定义
   * 对应 command），真实 adapter 为空实现；mock 用它停/启脉冲定时器。
   */
  setVisible(visible: boolean): void;
}
