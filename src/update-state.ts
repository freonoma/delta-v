export type UpdateError =
  | { kind: "network" }
  | { kind: "invalid_response" }
  | { kind: "service"; status: number; retry_at: number | null }
  | { kind: "rate_limited"; retry_at: number }
  | { kind: "interrupted" }
  | { kind: "state_unavailable" };

export type CheckedRelease = {
  installed_version: string;
  latest_version: string;
  status: "available" | "current" | "ahead";
  checked_at: number;
};

export type UpdateState = {
  checking: boolean;
  last_success: CheckedRelease | null;
  error: UpdateError | null;
  next_check_at: number | null;
};

export function emptyUpdateState(): UpdateState {
  return { checking: false, last_success: null, error: null, next_check_at: null };
}

export function updateErrorMessage(error: UpdateError): string {
  switch (error.kind) {
    case "network": return "Could not reach GitHub. Check your connection and try again.";
    case "invalid_response": return "Could not read the latest release from GitHub. Try again later, or view releases on GitHub.";
    case "service": return "GitHub could not complete the check. Try again later, or view releases on GitHub.";
    case "rate_limited": return "GitHub's request limit was reached. You can also view releases on GitHub.";
    case "interrupted": return "The update check was interrupted. Try again.";
    case "state_unavailable": return "Could not read update check status. Restart Delta-V and try again.";
  }
}

export function beginUpdateCheck(state: UpdateState, now: number): UpdateState | null {
  if (state.checking || (state.next_check_at !== null && state.next_check_at > now)) return null;
  return { ...state, checking: true, error: null, next_check_at: null };
}

type PreviewOutcome = "unchecked" | "current" | "available" | "ahead" | "network"
  | "rate_limited" | "invalid_response" | "service" | "stale";

function previewOutcome(value: string | null, fallback: PreviewOutcome): PreviewOutcome {
  switch (value) {
    case "unchecked": case "current": case "available": case "ahead": case "network":
    case "rate_limited": case "invalid_response": case "service": case "stale": return value;
    default: return fallback;
  }
}

function sampleVersion(installed: string, status: "available" | "ahead"): string {
  const [major = 0, minor = 0, patch = 0] = installed.split(".").map((part) => Number.parseInt(part, 10));
  if (status === "available") return `${major}.${minor}.${patch + 1}`;
  if (patch > 0) return `${major}.${minor}.${patch - 1}`;
  if (minor > 0) return `${major}.${minor - 1}.0`;
  return `${Math.max(0, major - 1)}.0.0`;
}

function previewState(outcome: PreviewOutcome, previous: UpdateState, installed: string, now: number): UpdateState {
  const state = { ...emptyUpdateState(), last_success: previous.last_success };
  if (outcome === "unchecked") return emptyUpdateState();
  if (outcome === "current" || outcome === "available" || outcome === "ahead") {
    state.last_success = {
      installed_version: installed,
      latest_version: outcome === "current" ? installed : sampleVersion(installed, outcome),
      status: outcome,
      checked_at: now,
    };
  } else if (outcome === "stale") {
    state.last_success = { installed_version: installed, latest_version: installed, status: "current", checked_at: now - 600 };
    state.error = { kind: "network" };
  } else if (outcome === "rate_limited") {
    state.error = { kind: "rate_limited", retry_at: now + 60 };
    state.next_check_at = now + 60;
  } else if (outcome === "service") {
    state.error = { kind: "service", status: 503, retry_at: null };
  } else {
    state.error = { kind: outcome };
  }
  return state;
}

export function createUpdatePreview(search: string, installed: string, now: number): UpdateState {
  const outcome = previewOutcome(new URLSearchParams(search).get("updates"), "unchecked");
  return previewState(outcome, emptyUpdateState(), installed, now);
}

export function completeUpdatePreview(previous: UpdateState, search: string, installed: string, now: number): UpdateState {
  const outcome = previewOutcome(new URLSearchParams(search).get("updates_result"), "current");
  return previewState(outcome, previous, installed, now);
}
