import type { IssueKind, ProviderId, ProviderState, RecoveryPhase, Settings } from "./types";

export const providerNames: Record<ProviderId, string> = { claude: "Claude", codex: "Codex" };
export const clientNames: Record<ProviderId, string> = { claude: "Claude Code", codex: "Codex" };

export const issueStatuses: Record<IssueKind, string> = {
  sign_in: "Sign in needed", authentication: "Sign in needed", recovery: "Sign in needed",
  credential_access: "Access needed", configuration: "Setup needed", client_missing: "Setup needed",
  access_denied: "Access denied", network: "Unavailable", rate_limited: "Waiting",
  service: "Unavailable", response: "Unavailable",
};
export const recoveryMessages: Record<RecoveryPhase, string> = {
  renewing: "Reconnecting", signing_in: "Finish signing in in your browser",
  checking: "Checking usage", cancelling: "Cancelling",
};

export function providerEnabled(settings: Settings, provider: ProviderId): boolean {
  return provider === "claude" ? settings.claude_enabled : settings.codex_enabled;
}

export function signInBlocked(provider: ProviderState, now: number): boolean {
  const kind = provider.error?.kind;
  const signInIssue = kind === "authentication" || kind === "sign_in" || kind === "recovery" || kind === "client_missing";
  return provider.refreshing || provider.recovery !== null
    || (kind !== undefined && !signInIssue)
    || (provider.next_retry_at !== null && provider.next_retry_at > now && !signInIssue);
}
