import type { Limit, ProviderId, ProviderState, Settings } from "./types";

export type ResetState = "upcoming" | "elapsed" | "missing" | "invalid";

export interface UpcomingReset {
  provider: ProviderId;
  limitId: string;
  label: string;
  windowSeconds: number | null;
  fetchedAt: number;
  resetAt: number | null;
  state: ResetState;
  stale: boolean;
}

type ResetSettings = Pick<Settings, "providers" | "claude_enabled" | "codex_enabled">;

function validTimestamp(timestamp: number): boolean {
  return Number.isFinite(timestamp) && Number.isFinite(new Date(timestamp * 1000).getTime());
}

function resetState(timestamp: Limit["resets_at"], now: number): ResetState {
  if (timestamp === null) return "missing";
  if (!validTimestamp(timestamp)) return "invalid";
  return timestamp <= now ? "elapsed" : "upcoming";
}

const stateOrder: Record<ResetState, number> = { upcoming: 0, elapsed: 1, missing: 2, invalid: 3 };

function compareText(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function compareResets(left: UpcomingReset, right: UpcomingReset): number {
  const group = stateOrder[left.state] - stateOrder[right.state];
  if (group !== 0) return group;
  if ((left.state === "upcoming" || left.state === "elapsed") && left.resetAt !== null && right.resetAt !== null) {
    const time = left.state === "upcoming" ? left.resetAt - right.resetAt : right.resetAt - left.resetAt;
    if (time !== 0) return time;
  }
  return compareText(left.provider, right.provider) || compareText(left.limitId, right.limitId)
    || (left.windowSeconds ?? Infinity) - (right.windowSeconds ?? Infinity) || 0;
}

// Reset times describe the last report, never a refill or a prediction of the next period.
// Keep timing separate from freshness, including while a stale reading is being checked.
export function upcomingResets(providers: readonly ProviderState[], settings: ResetSettings, now: number): UpcomingReset[] {
  if (!validTimestamp(now)) throw new RangeError("Current time must be valid Unix seconds");
  const rows: UpcomingReset[] = [];
  for (const provider of providers) {
    const enabled = provider.id === "claude" ? settings.claude_enabled : settings.codex_enabled;
    if (!enabled || (settings.providers !== "both" && settings.providers !== provider.id)
      || provider.recovery !== null || provider.snapshot === null) continue;
    const snapshot = provider.snapshot;
    for (const limit of snapshot.limits) {
      // Percentage eligibility would hide elapsed resets and windows with missing usage.
      if (!limit.enabled || limit.kind !== "quota" || limit.provenance !== "official") continue;
      rows.push({
        provider: provider.id, limitId: limit.id, label: limit.label, windowSeconds: limit.window_seconds,
        fetchedAt: snapshot.fetched_at, resetAt: limit.resets_at, state: resetState(limit.resets_at, now),
        stale: provider.stale || provider.error !== null,
      });
    }
  }
  // Future resets come first; retain elapsed times (most recent first) and unavailable times.
  return rows.sort(compareResets);
}
