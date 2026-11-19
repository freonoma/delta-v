import { allowancePace } from "../pacing.ts";
import { providerEnabled } from "../provider-display.ts";
import type { ProviderId, ProviderState, Settings } from "../types.ts";
import { featuredQuota, remainingPercent } from "../usage.ts";

interface WindowSample {
  provider: ProviderId;
  scope: string;
  limit: string;
  readingId: string;
  observedAt: number;
  resetAt: number;
  windowSeconds: number;
}

export type MascotReading =
  | { pose: "unavailable"; remaining: null; sample: null }
  | { pose: "normal" | "low" | "exhausted"; remaining: number; sample: WindowSample | null };

const unavailable: MascotReading = { pose: "unavailable", remaining: null, sample: null };

export function mascotReading(
  provider: ProviderState, settings: Settings, now: number,
): MascotReading {
  const snapshot = provider.snapshot;
  if (!providerEnabled(settings, provider.id) || !snapshot || snapshot.provider !== provider.id
    || provider.stale || provider.error || provider.recovery
    || !Number.isFinite(now) || !Number.isFinite(snapshot.fetched_at) || snapshot.fetched_at > now) {
    return unavailable;
  }
  // The provider card marks a cached snapshot stale when a reported quota expires.
  if (snapshot.limits.some((limit) =>
    limit.enabled && limit.kind === "quota" && limit.resets_at !== null && limit.resets_at <= now)) {
    return unavailable;
  }
  const featured = featuredQuota(provider.id, snapshot.limits, settings.tracked_limit, now);
  if (!featured) return unavailable;
  const remaining = remainingPercent(featured);
  if (remaining === null) return unavailable;

  const pose = remaining === 0 ? "exhausted" : remaining < settings.threshold ? "low" : "normal";
  const pace = allowancePace(featured, provider, now);
  if (pace.state !== "ready" || !provider.reading_scope || !provider.reading_id) return { pose, remaining, sample: null };
  return { pose, remaining, sample: {
    provider: provider.id, scope: provider.reading_scope, limit: featured.id, readingId: provider.reading_id,
    observedAt: pace.observedAt, resetAt: pace.resetAt, windowSeconds: pace.windowSeconds,
  } };
}

export function confirmedRefill(previous: MascotReading | null, current: MascotReading): boolean {
  const before = previous?.sample;
  const after = current.sample;
  if (!before || !after) return false;
  return before.provider === after.provider && before.scope === after.scope && before.limit === after.limit
    && before.windowSeconds === after.windowSeconds && before.readingId !== after.readingId
    && after.observedAt > before.observedAt && after.observedAt >= before.resetAt
    && after.resetAt - after.windowSeconds >= before.resetAt && current.remaining > previous.remaining;
}

export function refillBaseline(
  previous: MascotReading | null, reading: MascotReading, provider: ProviderState,
): MascotReading {
  const sample = previous?.sample;
  // The scheduler can mark this same receipt stale before the next window is checked.
  if (sample && provider.stale && !provider.error && !provider.recovery
    && provider.id === sample.provider && provider.reading_id === sample.readingId
    && provider.reading_scope === sample.scope) {
    return previous;
  }
  return reading;
}
