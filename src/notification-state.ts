import type { NotificationPermission, NotificationStatus } from "./types";

export type NotificationAction = "enable" | "disable" | "test" | "open";
export type NotificationAvailability = "ready" | "not_determined" | "denied" | "alerts_disabled" | "unavailable" | "unknown";

export const notificationReadError = "Could not read notification status. Check again.";
export const notificationTestNotice = "macOS accepted the test notification request. Focus or notification settings may prevent it from appearing.";

export function notificationAvailability(state: NotificationStatus): NotificationAvailability {
  const permission = state.permission;
  if (!permission) return "unknown";
  if (!permission.available) return "unavailable";
  if (permission.authorization === "denied") return "denied";
  if (permission.authorization === "not_determined") return "not_determined";
  if (permission.authorization === "unknown") return "unknown";
  return permission.alerts_enabled ? "ready" : "alerts_disabled";
}

export function notificationActionAllowed(state: NotificationStatus, action: NotificationAction): boolean {
  const availability = notificationAvailability(state);
  if (action === "disable") return state.enabled;
  if (action === "open") return true;
  if (action === "test") return state.enabled && availability === "ready";
  return availability === "ready" || availability === "not_determined";
}

export function notificationStatusText(state: NotificationStatus): string {
  switch (notificationAvailability(state)) {
    case "unknown": return "Could not check notification permission. Check again.";
    case "unavailable": return state.permission?.reason ?? "Notifications are unavailable in this copy of Delta-V.";
    case "denied": return "Notifications are blocked in macOS. Allow Delta-V in System Settings > Notifications.";
    case "alerts_disabled": return "Alerts are off in macOS. Allow alerts for Delta-V in System Settings > Notifications.";
    case "not_determined": return state.enabled
      ? "Permission is needed to resume alerts. Choose Allow notifications to ask macOS."
      : "Turn on to request permission from macOS.";
    case "ready": return state.enabled ? "Low-allowance notifications are on." : "Low-allowance notifications are off.";
  }
}

export function notificationFailure(action: NotificationAction, caught: unknown): string {
  if (typeof caught === "string" && caught.trim() && caught.length <= 500) return caught;
  if (caught instanceof Error && caught.message.trim() && caught.message.length <= 500) return caught.message;
  switch (action) {
    case "enable": return "Could not enable notifications. Check permission and try again.";
    case "disable": return "Could not turn off notifications. Try again.";
    case "test": return "Could not send the test notification. Try again.";
    case "open": return "Could not open notification settings. Open System Settings > Notifications > Delta-V.";
  }
}

function previewState(example: string | null, fallbackEnabled = false): NotificationStatus {
  const permission: NotificationPermission = {
    authorization: "authorized", alerts_enabled: true, available: true, reason: null,
  };
  switch (example) {
    case "enabled": return { enabled: true, permission, error: null };
    case "off": return { enabled: false, permission, error: null };
    case "denied": return { enabled: true, permission: { ...permission, authorization: "denied", alerts_enabled: false }, error: null };
    case "alerts_disabled": return { enabled: true, permission: { ...permission, alerts_enabled: false }, error: null };
    case "unavailable": return { enabled: fallbackEnabled, permission: {
      ...permission, available: false, authorization: "unknown", alerts_enabled: false,
      reason: "Notifications are available when Delta-V runs from its app bundle. Open Delta-V.app and try again.",
    }, error: null };
    case "read_error": return { enabled: fallbackEnabled, permission: null, error: notificationReadError };
    case "delivery_error": return { enabled: true, permission, error: "Could not send the notification. Try again." };
    default: return { enabled: false, permission: { ...permission, authorization: "not_determined", alerts_enabled: false }, error: null };
  }
}

export function createNotificationPreview(search: string): NotificationStatus {
  const parameters = new URLSearchParams(search);
  const example = parameters.get("notifications");
  // Both kinds of alerts share macOS permission, but keep independent opt-ins.
  if (example === null && parameters.has("watches")) {
    const watchExample = parameters.get("watches");
    const sharedPermission = ["denied", "unavailable", "read_error"].includes(watchExample ?? "")
      ? watchExample : "off";
    return { ...previewState(sharedPermission), enabled: false };
  }
  return previewState(example, example === "read_error");
}

export function readNotificationPreview(current: NotificationStatus, search: string): NotificationStatus {
  const result = new URLSearchParams(search).get("notifications_read_result");
  return result ? { ...previewState(result, current.enabled), enabled: current.enabled } : current;
}

export function completeNotificationPreview(current: NotificationStatus, action: NotificationAction, search: string): {
  state: NotificationStatus; error: string | null; notice: string | null;
} {
  const parameters = new URLSearchParams(search);
  const result = parameters.get(`notifications_${action}_result`) ?? parameters.get("notifications_result");
  if (result === "error" || result === "save_error" || result === "delivery_error") {
    const error = result === "save_error" ? "Could not save the notification preference. Try again."
      : notificationFailure(action, null);
    return { state: action === "test" ? { ...current, error } : current, error, notice: null };
  }
  if (action === "open") return { state: current, error: null, notice: "Browser preview: opening System Settings is simulated." };
  if (action === "test") {
    if (result && !["enabled", "off"].includes(result)) {
      const next = { ...previewState(result, current.enabled), enabled: current.enabled };
      return { state: next, error: notificationAvailability(next) === "ready" ? null : notificationStatusText(next), notice: null };
    }
    return { state: { ...current, error: null }, error: null,
      notice: "Browser preview: test request simulated. No macOS notification was sent." };
  }
  let next = result ? previewState(result, current.enabled) : { ...current, error: null };
  if (action === "disable") next = { ...next, enabled: false };
  else if (!result || result === "enabled" || result === "off") next = previewState("enabled");
  else next = { ...next, enabled: current.enabled };
  const allowed = action === "disable" || notificationAvailability(next) === "ready";
  return { state: next, error: allowed ? null : notificationStatusText(next), notice: null };
}
