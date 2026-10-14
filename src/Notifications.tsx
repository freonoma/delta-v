import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { NotificationStatus } from "./types";
import { native, preview } from "./ui-state";
import {
  completeNotificationPreview, createNotificationPreview, notificationActionAllowed,
  notificationAvailability, notificationFailure, notificationReadError, notificationStatusText,
  notificationTestNotice, readNotificationPreview,
} from "./notification-state";
import type { NotificationAction } from "./notification-state";

export function useNotifications(savedEnabled: boolean, onPreviewChange: (enabled: boolean) => void) {
  const [state, setState] = useState<NotificationStatus>(() => preview
    ? createNotificationPreview(window.location.search)
    : { enabled: savedEnabled, permission: null, error: null });
  const [reading, setReading] = useState(native);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const current = useRef(state);
  const mounted = useRef(false);
  const lifetime = useRef(0);
  const revision = useRef(0);
  const operation = useRef<number | null>(null);
  const previewChange = useRef(onPreviewChange);
  previewChange.current = onPreviewChange;

  const publish = useCallback((next: NotificationStatus) => {
    current.current = next;
    setState(next);
  }, []);

  useEffect(() => {
    if (operation.current === null && current.current.enabled !== savedEnabled) {
      revision.current += 1;
      publish({ ...current.current, enabled: savedEnabled });
      setReading(false);
      setNotice(null);
    }
  }, [savedEnabled, publish]);

  const refresh = useCallback(async () => {
    if (!mounted.current || operation.current !== null || (!native && !preview)) return;
    const life = lifetime.current;
    const read = ++revision.current;
    setReading(true);
    setNotice(null);
    try {
      const next = native ? await invoke<NotificationStatus>("get_notification_state")
        : readNotificationPreview(current.current, window.location.search);
      if (mounted.current && life === lifetime.current && read === revision.current) {
        publish(next);
        setError(null);
        if (preview) previewChange.current(next.enabled);
      }
    } catch {
      if (mounted.current && life === lifetime.current && read === revision.current) {
        publish({ ...current.current, permission: null });
        setError(notificationReadError);
      }
    } finally {
      if (mounted.current && life === lifetime.current && read === revision.current) setReading(false);
    }
  }, [publish]);

  useEffect(() => {
    mounted.current = true;
    lifetime.current += 1;
    setPending(false);
    if (native) void refresh();
    const focus = () => { void refresh(); };
    window.addEventListener("focus", focus);
    return () => {
      mounted.current = false;
      lifetime.current += 1;
      revision.current += 1;
      operation.current = null;
      window.removeEventListener("focus", focus);
    };
  }, [refresh]);

  const act = useCallback(async (action: NotificationAction) => {
    if (!mounted.current || operation.current !== null || (!native && !preview)
      || !notificationActionAllowed(current.current, action)) return;
    const life = lifetime.current;
    const actionRevision = ++revision.current;
    operation.current = actionRevision;
    const valid = () => mounted.current && life === lifetime.current && actionRevision === revision.current;
    setReading(false);
    setPending(true);
    setError(null);
    setNotice(null);
    try {
      if (preview) {
        await new Promise<void>((resolve) => window.setTimeout(resolve, 400));
        if (!valid()) return;
        const result = completeNotificationPreview(current.current, action, window.location.search);
        const next = action === "open" ? result.state : readNotificationPreview(result.state, window.location.search);
        publish(next);
        previewChange.current(next.enabled);
        setError(result.error);
        setNotice(result.notice);
      } else if (action === "enable" || action === "disable") {
        const next = await invoke<NotificationStatus>("set_notifications_enabled", { enabled: action === "enable" });
        if (valid()) publish(next);
      } else {
        await invoke(action === "test" ? "send_test_notification" : "open_notification_settings");
        if (valid() && action === "test") {
          setNotice(notificationTestNotice);
          publish({ ...current.current, error: null });
          try {
            const next = await invoke<NotificationStatus>("get_notification_state");
            if (valid()) publish(next);
          } catch {
            if (valid()) {
              publish({ ...current.current, permission: null });
              setError(notificationReadError);
            }
          }
        }
      }
    } catch (caught: unknown) {
      if (!valid()) return;
      const failure = notificationFailure(action, caught);
      if (action !== "open") {
        try {
          const next = await invoke<NotificationStatus>("get_notification_state");
          if (valid()) publish(next);
        } catch {
          if (valid()) publish({ ...current.current, permission: null });
        }
      }
      if (valid()) setError(failure);
    } finally {
      if (operation.current === actionRevision) operation.current = null;
      if (valid()) setPending(false);
    }
  }, [publish]);

  return { state, reading, pending, error, notice, refresh, act };
}

