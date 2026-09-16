export type ProviderId = "claude" | "codex";
export type ProviderSelection = ProviderId | "both";
export type Theme = "system" | "light" | "dark";
export type PercentageMode = "remaining" | "used";
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
  tracked_limit: string;
  threshold: number;
  refresh_seconds: number;
  theme: Theme;
  percentage_mode: PercentageMode;
  claude_windows: string[];
  codex_windows: string[];
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
