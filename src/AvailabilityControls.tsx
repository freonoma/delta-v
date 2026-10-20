import { useRef } from "react";
import type { AppState, Limit, ProviderState } from "./types";
import type { useAvailabilityWatches } from "./useAvailabilityWatches";
import {
  availabilityPermissionMessage, availabilityQuota, currentAvailabilityWatch,
} from "./availability-state";
import { providerEnabled, providerNames } from "./provider-display";
import { preview } from "./ui-state";
import "./AvailabilityControls.css";

type Watches = ReturnType<typeof useAvailabilityWatches>;

export function QuotaWatchControl({ watches, provider, limit, now, blocked }: {
  watches: Watches; provider: ProviderState; limit: Limit; now: number; blocked: boolean;
}) {
  const quota = blocked ? null : availabilityQuota(watches.state, provider, limit, now);
  const watch = currentAvailabilityWatch(watches.state, provider, limit);
  const preparing = watches.state.providers.find((item) => item.provider === provider.id)?.preparing ?? false;
  if (!watch && !quota) return null;
  const verify = quota?.readiness === "needs_identity";
  const permission = watches.state.permission;
  const canArm = permission?.available && ["authorized", "provisional", "not_determined"].includes(permission.authorization)
    && (permission.authorization === "not_determined" || permission.alerts_enabled);
  const label = watch ? "Cancel alert"
    : verify ? preparing ? "Cancel verification" : "Verify account to notify"
    : "Notify when available";

  return (
    <div className="quota-watch-control" data-watch-provider={provider.id}>
      {watch && <span className="quota-watch-status">Watching for allowance</span>}
      {!watch && verify && preparing && <span className="quota-watch-status">
        Waiting for account verification at the next scheduled check.
      </span>}
      <button type="button" className="text-button"
        aria-label={`${label}: ${providerNames[provider.id]}, ${limit.label}`}
        title={watch ? "Cancel this one-time alert." : "Alert once after a fresh reading confirms this quota has remaining allowance."}
        disabled={watches.pending || (!watch && !(verify && preparing) && (!canArm || blocked))}
        onClick={async (event) => {
          const button = event.currentTarget;
          const row = button.closest("li");
          const wasFocused = document.activeElement === button;
          const success = watch ? await watches.cancel(watch.id)
            : verify ? preparing ? await watches.clearPreparation(provider.id) : await watches.prepare(provider.id)
            : provider.reading_id ? await watches.arm(provider.id, limit.id, provider.reading_id) : false;
          if (success && wasFocused) window.requestAnimationFrame(() => {
            if (!button.isConnected && document.activeElement === document.body) row?.focus({ preventScroll: true });
          });
        }}>{watches.pending ? "Please wait…" : label}</button>
    </div>
  );
}

export function WatchFeedback({ watches, onManage }: { watches: Watches; onManage: () => void }) {
  const manage = useRef<HTMLButtonElement>(null);
  const relevant = watches.state.watches.length > 0 || watches.state.providers.some((provider) =>
    provider.preparing || provider.quotas.length > 0);
  const failure = watches.error ?? watches.state.error;
  const permission = relevant ? availabilityPermissionMessage(watches.state.permission) : null;
  if (!relevant && !failure && !watches.notice) return null;
  return (
    <div className="watch-feedback">
      <WatchPreparations watches={watches} onFocusFallback={() => manage.current?.focus({ preventScroll: true })} />
      {watches.notice && <p role="status">{watches.notice}</p>}
      {failure && <p className="watch-error" role="alert">{failure}</p>}
      {permission && <p>{permission}</p>}
      <button ref={manage} type="button" className="text-button" onClick={onManage}>Manage availability alerts{watches.state.watches.length > 0
        ? ` (${watches.state.watches.length})` : ""}</button>
      {(failure || permission || watches.state.providers.some((provider) => provider.preparing)) && <button
        type="button" className="text-button watch-status-check" disabled={watches.pending || watches.reading}
        onClick={() => void watches.refresh()}>Check alert status</button>}
    </div>
  );
}

