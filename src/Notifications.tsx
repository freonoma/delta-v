import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { NotificationStatus, PaceNotificationScope, Settings } from "./types";
import { native, preview } from "./ui-state";
import {
  completeNotificationPreview, createNotificationPreview, notificationActionAllowed,
  notificationAvailability, notificationFailure, notificationReadError, notificationStatusText,
  notificationTestNotice, readNotificationPreview,
} from "./notification-state";
import type { NotificationAction } from "./notification-state";

export function useNotifications(saved: Pick<Settings, "notifications_enabled" | "pace_notifications_enabled" | "pace_notifications_scope">,
  onPreviewChange: (state: NotificationStatus) => void) {
  const savedEnabled = saved.notifications_enabled;
  const savedPaceEnabled = saved.pace_notifications_enabled;
  const savedPaceScope = saved.pace_notifications_scope;
  const [state, setState] = useState<NotificationStatus>(() => preview
    ? createNotificationPreview(window.location.search)
    : { enabled: savedEnabled, pace_enabled: savedPaceEnabled, pace_scope: savedPaceScope, permission: null, error: null });
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
    if (operation.current === null && (current.current.enabled !== savedEnabled
      || current.current.pace_enabled !== savedPaceEnabled || current.current.pace_scope !== savedPaceScope)) {
      revision.current += 1;
      publish({ ...current.current, enabled: savedEnabled, pace_enabled: savedPaceEnabled, pace_scope: savedPaceScope });
      setReading(false);
      setNotice(null);
    }
  }, [savedEnabled, savedPaceEnabled, savedPaceScope, publish]);

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
        if (preview) previewChange.current(next);
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

  const act = useCallback(async (action: NotificationAction, scope?: PaceNotificationScope) => {
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
    const targetScope = scope ?? current.current.pace_scope;
    try {
      if (preview) {
        await new Promise<void>((resolve) => window.setTimeout(resolve, 400));
        if (!valid()) return;
        const result = completeNotificationPreview(current.current, action, window.location.search, targetScope);
        const next = action === "open" ? result.state : readNotificationPreview(result.state, window.location.search);
        publish(next);
        previewChange.current(next);
        setError(result.error);
        setNotice(result.notice);
      } else if (action === "enable" || action === "disable") {
        const next = await invoke<NotificationStatus>("set_notifications_enabled", { enabled: action === "enable" });
        if (valid()) publish(next);
      } else if (action === "pace_enable" || action === "pace_disable" || action === "pace_scope") {
        const next = await invoke<NotificationStatus>("set_pace_notifications", {
          enabled: action === "pace_scope" ? current.current.pace_enabled : action === "pace_enable", scope: targetScope,
        });
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
  const paceToggleDisabled = pending || (!state.pace_enabled && (reading || !notificationActionAllowed(state, "pace_enable")));

  return (
    <section className="notification-settings" aria-label="Allowance notifications">
      <h3>Allowance notifications</h3>
      <p className="notification-caption">Check both connected providers, including those hidden from the panel. Changes apply immediately.</p>
      <div className="notification-setting-row">
        <div><span id="notification-label">Notify when allowance is low</span>
          <p id="notification-description">Notify below {savedThreshold}% remaining.</p></div>
        <button type="button" role="switch" className="startup-switch" aria-labelledby="notification-label"
          aria-describedby="notification-description notification-status" aria-checked={state.enabled}
          disabled={toggleDisabled} onClick={() => void act(state.enabled ? "disable" : "enable")}><span /></button>
      </div>
      {state.enabled && <div className="notification-threshold">
        {savedThreshold === 0 && <p className="notification-caption">At 0%, usage cannot cross below the threshold, so no low-allowance alerts will be sent. Choose a higher threshold to receive alerts.</p>}
        {thresholdChanged && <p className="notification-caption">Your threshold change is not saved yet. Highlighting and enabled alerts still use {savedThreshold}% remaining.</p>}
        <button type="button" className="text-button" onClick={onEditThreshold}>Edit threshold in Display &amp; usage</button>
        <p className="notification-caption">The first reading stays quiet, even if already low. A later reading must cross below the threshold. While Delta-V stays open, each quota period can send one alert.</p>
      </div>}
      <section className="notification-threshold" aria-label="Usage above pace">
        <div className="notification-setting-row">
          <div><span id="pace-notification-label">Usage above pace</span>
            {state.pace_enabled && <p id="pace-notification-description">Warn after usage stays above the guide across fresh checks. While Delta-V stays open, each window can alert once.</p>}</div>
          <button type="button" role="switch" className="startup-switch" aria-labelledby="pace-notification-label"
            aria-describedby={state.pace_enabled ? "pace-notification-description notification-status" : "notification-status"}
            aria-checked={state.pace_enabled} disabled={paceToggleDisabled}
            onClick={() => void act(state.pace_enabled ? "pace_disable" : "pace_enable")}><span /></button>
        </div>
        {state.pace_enabled && <>
          <label className="setting-row">
            <span>Watch allowances</span>
            <select value={state.pace_scope} disabled={busy} onChange={(event) => {
              const scope = event.target.value;
              if (scope === "weekly" || scope === "all") void act("pace_scope", scope);
            }}>
              <option value="weekly">Weekly and longer</option>
              <option value="all">All allowances</option>
            </select>
          </label>
        </>}
      </section>
      <div className="notification-status" id="notification-status" role="status" aria-live="polite" aria-atomic="true">
        <p>{pending ? "Updating notifications…" : reading ? "Checking macOS permission…"
          : availability === "unknown" && failure ? `Low allowance alerts: ${state.enabled ? "on" : "off"}. Pace alerts: ${state.pace_enabled ? "on" : "off"}.`
          : notificationStatusText(state)}</p>
        {notice && <p className="notification-notice">{notice}</p>}
      </div>
      {failure && <p className="notification-error" role="alert">{failure}</p>}
      <div className="notification-actions">
        {(state.enabled || state.pace_enabled) && availability === "not_determined" && <button type="button" className="primary-button"
          disabled={busy} onClick={() => void act(state.enabled ? "enable" : "pace_enable")}>Allow notifications</button>}
        <button type="button" className="text-button" disabled={busy || !notificationActionAllowed(state, "test")}
          onClick={() => void act("test")}>Send test notification</button>
        <button type="button" className="text-button" disabled={pending} onClick={() => void act("open")}>Open macOS notification settings</button>
        <button type="button" className="text-button" disabled={busy} onClick={() => void refresh()}>Check again</button>
      </div>
      <p className="notification-caption">Alerts are silent and do not require history recording. Focus or macOS notification settings may hide them.</p>
      {preview && <p className="notification-preview">Browser preview: settings and test requests are simulated. No macOS permission prompt or notification is sent.</p>}
    </section>
  );
}
