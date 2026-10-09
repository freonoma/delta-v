import { useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createHistoryPreview } from "./preview";
import { getHistoryPreview } from "./history-preview";
import type { HistoryRetention, HistoryState, Settings } from "./types";

const native = isTauri();
const preview = import.meta.env.DEV && !native;

type HistoryAction = { kind: "recording"; enabled: boolean }
  | { kind: "retention"; retention: HistoryRetention }
  | { kind: "clear" };
type Confirmation = Exclude<HistoryAction, { kind: "recording" }>;

function failureMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Could not update usage history. Please try again.";
}

function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} bytes`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function HistoryConfirmation({ action, pending, error, onConfirm, onCancel }: {
  action: Confirmation;
  pending: boolean;
  error: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const clearing = action.kind === "clear";
  const days = action.kind === "retention" && action.retention === "days30" ? 30 : 90;

  useEffect(() => {
    const element = dialog.current;
    if (element && !element.open) element.showModal();
  }, []);

  return (
    <dialog ref={dialog} className="disconnect-dialog history-dialog" aria-labelledby="history-confirmation-title"
      aria-describedby="history-confirmation-description" aria-busy={pending}
      onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}
      onCancel={(event) => { event.preventDefault(); if (!pending) onCancel(); }}
      onClose={() => { if (!pending) onCancel(); }}>
      <h3 id="history-confirmation-title">{clearing ? "Clear saved usage history?" : `Keep only the last ${days} days?`}</h3>
      <p id="history-confirmation-description">{clearing
        ? "This deletes saved readings for both providers, including earlier accounts. Any exported files are kept. Your recording setting will not change. This cannot be undone."
        : `Readings older than ${days} days will be deleted for both providers, including earlier accounts. Any exported files are kept. This cannot be undone.`}</p>
      {error && <p className="history-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="text-button" autoFocus disabled={pending} onClick={onCancel}>Cancel</button>
        <button className="primary-button" disabled={pending} onClick={onConfirm}>
          {pending ? "Deleting" : clearing ? "Clear history" : `Keep ${days} days`}
        </button>
      </div>
    </dialog>
  );
}

export function HistorySettings({ settings, onChange }: {
  settings: Settings;
  onChange: (state: HistoryState) => void;
}) {
  const [state, setState] = useState<HistoryState | null>(() => preview ? createHistoryPreview(settings) : null);
  const [reading, setReading] = useState(native);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const [retry, setRetry] = useState(0);
  const mounted = useRef(false);
  const operation = useRef(false);
  const revision = useRef(0);
  const busy = reading || pending;

  useEffect(() => {
    mounted.current = true;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    if (native) {
      setReading(true);
      setError(null);
      void (async () => {
        let version: number | undefined;
        try {
          const stop = await listen<HistoryState>("history-state", (event) => {
            if (disposed) return;
            revision.current += 1;
            setState(event.payload);
            onChange(event.payload);
            setReading(false);
          });
          if (disposed) { stop(); return; }
          unlisten = stop;
          version = ++revision.current;
          const next = await invoke<HistoryState>("get_history_state");
          if (!disposed && version === revision.current) {
            setState(next);
            onChange(next);
          }
        } catch (caught: unknown) {
          if (!disposed && (version === undefined || version === revision.current)) setError(failureMessage(caught));
        } finally {
          if (!disposed) setReading(false);
        }
      })();
    }
    return () => {
      disposed = true;
      mounted.current = false;
      revision.current += 1;
      unlisten?.();
    };
  }, [onChange, retry]);

  async function act(action: HistoryAction): Promise<void> {
    if (operation.current || !state || (!native && !preview)) return;
    operation.current = true;
    const version = ++revision.current;
    setPending(true);
    setError(null);
    try {
      let next: HistoryState;
      if (preview) {
        const store = getHistoryPreview();
        if (action.kind === "clear") store.clear();
        else if (action.kind === "recording") store.setRecording(action.enabled);
        else store.setRetention(action.retention);
        next = store.state(settings);
      } else {
        next = action.kind === "clear" ? await invoke<HistoryState>("clear_history")
          : action.kind === "recording" ? await invoke<HistoryState>("set_history_recording", { enabled: action.enabled })
          : await invoke<HistoryState>("set_history_retention", { retention: action.retention });
      }
      if (mounted.current) {
        if (version === revision.current) {
          setState(next);
          onChange(next);
        }
        setConfirmation(null);
      }
    } catch (caught: unknown) {
      if (mounted.current) setError(failureMessage(caught));
    } finally {
      operation.current = false;
      if (mounted.current) setPending(false);
    }
  }

  function chooseRetention(value: string) {
    if (!state || busy || (value !== "forever" && value !== "days30" && value !== "days90") || value === state.retention) return;
    const action: Confirmation = { kind: "retention", retention: value };
    if (value !== "forever" && (state.retention === "forever" || (state.retention === "days90" && value === "days30"))) {
      setError(null);
      setConfirmation(action);
    } else void act(action);
  }

  const hasRecords = state !== null && (state.info.records > 0 || state.info.bytes > 0);
  const canClear = state !== null && (hasRecords || state.error !== null);
  const recordDescription = reading ? "Reading history settings."
    : !state ? "History settings could not be loaded."
    : state.recording ? "New quota readings are saved when usage and account details are available."
    : "Recording is off. The Keep history setting still applies.";
  const failure = error ?? state?.error;

  return (
    <section className="history-settings" aria-label="Usage history" aria-busy={busy}>
      <h3>Usage history</h3>
      <p className="history-introduction">Save readings from both connected providers, even when one is hidden. History stays on this Mac and contains no conversations or sign-in tokens.</p>
      <div className="startup-setting-row">
        <div><span id="history-recording-label">Record usage history</span><p id="history-recording-description">{recordDescription}</p></div>
        <button type="button" role="switch" className="startup-switch" aria-labelledby="history-recording-label"
          aria-describedby="history-recording-description" aria-checked={state?.recording ?? false}
          disabled={busy || !state} onClick={() => void act({ kind: "recording", enabled: !state?.recording })}><span /></button>
      </div>
      <label className="setting-row history-retention">
        <span>Keep history<small>Applies to both providers and earlier accounts</small></span>
        <select value={state?.retention ?? settings.history_retention} disabled={busy || !state} onChange={(event) => chooseRetention(event.target.value)}>
          <option value="forever">Until deleted</option>
          <option value="days30">30 days</option>
          <option value="days90">90 days</option>
        </select>
      </label>
      {state && <div className="history-storage">
        <p>{state.error ? "Could not confirm the saved history." : hasRecords ? `${state.info.records.toLocaleString()} saved ${state.info.records === 1 ? "reading" : "readings"} · ${fileSize(state.info.bytes)}` : "No readings saved yet."}</p>
        {state.info.last_recorded_at !== null && <p>Last saved {new Date(state.info.last_recorded_at * 1000).toLocaleString()}.</p>}
        <code>~/.local/share/delta-v/history/</code>
      </div>}
      {failure && !confirmation && <p className="history-error" role="alert">{failure}</p>}
      {state?.provider_issues.map((issue) => <p key={issue.provider} className="history-error" role="status">
        {issue.provider === "claude" ? "Claude" : "Codex"}: {issue.message}
      </p>)}
      <div className="history-actions">
        {(!state || failure) && <button className="text-button" disabled={busy} onClick={() => setRetry((value) => value + 1)}>Try again</button>}
        <button className="text-button" disabled={busy || !canClear} onClick={() => { setError(null); setConfirmation({ kind: "clear" }); }}>Clear history</button>
      </div>
      <p className="startup-caption">Changes apply immediately.</p>
      {confirmation && <HistoryConfirmation action={confirmation} pending={pending} error={error}
        onConfirm={() => void act(confirmation)} onCancel={() => { if (!operation.current) { setConfirmation(null); setError(null); } }} />}
    </section>
  );
}
