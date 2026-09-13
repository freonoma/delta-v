export type ProviderId = "claude" | "codex";
export type ProviderSelection = ProviderId | "both";
export type Theme = "system" | "light" | "dark";
export type PercentageMode = "remaining" | "used";

export interface Settings {
  providers: ProviderSelection;
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
  error: string | null;
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
