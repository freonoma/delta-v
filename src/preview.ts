import type { AppState, Limit, ProviderId, ProviderIssue, ProviderState } from "./types";

const previewIssues: ProviderIssue[] = [
  { kind: "sign_in", message: "No saved CLI sign-in was found." },
  { kind: "authentication", message: "Your saved CLI sign-in is no longer accepted." },
  { kind: "credential_access", message: "Could not read the CLI credential store." },
  { kind: "configuration", message: "The CLI credential storage setting is unsupported." },
  { kind: "access_denied", message: "The usage service denied access to this account." },
  { kind: "network", message: "Could not reach the usage service." },
  { kind: "rate_limited", message: "The usage service asked us to wait before checking again." },
  { kind: "service", message: "The usage service returned an error (503)." },
  { kind: "response", message: "The usage response could not be read." },
  { kind: "client_missing", message: "The official CLI could not be found on this Mac." },
  { kind: "recovery", message: "The CLI could not finish reconnecting." },
];

export function createPreview(search = ""): AppState {
  const now = Math.floor(Date.now() / 1000);
  const parameters = new URLSearchParams(search);
  const issue = previewIssues.find((candidate) => candidate.kind === parameters.get("issue"));
  const issueProvider: ProviderId = parameters.get("provider") === "codex" ? "codex" : "claude";
  const disconnected = parameters.get("disconnected");
  const quota = (id: string, label: string, used: number, duration: number, reset: number): Limit => ({
    id, label, kind: "quota", used_fraction: used, window_seconds: duration,
    resets_at: now + reset, provenance: "official", enabled: true, amount: null, detail: null,
  });
  const provider = (id: ProviderId, limits: Limit[], plan: string): ProviderState => {
    const enabled = disconnected !== id && disconnected !== "both";
    const error = enabled && id === issueProvider ? issue ?? null : null;
    return {
      id, snapshot: error || !enabled ? null : { provider: id, limits, plan, fetched_at: now - 24, warnings: [] },
      error, recovery: null, refreshing: false, stale: error !== null,
      next_retry_at: error?.kind === "rate_limited" ? now + 90 : null,
    };
  };

  return {
    settings: { providers: "both", claude_enabled: disconnected !== "claude" && disconnected !== "both", codex_enabled: disconnected !== "codex" && disconnected !== "both", tracked_limit: "auto", threshold: 20, refresh_seconds: 60, theme: "system", percentage_mode: "remaining", claude_windows: [], codex_windows: [] },
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