export function Notifications({ notifications, savedThreshold, thresholdChanged, onEditThreshold }: {
  notifications: ReturnType<typeof useNotifications>;
  savedThreshold: number;
  thresholdChanged: boolean;
  onEditThreshold: () => void;
}) {
  const { state, reading, pending, error, notice, refresh, act } = notifications;
  const availability = notificationAvailability(state);
  const failure = error ?? state.error;
  const busy = reading || pending;
  const toggleDisabled = pending || (!state.enabled && (reading || !notificationActionAllowed(state, "enable")));

  return (
    <section className="notification-settings" aria-label="Low-allowance notifications">
      <h3>Low-allowance notifications</h3>
      <div className="notification-setting-row">
        <div><span id="notification-label">Notify when allowance is low</span>
          <p id="notification-description">Check both connected providers, including those hidden from the panel. Changes apply immediately.</p></div>
        <button type="button" role="switch" className="startup-switch" aria-labelledby="notification-label"
          aria-describedby="notification-description notification-status" aria-checked={state.enabled}
          disabled={toggleDisabled} onClick={() => void act(state.enabled ? "disable" : "enable")}><span /></button>
      </div>
      <div className="notification-status" id="notification-status" role="status" aria-live="polite" aria-atomic="true">
        <p>{pending ? "Updating notifications…" : reading ? "Checking macOS permission…"
          : availability === "unknown" && failure ? `Low-allowance notifications are ${state.enabled ? "on" : "off"}.`
          : notificationStatusText(state)}</p>
        {notice && <p className="notification-notice">{notice}</p>}
      </div>
      {failure && <p className="notification-error" role="alert">{failure}</p>}
      <div className="notification-actions">
        {state.enabled && availability === "not_determined" && <button type="button" className="primary-button"
          disabled={busy} onClick={() => void act("enable")}>Allow notifications</button>}
        <button type="button" className="text-button" disabled={busy || !notificationActionAllowed(state, "test")}
          onClick={() => void act("test")}>Send test notification</button>
        <button type="button" className="text-button" disabled={pending} onClick={() => void act("open")}>Open macOS notification settings</button>
        <button type="button" className="text-button" disabled={busy} onClick={() => void refresh()}>Check again</button>
      </div>
      <div className="notification-threshold">
        <p>When enabled, notify below <strong>{savedThreshold}% remaining</strong>, using the saved Low allowance threshold in Display &amp; usage.</p>
        {savedThreshold === 0 && <p className="notification-caption">At 0%, usage cannot cross below the threshold, so no low-allowance alerts will be sent. Choose a higher threshold to receive alerts.</p>}
        {thresholdChanged && <p className="notification-caption">Your threshold change is not saved yet. Highlighting and enabled alerts still use {savedThreshold}% remaining.</p>}
        <button type="button" className="text-button" onClick={onEditThreshold}>Edit threshold in Display &amp; usage</button>
      </div>
      <p className="notification-caption">The first reading stays quiet, even if already low. A later reading must cross below the threshold. While Delta-V stays open, each quota period can send one alert.</p>
      <p className="notification-caption">Alerts are silent and do not require history recording. Focus or macOS notification settings may hide them.</p>
      {preview && <p className="notification-preview">Browser preview: settings and test requests are simulated. No macOS permission prompt or notification is sent.</p>}
    </section>
  );
}
