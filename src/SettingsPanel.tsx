import { useEffect, useRef, useState } from "react";
import type { AppState, DisplayPreferences, HistoryState, MiniLayout, ProviderId, ReconnectAction, RecoveryPhase, Theme } from "./types";
import { displayPreferences, visibleTrackedLimit } from "./preference-state";
import { AccountsSettings } from "./AccountControls";
import { DisplaySettings } from "./DisplaySettings";
import { HistorySettings } from "./HistorySettings";
import { Icon } from "./Icon";
import { Startup, type useLoginItem } from "./Startup";
import { Updates } from "./Updates";
import type { useUpdates } from "./useUpdates";
import { errorMessage } from "./ui-state";

export function SettingsPanel({ state, saving, now, pendingRecovery, pendingConnection, login, updates, miniLayout, onMiniLayoutChange, onHistoryChange, onSave, onClose, onThemePreview, onSetEnabled, onReconnect, onCancel }: {
  state: AppState;
  saving: boolean;
  now: number;
  pendingRecovery: Partial<Record<ProviderId, RecoveryPhase>>;
  pendingConnection: Partial<Record<ProviderId, boolean>>;
  login: ReturnType<typeof useLoginItem>;
  updates: ReturnType<typeof useUpdates>;
  miniLayout: MiniLayout;
  onMiniLayoutChange: (layout: MiniLayout) => Promise<void>;
  onHistoryChange: (history: HistoryState) => void;
  onSave: (preferences: DisplayPreferences) => Promise<void>;
  onClose: () => void;
  onThemePreview: (theme: Theme | null) => void;
  onSetEnabled: (provider: ProviderId, enabled: boolean) => void;
  onReconnect: (provider: ProviderId, action: ReconnectAction) => void;
  onCancel: (provider: ProviderId) => void;
}) {
  const panel = useRef<HTMLElement>(null);
  const [draft, setDraft] = useState(() => displayPreferences(state.settings));
  const [layoutDraft, setLayoutDraft] = useState(miniLayout);
  const [submitting, setSubmitting] = useState(false);
  const busy = saving || submitting;
  const [threshold, setThreshold] = useState(String(state.settings.threshold));
  const [interval, setIntervalValue] = useState(String(state.settings.refresh_seconds));
  const [validation, setValidation] = useState<string | null>(null);
  const tracked = visibleTrackedLimit(draft.tracked_limit, state.settings.providers);

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => panel.current?.scrollIntoView({ block: "start" }));
    return () => window.cancelAnimationFrame(frame);
  }, []);

  useEffect(() => () => onThemePreview(null), [onThemePreview]);

  async function save() {
    if (busy) return;
    const parsedThreshold = Number(threshold);
    const parsedInterval = Number(interval);
    if (!threshold.trim() || !Number.isInteger(parsedThreshold) || parsedThreshold < 0 || parsedThreshold > 100) {
      setValidation("Enter a whole-number remaining threshold from 0 to 100.");
      return;
    }
    if (!interval.trim() || !Number.isInteger(parsedInterval) || parsedInterval < 30 || parsedInterval > 900) {
      setValidation("Enter a refresh interval from 30 to 900 seconds.");
      return;
    }
    setValidation(null);
    setSubmitting(true);
    try {
      await onSave({
        ...displayPreferences(draft), tracked_limit: tracked, threshold: parsedThreshold,
        refresh_seconds: parsedInterval,
      });
      await onMiniLayoutChange(layoutDraft);
      onClose();
    } catch (error: unknown) {
      setValidation(errorMessage(error));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <section ref={panel} className="settings-panel" aria-label="Settings"
      onKeyDown={(event) => { if (busy && event.key === "Escape") event.stopPropagation(); }}>
      <div className="settings-heading">
        <h2>Settings</h2>
        <button className="icon-button" onClick={onClose} disabled={busy} aria-label="Close settings"><Icon name="close" /></button>
      </div>
      <DisplaySettings state={state} now={now} draft={draft} setDraft={setDraft}
        layoutDraft={layoutDraft} setLayoutDraft={setLayoutDraft} threshold={threshold} setThreshold={setThreshold}
        interval={interval} setIntervalValue={setIntervalValue} busy={busy} onThemePreview={onThemePreview} />
      {validation && <p className="settings-validation" role="alert">{validation}</p>}
      <div className="settings-actions">
        <button className="text-button" onClick={onClose} disabled={busy}>Cancel</button>
        <button className="primary-button" onClick={() => void save()} disabled={busy}>{busy ? "Saving" : "Save settings"}</button>
      </div>
      <Startup login={login} mode="settings" dismissed={state.settings.launch_at_login_prompt_dismissed} />
      <HistorySettings settings={state.settings} onChange={onHistoryChange} />
      <AccountsSettings state={state} now={now} pendingRecovery={pendingRecovery} pendingConnection={pendingConnection}
        onSetEnabled={onSetEnabled} onReconnect={onReconnect} onCancel={onCancel} />
      <Updates updates={updates} now={now} />
    </section>
  );
}
