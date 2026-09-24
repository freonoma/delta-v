import type { AppState, Limit, LoginItemState, ProviderId, ProviderIssue, ProviderState } from "./types";

export function createLoginItemPreview(search: string): LoginItemState {
  const status = new URLSearchParams(search).get("startup");
  if (status === "enabled" || status === "approval_required") return { status, reason: null };
  if (status === "unavailable") return { status, reason: "Move Delta-V to Applications and open it from there to use launch at login." };
  return { status: "disabled", reason: null };
}

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
  { kind: "client_missing", message: "The required sign-in app is missing or needs an update." },
  { kind: "recovery", message: "The CLI could not finish reconnecting." },
];

export function createPreview(search = ""): AppState {
  const now = Math.floor(Date.now() / 1000);
  const parameters = new URLSearchParams(search);
  const issue = previewIssues.find((candidate) => candidate.kind === parameters.get("issue"));
  const issueProvider = parameters.get("provider") ?? "claude";
  const disconnected = parameters.get("disconnected");
  const stale = parameters.get("stale");
  const codexFiveHour = parameters.get("codex_5h") === "1";
  const quota = (id: string, label: string, used: number, duration: number, reset: number): Limit => ({
    id, label, kind: "quota", used_fraction: used, window_seconds: duration,
    resets_at: now + reset, provenance: "official", enabled: true, amount: null, detail: null,
  });
  const provider = (id: ProviderId, limits: Limit[], plan: string): ProviderState => {
    const enabled = disconnected !== id && disconnected !== "both";
    const cached = enabled && (stale === id || stale === "both");
    const error = enabled && (id === issueProvider || issueProvider === "both") && issue
      ? issue : cached ? previewIssues.find((candidate) => candidate.kind === "network") ?? null : null;
    return {
      id, snapshot: !enabled || (error && !cached) ? null : { provider: id, limits, plan, fetched_at: now - (cached ? 480 : 24), warnings: [] },
      error, recovery: null, refreshing: false, stale: error !== null,
      next_retry_at: error?.kind === "rate_limited" ? now + 90 : null,
    };
  };

  return {
    settings: { providers: "both", claude_enabled: disconnected !== "claude" && disconnected !== "both", codex_enabled: disconnected !== "codex" && disconnected !== "both", launch_at_login_prompt_dismissed: parameters.get("first_run") !== "1", tracked_limit: "auto", threshold: 20, refresh_seconds: 60, theme: "system", percentage_mode: "remaining", claude_windows: parameters.get("missing_window") === "1" ? ["missing", "weekly"] : [], codex_windows: [] },
    providers: [
      provider("claude", [
        quota("session", "5-hour", 0.36, 18000, parameters.get("expired") === "1" ? -60 : 8120),
        quota("weekly", "Weekly", parameters.get("hidden_low") === "1" ? 0.92 : 0.62, 604800, 270120),
        quota("seven_day_sonnet", "Weekly · Sonnet", 0.21, 604800, 270120),
        { id: "unrecognized_field", label: "unrecognized_field", kind: "unknown", used_fraction: null,
          window_seconds: null, resets_at: null, provenance: "unknown", enabled: false, amount: null, detail: null },
      ], "Max"),
      provider("codex", [
        quota("rate_limit:primary_window", codexFiveHour ? "5-hour" : "Weekly", codexFiveHour ? 0.12 : 0.84, codexFiveHour ? 18000 : 604800, codexFiveHour ? 10830 : 138600),
        ...(codexFiveHour ? [quota("rate_limit:secondary_window", "Weekly", 0.84, 604800, 138600)] : []),
        quota("spark:primary", "Spark · 5 hours", 0.12, 18000, 10830),
        quota("spark:secondary", "Spark · Weekly", 0.27, 604800, 398430),
      ], "Pro"),
    ],
    settings_error: null,
    paused: false,
  };
}
