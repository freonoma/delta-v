export type ProviderId = "claude" | "codex";
export type ProviderSelection = ProviderId | "both";
export type Theme = "system" | "light" | "dark";
export type PercentageMode = "remaining" | "used";
export type MiniLayout = "columns" | "stacked";
export type HistoryRetention = "forever" | "days30" | "days90";

export interface HistoryState {
  recording: boolean;
  retention: HistoryRetention;
  info: {
    bytes: number;
    records: number;
    last_recorded_at: number | null;
  };
  error: string | null;
  provider_issues: { provider: ProviderId; message: string }[];
}

export interface HistoryLimitKey {
  id: string;
  window_seconds: number | null;
  provenance: Limit["provenance"];
}

export interface HistoryWindow {
  key: HistoryLimitKey;
  label: string;
  first_recorded_at: number;
  last_recorded_at: number;
}

export interface HistoryAccount {
  provider: ProviderId;
  account_key: string;
  first_recorded_at: number;
  last_recorded_at: number;
  windows: HistoryWindow[];
}

export interface HistoryCatalog {
  accounts: HistoryAccount[];
  current_accounts: { provider: ProviderId; account_key: string | null; verified_at: number | null }[];
}

export type HistoryRange = { kind: "today" | "days7" | "days30" | "all_time" } | { kind: "day"; date: string };
export interface HistoryRequest {
  provider: ProviderId;
  account_key: string;
  limit: HistoryLimitKey;
  range: HistoryRange;
}

export type HistoryBreak = "missing_time" | "reset_changed" | "reset_boundary" | "usage_decreased" | "limit_unavailable";
export interface HistoryPoint {
  observed_at: number;
  used_fraction: number;
  resets_at: number | null;
  break_before: HistoryBreak[];
}

export interface HistoryDay {
  date: string;
  starts_at: number;
  ends_at: number;
  sample_count: number;
  peak: HistoryPoint | null;
}

export interface HistoryQuery {
  request: HistoryRequest;
  timezone: string;
  from: number;
  until: number;
  days: HistoryDay[];
  points: HistoryPoint[];
  observations: {
    peak: HistoryPoint | null;
    days_with_readings: number;
    days_below_threshold: number;
    days_in_range: number;
    threshold_remaining: number;
  };
}

export interface PanelPreferences {
  pinned: boolean;
  mini: boolean;
  expanded: boolean;
  layout: MiniLayout;
}

export interface PanelPreferencesState {
  preferences: PanelPreferences;
  error: string | null;
}
export type IssueKind = "sign_in" | "authentication" | "credential_access" | "configuration"
  | "access_denied" | "network" | "rate_limited" | "service" | "response" | "client_missing" | "recovery";
export type RecoveryPhase = "renewing" | "signing_in" | "checking" | "cancelling";
export type ReconnectAction = "renew" | "sign_in";

export interface ProviderIssue {
  kind: IssueKind;
  message: string;
}

export interface Settings {
  providers: ProviderSelection;
  claude_enabled: boolean;
  codex_enabled: boolean;
  launch_at_login_prompt_dismissed: boolean;
  history_recording: boolean;
  history_retention: HistoryRetention;
  tracked_limit: string;
  threshold: number;
  refresh_seconds: number;
  theme: Theme;
  percentage_mode: PercentageMode;
  claude_windows: string[];
  codex_windows: string[];
}

export interface LoginItemState {
  status: "disabled" | "enabled" | "approval_required" | "unavailable";
  reason: string | null;
}

export interface Amount {
  used: string | null;
  limit: string | null;
  balance: string | null;
  currency: string | null;
}

export interface Limit {
  id: string;
  label: string;
  kind: "quota" | "spend" | "credits" | "unknown";
  used_fraction: number | null;
  resets_at: number | null;
  window_seconds: number | null;
  provenance: "official" | "local_estimate" | "unknown";
  enabled: boolean;
  amount: Amount | null;
  detail: string | null;
}

export interface ProviderSnapshot {
  provider: ProviderId;
  limits: Limit[];
  plan: string | null;
  fetched_at: number;
  warnings: string[];
}

export interface ProviderState {
  id: ProviderId;
  snapshot: ProviderSnapshot | null;
  error: ProviderIssue | null;
  recovery: RecoveryPhase | null;
  refreshing: boolean;
  stale: boolean;
  next_retry_at: number | null;
}

export interface AppState {
  settings: Settings;
  providers: ProviderState[];
  settings_error: string | null;
  paused: boolean;
}
