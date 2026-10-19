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

export interface ModelRow {
  model: string;
  tokens: number;
}

export interface Quota {
  total: number | null;
  used: number | null;
  reset_date: string | null;
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

export type Unsubscribe = () => void;

export interface UiAdapter {
  getDashboard(range: Range): Promise<Dashboard>;
  getScanStatus(): Promise<ScanStatus>;
  updateProfileDisplay(profileId: number, displayName: string): Promise<void>;
  updateProfileQuota(
    profileId: number,
    quota: { total: number | null; used: number | null; reset_date: string | null },
  ): Promise<void>;
  reassignUsage(
    profileId: number | null,
    startMs: number,
    endMs: number,
  ): Promise<void>;

  onPulse(cb: (pulse: UsagePulse) => void): Unsubscribe;
  onScanProgress(cb: (status: ScanStatus) => void): Unsubscribe;
  onDataHealth(cb: (health: DataHealth) => void): Unsubscribe;
  onDashboardDirty(cb: () => void): Unsubscribe;

  /**
   * 窗口可见性通知。Tauri 侧 Rust 自己监听窗口事件暂停 emit（契约未定义
   * 对应 command），真实 adapter 为空实现；mock 用它停/启脉冲定时器。
   */
  setVisible(visible: boolean): void;
}
