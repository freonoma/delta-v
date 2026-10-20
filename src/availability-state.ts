import type { Limit, NotificationPermission, ProviderId, ProviderState } from "./types";

export interface AvailabilityWatch {
  id: string;
  provider: ProviderId;
  limit_id: string;
  label: string;
  window_seconds: number | null;
  armed_at: number;
  account_match: "current" | "different" | "unverified";
}

export interface AvailabilityQuota {
  limit_id: string;
  window_seconds: number | null;
  readiness: "ready" | "needs_identity";
}

export interface AvailabilityProviderStatus {
  provider: ProviderId;
  reading_id: string | null;
  preparing: boolean;
  quotas: AvailabilityQuota[];
}

export interface AvailabilityStatus {
  watches: AvailabilityWatch[];
  providers: AvailabilityProviderStatus[];
  permission: NotificationPermission | null;
  error: string | null;
}

export type AvailabilityAction = "arm" | "prepare" | "clear" | "cancel";
type ProviderReading = ProviderState & { reading_id?: string | null };

export const availabilityReadError = "Could not read availability notifications. Check again.";

export function emptyAvailabilityStatus(): AvailabilityStatus {
  return { watches: [], providers: [], permission: null, error: null };
}

function validText(value: string): boolean {
  return value.trim().length > 0 && new TextEncoder().encode(value).length <= 256
    && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
}

function validTimestamp(value: number): boolean {
  return Number.isSafeInteger(value) && value >= -377705116800 && value <= 253402300799;
}

// This only filters visible actions. The backend owns account and freshness validation.
export function exhaustedAvailabilityQuota(provider: ProviderState, limit: Limit, now: number): boolean {
  const snapshot = provider.snapshot;
  if (!snapshot || snapshot.provider !== provider.id || provider.error || provider.recovery
    || !validTimestamp(now) || !validTimestamp(snapshot.fetched_at) || snapshot.fetched_at > now
    || !limit.enabled || limit.kind !== "quota" || limit.provenance !== "official"
    || limit.used_fraction !== 1 || limit.resets_at === null || !validTimestamp(limit.resets_at)
    || limit.resets_at <= now || limit.window_seconds === 0
    || !validText(limit.id) || !validText(limit.label)
    || limit.id.split(":").includes("row") || /^additional:\d+(?::|$)/u.test(limit.id)) return false;
  const matches = snapshot.limits.filter((candidate) => candidate.id === limit.id);
  return matches.length === 1 && matches[0] === limit
    && !snapshot.limits.some((candidate) => candidate.id.startsWith(`${limit.id}:row:`));
}

function currentProviderStatus(state: AvailabilityStatus, provider: ProviderReading): AvailabilityProviderStatus | undefined {
  if (!provider.reading_id) return undefined;
  return state.providers.find((candidate) => candidate.provider === provider.id
    && candidate.reading_id === provider.reading_id);
}

export function availabilityQuota(
  state: AvailabilityStatus, provider: ProviderReading, limit: Limit, now: number,
): AvailabilityQuota | null {
  if (!exhaustedAvailabilityQuota(provider, limit, now)) return null;
  return currentProviderStatus(state, provider)?.quotas.find((candidate) =>
    candidate.limit_id === limit.id && candidate.window_seconds === limit.window_seconds) ?? null;
}

export function currentAvailabilityWatch(
  state: AvailabilityStatus, provider: ProviderReading, limit: Limit,
): AvailabilityWatch | undefined {
  if (!currentProviderStatus(state, provider)) return undefined;
  return state.watches.find((watch) => watch.provider === provider.id && watch.limit_id === limit.id
    && watch.window_seconds === limit.window_seconds && watch.account_match === "current");
}

export function availabilityPermissionMessage(permission: NotificationPermission | null): string | null {
  if (!permission) return "Could not check notification permission. Check again.";
  if (!permission.available) return permission.reason ?? "Notifications are unavailable in this copy of Delta-V.";
  if (permission.authorization === "denied") return "Notifications are blocked. Allow Delta-V in System Settings > Notifications.";
  if (permission.authorization === "not_determined") return "Starting a notification will ask macOS for permission.";
  if (permission.authorization === "unknown") return "Could not check notification permission. Check again.";
  if (!permission.alerts_enabled) return "Alerts are off in macOS. Allow alerts for Delta-V in System Settings > Notifications.";
  return null;
}

export function availabilityFailure(action: AvailabilityAction, caught: unknown): string {
  const message = typeof caught === "string" ? caught : caught instanceof Error ? caught.message : "";
  if (message.trim() && message.length <= 500) return message;
  switch (action) {
    case "arm": return "Could not save this availability notification. Check again and retry.";
    case "prepare": return "Could not prepare this account. Try again.";
    case "clear": return "Could not stop preparing this account. Try again.";
    case "cancel": return "Could not cancel this availability notification. Check again and retry.";
  }
}

// Serialize cleanup before a later preparation, including a response arriving after dismissal.
export function createAvailabilityPreparation(
  prepare: (provider: ProviderId) => Promise<void>,
  clear: (provider: ProviderId) => Promise<void>,
) {
  let sequence = 0;
  let tail = Promise.resolve();
  const owned = new Map<ProviderId, number>();
  function enqueue<T>(action: () => Promise<T>): Promise<T> {
    const result = tail.then(action);
    tail = result.then(() => undefined, () => undefined);
    return result;
  }
  async function clearOwned(provider: ProviderId, token: number): Promise<void> {
    try {
      await clear(provider);
      if (owned.get(provider) === token) owned.delete(provider);
    }
    catch (error: unknown) {
      // A failed dismissal must remain retryable without replacing a newer request.
      if (!owned.has(provider)) owned.set(provider, token);
      throw error;
    }
  }
  return {
    prepare(provider: ProviderId): Promise<boolean> {
      const token = ++sequence;
      owned.set(provider, token);
      return enqueue(async () => {
        if (owned.get(provider) !== token) return false;
        try { await prepare(provider); }
        finally {
          if (owned.get(provider) !== token) await clearOwned(provider, token);
        }
        return owned.get(provider) === token;
      });
    },
    clear(provider: ProviderId): Promise<boolean> {
      const token = owned.get(provider) ?? ++sequence;
      owned.delete(provider);
      return enqueue(async () => { await clearOwned(provider, token); return true; });
    },
    release(provider: ProviderId) { owned.delete(provider); },
    abandon(): Promise<boolean> {
      const providers = [...owned.entries()];
      owned.clear();
      return enqueue(async () => {
        const results = await Promise.allSettled(providers.map(([provider, token]) => clearOwned(provider, token)));
        const failure = results.find((result) => result.status === "rejected");
        if (failure?.status === "rejected") throw failure.reason;
        return true;
      });
    },
  };
}
