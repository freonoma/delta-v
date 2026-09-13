import type { AppState, Limit, ProviderId } from "./types";

export function createPreview(): AppState {
  const now = Math.floor(Date.now() / 1000);
  const quota = (id: string, label: string, used: number, duration: number, reset: number): Limit => ({
    id, label, kind: "quota", used_fraction: used, window_seconds: duration,
    resets_at: now + reset, provenance: "official", enabled: true, amount: null, detail: null,
  });
  const provider = (id: ProviderId, limits: Limit[], plan: string) => ({
    id, snapshot: { provider: id, limits, plan, fetched_at: now - 24, warnings: [] },
    error: null, refreshing: false, stale: false, next_retry_at: null,
  });

  return {
    settings: { providers: "both", tracked_limit: "auto", threshold: 20, refresh_seconds: 60, theme: "system", percentage_mode: "remaining", claude_windows: [], codex_windows: [] },
    providers: [
      provider("claude", [
        quota("session", "5-hour", 0.36, 18000, 8120),
        quota("weekly", "Weekly", 0.62, 604800, 270120),
        quota("seven_day_sonnet", "Weekly · Sonnet", 0.21, 604800, 270120),
        { id: "unrecognized_field", label: "unrecognized_field", kind: "unknown", used_fraction: null,
          window_seconds: null, resets_at: null, provenance: "unknown", enabled: false, amount: null, detail: null },
      ], "Max"),
      provider("codex", [
        quota("rate_limit:primary_window", "Weekly", 0.84, 604800, 138600),
        quota("spark:primary", "Spark · 5 hours", 0.12, 18000, 10830),
        quota("spark:secondary", "Spark · Weekly", 0.27, 604800, 398430),
      ], "Pro"),
    ],
    settings_error: null,
    paused: false,
  };
}
