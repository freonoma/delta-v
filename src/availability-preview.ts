import type { AppState, NotificationPermission, ProviderId } from "./types";
import {
  availabilityPermissionMessage, availabilityQuota, availabilityReadError, exhaustedAvailabilityQuota,
} from "./availability-state.ts";
import type { AvailabilityStatus, AvailabilityWatch } from "./availability-state";

export interface AvailabilityPreview {
  watches: AvailabilityWatch[];
  nextId: number;
  prepared: ProviderId[];
  verified: ProviderId[];
  permission: NotificationPermission | null;
  error: string | null;
}

export type AvailabilityPreviewAction =
  | { kind: "prepare"; provider: ProviderId }
  | { kind: "clear"; provider: ProviderId }
  | { kind: "arm"; provider: ProviderId; limitId: string; readingId: string }
  | { kind: "cancel"; id: string };

export function availabilityPreviewRequested(search: string): boolean {
  return new URLSearchParams(search).has("watches");
}

export function createAvailabilityPreview(search: string, now: number): AvailabilityPreview {
  const mode = new URLSearchParams(search).get("watches");
  let permission: NotificationPermission | null = {
    authorization: mode === null ? "not_determined" : "authorized",
    alerts_enabled: mode !== null, available: true, reason: null,
  };
  if (mode === "denied") permission = { ...permission, authorization: "denied", alerts_enabled: false };
  if (mode === "unavailable") permission = {
    ...permission, available: false, authorization: "unknown", alerts_enabled: false,
    reason: "Notifications are available when Delta-V runs from its app bundle. Open Delta-V.app and try again.",
  };
  if (mode === "read_error") permission = null;
  const saved = ["pending", "other_account", "denied", "unavailable", "read_error", "save_error", "delivery_error", "long"].includes(mode ?? "");
  const watches: AvailabilityWatch[] = saved ? (["claude", "codex"] as const).map((provider, index) => ({
    id: String(index + 1), provider, limit_id: provider === "claude" ? "session" : "rate_limit:primary_window",
    label: mode === "long" ? "Shared allowance for extended reasoning and large codebase reviews"
      : provider === "claude" ? "5-hour" : "Weekly",
    window_seconds: provider === "claude" ? 18000 : 604800, armed_at: now - (index + 1) * 3600,
    account_match: mode === "other_account" ? "different" : "current",
  })) : [];
  return {
    watches, nextId: watches.length + 1, prepared: [],
    verified: mode === "identity" ? [] : ["claude", "codex"], permission,
    error: mode === "read_error" ? availabilityReadError
      : mode === "delivery_error" ? "macOS could not accept a previous availability notification." : null,
  };
}

function providerEnabled(app: AppState, provider: ProviderId): boolean {
  return provider === "claude" ? app.settings.claude_enabled : app.settings.codex_enabled;
}

export function readAvailabilityPreview(
  model: AvailabilityPreview, app: AppState | null, search: string, now: number, explicit = false,
): { model: AvailabilityPreview; state: AvailabilityStatus } {
  const parameters = new URLSearchParams(search);
  if (parameters.get("watches_read_result") === "error") throw new Error(availabilityReadError);
  const next = explicit && parameters.get("watches_verify") === "ready"
    ? { ...model, verified: [...new Set([...model.verified, ...model.prepared])] } : model;
  const providers = app?.providers.map((provider) => {
    const usable = providerEnabled(app, provider.id) && !app.paused && provider.snapshot !== null
      && provider.error === null && provider.recovery === null;
    const readingId = (provider as typeof provider & { reading_id?: string | null }).reading_id ?? null;
    return {
      provider: provider.id, reading_id: readingId, preparing: next.prepared.includes(provider.id),
      quotas: usable ? (provider.snapshot?.limits ?? []).filter((limit) => exhaustedAvailabilityQuota(provider, limit, now))
        .map((limit) => ({
          limit_id: limit.id, window_seconds: limit.window_seconds,
          readiness: next.verified.includes(provider.id) ? "ready" as const : "needs_identity" as const,
        })) : [],
    };
  }) ?? [];
  return {
    model: next,
    state: {
      watches: next.watches.map((watch) => {
        const provider = app?.providers.find((candidate) => candidate.id === watch.provider);
        const verified = app && providerEnabled(app, watch.provider) && provider?.snapshot
          && !provider.error && !provider.recovery && next.verified.includes(watch.provider);
        return { ...watch, account_match: verified ? watch.account_match : "unverified" };
      }),
      providers, permission: next.permission, error: next.error,
    },
  };
}

