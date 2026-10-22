// T6 —— 前端数据契约类型，字段名与 docs/contracts/ui-data-contract.md 一致。

export type Range = "today" | "week" | "month" | "all";

export interface UpdateInfo {
  has_update: boolean;
  current_version: string;
  latest_version: string;
  release_url: string;
}

export type HudMode = "widget" | "capsule";

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
  engine?: string;
}

export interface PulsePoint {
  index: number;
  timestamp_ms: number;
  total_tokens: number;
  cache_read_tokens: number;
  is_sidechain: boolean;
  is_spike: boolean;
}

export interface FlightRecordEvent {
  request_index: number;
  record_id: string;
  timestamp_ms: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  total_tokens: number;
  model: string;
  entrypoint: string;
  is_sidechain: boolean;
  profile_id: number | null;
  attribution_source: string;
  cost: TokenCost;
  model_switched: boolean;
  profile_switched: boolean;
  is_spike: boolean;
  spike_reason: string | null;
}

export interface SessionModelSlice {
  model: string;
  tokens: TokenTotals;
  cost: TokenCost;
  share: number;
}

export interface SessionProfileSlice {
  profile_id: number | null;
  display_name: string;
  tokens: TokenTotals;
  cost: TokenCost;
}

export interface SessionFlightRecorder {
  session_id: string;
  title: string;
  cwd: string | null;
  cc_version: string | null;
  engine?: string;
  start_time_ms: number;
  duration_seconds: number;
  is_active: boolean;
  total_requests: number;
  totals: TokenTotals;
  cost: TokenCost;
  cache_hit_ratio: number;
  models_used: SessionModelSlice[];
  profiles_used: SessionProfileSlice[];
  pulse_strip: PulsePoint[];
  events: FlightRecordEvent[];
  events_truncated: boolean;
}

export interface EngineSlice {
  engine: string;
  tokens: number;
}

export interface Dashboard {
  range: Range;
  engine?: string | null;
  totals: TokenTotals;
  cost: TokenCost;
  /** 固定最近 1 小时（与 range 无关） */
  pulses: UsagePulse[];
  models: ModelRow[];
  profiles: ProfileCard[];
  sessions: SessionRow[];
  available_engines?: EngineSlice[];
  thinking_share?: number | null;
  thoughts_tokens?: number;
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

export interface ActiveSessionItem {
  session_id: string;
  model: string;
  session_total: number;
  output_tokens: number;
  current_rate: number;
  last_active_ts: number;
}

export interface HudSummary {
  total_today: number;
  rate_per_min: number;
  current_rate: number;
  session_total: number;
  current_model: string;
  cache_rate: number;
  input_tokens: number;
  output_tokens: number;
  hud_metrics: string[];
  hud_locked: boolean;
  active_sessions?: ActiveSessionItem[];
  capsule_opacity?: number;
  widget_opacity?: number;
  hud_active_sessions_count?: number;
}

export type Unsubscribe = () => void;

export interface UiAdapter {
  getDashboard(range: Range, engine?: string | null): Promise<Dashboard>;
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
  moveHudBy(dx: number, dy: number): Promise<void>;
  saveCurrentHudPosition(): Promise<void>;
  getTheme(): Promise<string>;
  setTheme(theme: string): Promise<void>;
  onThemeChanged(cb: (theme: string) => void): Unsubscribe;
  getSessionFlightRecorder(sessionId: string, limit?: number): Promise<SessionFlightRecorder | null>;
  onSessionFlightUpdated?: (cb: (payload: { session_id: string }) => void) => Unsubscribe;
  showHudContextMenu(): Promise<void>;
  toggleHudMetric(metric: string): Promise<string[]>;
    resizeHud(width: number, height?: number): Promise<void>;
    checkUpdate(): Promise<UpdateInfo>;
    openExternalUrl(url: string): Promise<void>;
    getHudMode(): Promise<HudMode>;
    setHudMode(mode: HudMode): Promise<void>;
    onHudModeChanged?: (cb: (mode: HudMode) => void) => Unsubscribe;
    onHudUpdated?: (cb: () => void) => Unsubscribe;

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
