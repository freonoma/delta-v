import type { Limit, ProviderState } from "./types";

export type PaceUnavailableReason = "unsupported" | "blocked" | "freshness" | "usage"
  | "window" | "reset" | "elapsed" | "clock";

export interface ReadyPace {
  state: "ready";
  observedAt: number;
  resetAt: number;
  windowSeconds: number;
  remainingSeconds: number;
  usedFraction: number;
  remainingFraction: number;
  elapsedFraction: number;
  guideRemainingFraction: number;
  deltaFraction: number;
  // Percentage points of this quota per unit, not tokens or hours of work.
  remainingPerHour: number;
  remainingPerDay: number;
}

export type AllowancePace = ReadyPace | { state: "unavailable"; reason: PaceUnavailableReason };

function validTimestamp(timestamp: number): boolean {
  return Number.isFinite(timestamp) && Number.isFinite(new Date(timestamp * 1000).getTime());
}

// An even-usage reference for one reported window, not an exhaustion forecast.
// Keep the guide and remaining budget at the observation time: a cached percentage
// must not improve its pace merely because the clock advances between checks.
export function allowancePace(limit: Limit, provider: ProviderState, now: number, blocked = false): AllowancePace {
  if (!validTimestamp(now)) return { state: "unavailable", reason: "clock" };
  if (!limit.enabled || limit.kind !== "quota" || limit.provenance !== "official") {
    return { state: "unavailable", reason: "unsupported" };
  }
  if (blocked || provider.recovery !== null) return { state: "unavailable", reason: "blocked" };
  const snapshot = provider.snapshot;
  if (!snapshot || snapshot.provider !== provider.id || provider.stale || provider.error !== null) {
    return { state: "unavailable", reason: "freshness" };
  }
  const used = limit.used_fraction;
  if (used === null || !Number.isFinite(used) || used < 0 || used > 1) {
    return { state: "unavailable", reason: "usage" };
  }
  const window = limit.window_seconds;
  if (window === null || !Number.isFinite(window) || window <= 0) {
    return { state: "unavailable", reason: "window" };
  }
  const reset = limit.resets_at;
  if (reset === null || !validTimestamp(reset)) return { state: "unavailable", reason: "reset" };
  if (reset <= now) return { state: "unavailable", reason: "elapsed" };
  const observed = snapshot.fetched_at;
  if (!validTimestamp(observed) || observed > now) return { state: "unavailable", reason: "clock" };
  const start = reset - window;
  if (!validTimestamp(start) || observed < start) return { state: "unavailable", reason: "window" };

  const elapsedFraction = (observed - start) / window;
  const remainingSeconds = reset - observed;
  const remainingFraction = 1 - used;
  const remainingPerHour = remainingFraction * 100 * 3600 / remainingSeconds;
  const remainingPerDay = remainingPerHour * 24;
  if (!Number.isFinite(remainingPerHour) || !Number.isFinite(remainingPerDay)) {
    return { state: "unavailable", reason: "window" };
  }
  return {
    state: "ready", observedAt: observed, resetAt: reset, windowSeconds: window,
    remainingSeconds, usedFraction: used, remainingFraction, elapsedFraction,
    guideRemainingFraction: 1 - elapsedFraction, deltaFraction: used - elapsedFraction,
    remainingPerHour, remainingPerDay,
  };
}