export function completeAvailabilityPreview(
  model: AvailabilityPreview, action: AvailabilityPreviewAction, app: AppState | null, search: string, now: number,
): { model: AvailabilityPreview; error: string | null } {
  const parameters = new URLSearchParams(search);
  const result = parameters.get(`watches_${action.kind}_result`) ?? parameters.get("watches_result");
  const mode = parameters.get("watches");
  const failure = result === "error" || result === "save_error"
    || (mode === "save_error" && (action.kind === "arm" || action.kind === "cancel"));
  if (failure) return { model, error: action.kind === "cancel"
    ? "Could not cancel this availability notification. The saved notification is unchanged."
    : action.kind === "arm" ? "Could not save this availability notification."
    : "Could not update account preparation. Try again." };
  let next = model;
  if (action.kind === "prepare") {
    if (!app || !providerEnabled(app, action.provider)) return { model, error: "Connect this provider before preparing an availability notification." };
    next = { ...model, prepared: [...new Set([...model.prepared, action.provider])],
      verified: result === "ready" ? [...new Set([...model.verified, action.provider])] : model.verified };
  } else if (action.kind === "clear") {
    next = { ...model, prepared: model.prepared.filter((provider) => provider !== action.provider) };
  } else if (action.kind === "cancel") {
    next = { ...model, watches: model.watches.filter((watch) => watch.id !== action.id), error: null };
  } else {
    const state = readAvailabilityPreview(model, app, "", now).state;
    const provider = app?.providers.find((candidate) => candidate.id === action.provider);
    const limit = provider?.snapshot?.limits.find((candidate) => candidate.id === action.limitId);
    const current = state.providers.find((candidate) => candidate.provider === action.provider);
    const quota = provider && limit ? availabilityQuota(state, provider, limit, now) : null;
    if (!app || !providerEnabled(app, action.provider) || app.paused
      || !quota || current?.reading_id !== action.readingId) return { model, error: "The usage reading changed. Check the quota and try again." };
    if (quota.readiness === "needs_identity") return { model, error: "A verified account is needed to start this notification. Prepare the account, then try again after a usage check." };
    if (result === "denied") next = { ...model, permission: {
      authorization: "denied", alerts_enabled: false, available: true, reason: null,
    } };
    const permission = next.permission;
    if (!permission || !permission.available || (permission.authorization !== "not_determined"
      && availabilityPermissionMessage(permission) !== null)) {
      return { model: next, error: availabilityPermissionMessage(permission) };
    }
    const existing = next.watches.find((watch) => watch.provider === action.provider
      && watch.limit_id === action.limitId && watch.window_seconds === quota.window_seconds && watch.account_match === "current");
    next = {
      ...next, error: null, prepared: next.prepared.filter((candidate) => candidate !== action.provider),
      permission: { ...permission, authorization: "authorized", alerts_enabled: true },
      watches: existing ? next.watches : [...next.watches, {
        id: String(next.nextId), provider: action.provider, limit_id: action.limitId, label: limit?.label ?? action.limitId,
        window_seconds: quota.window_seconds, armed_at: now, account_match: "current",
      }],
      nextId: existing ? next.nextId : next.nextId + 1,
    };
  }
  return { model: next, error: result === "postcommit_error"
    ? "Availability notifications were updated, but their durable storage could not be confirmed." : null };
}