function WatchPreparations({ watches, onFocusFallback }: { watches: Watches; onFocusFallback: () => void }) {
  return <>{watches.state.providers.filter((provider) => provider.preparing).map((provider) => (
    <div key={provider.provider} className="watch-preparation">
      <p>{providerNames[provider.provider]}: {provider.quotas.some((quota) => quota.readiness === "ready")
        ? "Account verified. Choose Notify when available beside the quota to save an alert."
        : "Waiting for account verification at the next scheduled check."}</p>
      <button type="button" className="text-button" disabled={watches.pending}
        aria-label={`Cancel account verification for ${providerNames[provider.provider]}`}
        onClick={async (event) => {
          const button = event.currentTarget;
          const focused = document.activeElement === button;
          if (!await watches.clearPreparation(provider.provider)) return;
          window.requestAnimationFrame(() => {
            if (focused && (document.activeElement === button || document.activeElement === document.body)) onFocusFallback();
          });
        }}>Cancel verification</button>
    </div>
  ))}</>;
}

export function AvailabilitySettings({ watches, app }: { watches: Watches; app: AppState }) {
  const list = useRef<HTMLUListElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const failure = watches.error ?? watches.state.error;
  const permission = availabilityPermissionMessage(watches.state.permission);
  return (
    <section className="availability-settings" aria-labelledby="availability-heading">
      <h3 id="availability-heading" ref={heading} tabIndex={-1}>Notify when available</h3>
      <p className="watch-caption">One-time alerts for exhausted quota windows. A fresh reading must confirm remaining allowance. A reset time alone cannot confirm it.</p>
      <p className="watch-caption">These alerts stay separate from low-allowance notifications and history recording. They keep their original account and survive app restarts.</p>
      <WatchPreparations watches={watches} onFocusFallback={() => heading.current?.focus({ preventScroll: true })} />
      {permission && <p className="watch-caption">{permission} Saved alerts stay pending.</p>}
      {app.paused && <p className="watch-caption">Usage checks are paused while your screen is locked.</p>}
      {watches.notice && <p role="status" className="watch-caption">{watches.notice}</p>}
      {failure && <p className="watch-error" role="alert">{failure}</p>}
      {watches.state.watches.length === 0 ? <p className="watch-empty">
        {watches.reading ? "Reading saved alerts…" : failure ? "Saved alert status is unavailable. Check alert status to try again."
          : "No availability alerts are saved. Choose Notify when available beside an exhausted quota in the usage panel."}
      </p> : <ul ref={list} className="saved-watch-list" aria-label="Saved availability alerts">
        {watches.state.watches.map((watch, index) => {
          const provider = app.providers.find((item) => item.id === watch.provider);
          const status = !providerEnabled(app.settings, watch.provider) ? "Paused while disconnected."
            : provider?.recovery ? "Paused during sign-in."
            : watch.account_match === "different" ? "Waiting for the original account."
            : watch.account_match === "unverified" ? "Waiting for a verified reading from the original account."
            : "Waiting for a fresh reading confirming this quota has allowance.";
          const date = new Date(watch.armed_at * 1000);
          return <li key={watch.id} className="saved-watch">
            <div>
              <p className="saved-watch-label"><strong>{providerNames[watch.provider]}</strong> · {watch.label}</p>
              <p>{status}</p>
              <p>Saved {Number.isFinite(date.getTime()) ? date.toLocaleString() : "at an unavailable time"}</p>
            </div>
            <button type="button" className="text-button" data-watch-cancel
              disabled={watches.pending} aria-label={`Cancel alert for ${providerNames[watch.provider]}, ${watch.label}`}
              onClick={async (event) => {
                const button = event.currentTarget;
                const wasFocused = document.activeElement === button;
                if (!await watches.cancel(watch.id)) return;
                window.requestAnimationFrame(() => {
                  if (!wasFocused || (document.activeElement !== button && document.activeElement !== document.body)) return;
                  const buttons = list.current?.querySelectorAll<HTMLButtonElement>("[data-watch-cancel]");
                  (buttons?.[Math.min(index, buttons.length - 1)] ?? heading.current)?.focus({ preventScroll: true });
                });
              }}>Cancel</button>
          </li>;
        })}
      </ul>}
      <div className="watch-actions">
        <button type="button" className="text-button" disabled={watches.pending || watches.reading}
          onClick={() => void watches.refresh()}>Check alert status</button>
      </div>
      <p className="watch-caption">Checks follow the usual refresh schedule and service waits, including for hidden providers. Focus or macOS settings may hide an alert.</p>
      {preview && <p className="watch-caption">Browser preview: alerts and account verification are simulated. No notification is sent.</p>}
    </section>
  );
}
