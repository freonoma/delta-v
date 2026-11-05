import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { AllowancePace, ReadyPace } from "./pacing";
import type { Limit, PercentageMode, ProviderState } from "./types";
import { paceBudget, paceExplanation, paceMarkerPercent, paceStatus, paceUnavailable } from "./pacing-display";
import { providerNames } from "./provider-display";
import { Icon } from "./Icon";
import { PaceChart } from "./PaceChart";
import "./PaceGuide.css";

export function PaceMarker({ pace, mode }: { pace: AllowancePace; mode: PercentageMode }) {
  if (pace.state !== "ready") return null;
  return <span className="pace-marker" aria-hidden="true" style={{ left: `${paceMarkerPercent(pace, mode)}%` }} />;
}

function PacePopup({ pace, limit, provider, historyEnabled, anchor, id, onClose }: {
  pace: ReadyPace; limit: Limit; provider: ProviderState; historyEnabled: boolean; anchor: HTMLButtonElement; id: string;
  onClose: (restoreFocus: boolean) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);

  useLayoutEffect(() => {
    const element = dialog.current;
    const shell = anchor.closest<HTMLElement>(".popover-shell");
    if (!element || !shell) return;
    function place() {
      if (!element || !shell) return;
      const bounds = shell.getBoundingClientRect();
      const trigger = anchor.getBoundingClientRect();
      const width = Math.max(0, Math.min(304, bounds.width - 16));
      element.style.width = `${width}px`;
      element.style.maxHeight = `${Math.max(0, bounds.height - 16)}px`;
      const height = element.getBoundingClientRect().height;
      const left = Math.max(bounds.left + 8, Math.min(trigger.left, bounds.right - width - 8));
      const below = trigger.bottom + 6;
      const top = below + height <= bounds.bottom - 8 ? below
        : Math.max(bounds.top + 8, Math.min(trigger.top - height - 6, bounds.bottom - height - 8));
      element.style.left = `${left}px`;
      element.style.top = `${top}px`;
    }
    // The dialog leaves the panel's content height unchanged. The native resize
    // guard holds its current size while the explanation is open.
    if (!element.open) element.showModal();
    place();
    closeButton.current?.focus({ preventScroll: true });
    const observer = new ResizeObserver(place);
    observer.observe(shell);
    observer.observe(element);
    window.addEventListener("resize", place);
    return () => { observer.disconnect(); window.removeEventListener("resize", place); };
  }, [anchor]);

  return <dialog ref={dialog} id={id} className={`pace-popup ${provider.id}`} aria-labelledby={`${id}-title`}
    onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}
    onCancel={(event) => { event.preventDefault(); onClose(true); }}
    onClose={(event) => onClose(event.currentTarget.returnValue !== "dismissed")}
    onClick={(event) => {
      if (event.target !== event.currentTarget) return;
      const bounds = event.currentTarget.getBoundingClientRect();
      if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) onClose(true);
    }}>
    <div className="pace-popup-header">
      <h3 id={`${id}-title`} title={`${providerNames[provider.id]} · ${limit.label}`}>
        {providerNames[provider.id]} · {limit.label}<span className="pace-visually-hidden"> pace</span>
      </h3>
      <button ref={closeButton} type="button" className="pace-popup-close" aria-label="Close pace explanation" onClick={() => onClose(true)}><Icon name="close" /></button>
    </div>
    <div className="pace-popup-body" tabIndex={0} role="region" aria-label="Pace explanation">
      <p className="pace-explanation">{paceExplanation(pace)}</p>
      <div className="pace-budget">
        <span>{pace.usedFraction === 1 ? "Until reset" : "Spread across that time"}</span>
        <strong>{paceBudget(pace)}</strong>
      </div>
      <PaceChart pace={pace} limit={limit} provider={provider} historyEnabled={historyEnabled} />
      <details className="pace-help" onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}>
        <summary>How to read pace</summary>
        <p>The dashed line shows allowance used evenly between resets, including nights and weekends. Recorded marks show your account’s readings.</p>
        <p>Model choice, reasoning effort and task size affect how much work fits in the allowance left.</p>
        {paceStatus(pace).tone === "early" && <p>This window has just started. Pace warnings wait until more time has passed.</p>}
      </details>
      <p className="pace-sample-time">At last reading · {new Date(pace.observedAt * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</p>
    </div>
  </dialog>;
}

export function PaceGuide({ pace, limit, provider, historyEnabled, compact = false }: {
  pace: AllowancePace; limit: Limit; provider: ProviderState; historyEnabled: boolean; compact?: boolean;
}) {
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const shell = useRef<HTMLElement | null>(null);
  const restoreTrigger = useRef(false);
  const [open, setOpen] = useState(false);
  const ready = pace.state === "ready";

  useEffect(() => {
    if (!ready) {
      restoreTrigger.current = false;
      if (open && shell.current?.isConnected) shell.current.focus({ preventScroll: true });
      setOpen(false);
    }
  }, [ready, open]);
  useEffect(() => {
    if (open || !restoreTrigger.current) return;
    restoreTrigger.current = false;
    if (trigger.current?.isConnected && !trigger.current.disabled) trigger.current.focus({ preventScroll: true });
  }, [open]);

  if (!ready) {
    if (pace.reason === "unsupported") return null;
    return <p className={`pace-unavailable${compact ? " compact" : ""}`}>{paceUnavailable(pace.reason)}</p>;
  }
  const status = paceStatus(pace);
  return (
    <div className={`pace-guide${compact ? " compact" : ""}`} data-pace={status.tone}>
      <button ref={trigger} type="button" className="pace-trigger" aria-haspopup="dialog" aria-expanded={open}
        aria-controls={open ? id : undefined} aria-label={`${providerNames[provider.id]} ${limit.label}: ${status.label}. Explain pace`}
        onClick={() => { shell.current = trigger.current?.closest<HTMLElement>(".popover-shell") ?? null; setOpen(true); }}>
        <span className="pace-caption">Pace</span>
        <span className="pace-status">{status.label}</span>
        <span className="pace-info" aria-hidden="true">i</span>
      </button>
      {open && trigger.current && <PacePopup pace={pace} limit={limit} provider={provider} historyEnabled={historyEnabled} anchor={trigger.current} id={id}
        onClose={(restoreFocus) => { restoreTrigger.current = restoreFocus; setOpen(false); }} />}
    </div>
  );
}
