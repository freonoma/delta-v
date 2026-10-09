import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AccountsSettings } from "./AccountControls";
import { DisplaySettings } from "./DisplaySettings";
import { HistorySettings } from "./HistorySettings";
import { Startup } from "./Startup";
import { Updates } from "./Updates";
import { BrandMark } from "./BrandMark";
import { Icon } from "./Icon";
import { useAppController } from "./useAppController";
import { createSettingsDraft, isSettingsDraftDirty, reconcileSettingsDraft, validateSettingsDraft } from "./settings-draft";
import type { SettingsDraft } from "./settings-draft";
import { isSettingsSection, settingsSections } from "./settings-navigation";
import type { SettingsSection } from "./settings-navigation";
import type { AppState, PanelPreferences, Theme } from "./types";
import { errorMessage, native, preview } from "./ui-state";

const sectionTitles: Record<SettingsSection, string> = {
  display: "Display & usage", accounts: "Accounts", history: "History", app: "App",
};
const sectionDescriptions: Record<SettingsSection, string> = {
  display: "Choose usage windows, percentages and appearance.",
  accounts: "Manage the accounts Delta-V checks for usage.",
  history: "Manage the readings saved on this Mac.",
  app: "Launch at login and check for updates.",
};
type CloseIntent = "close" | "back" | "quit";

function UnsavedChanges({ intent, busy, onKeepEditing, onDiscard, onSave }: {
  intent: CloseIntent; busy: boolean; onKeepEditing: () => void; onDiscard: () => void; onSave: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    const previousFocus = document.activeElement;
    if (element && !element.open) element.showModal();
    return () => {
      element?.close();
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) previousFocus.focus();
    };
  }, []);
  return (
    <dialog ref={dialog} className="disconnect-dialog settings-unsaved-dialog"
      aria-labelledby="unsaved-title" aria-describedby="unsaved-description"
      onCancel={(event) => { event.preventDefault(); if (!busy) onKeepEditing(); }}
      onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}>
      <h3 id="unsaved-title">Save changes before {intent === "quit" ? "quitting" : intent === "back" ? "returning to the panel" : "closing"}?</h3>
      <p id="unsaved-description">Your display changes have not been saved. Account, history and launch-at-login changes already apply.</p>
      <div className="settings-dialog-actions">
        <button className="text-button" autoFocus disabled={busy} onClick={onKeepEditing}>Keep editing</button>
        <button className="text-button" disabled={busy} onClick={onDiscard}>Discard changes</button>
        <button className="primary-button" disabled={busy} onClick={onSave}>Save changes</button>
      </div>
    </dialog>
  );
}

function SettingsEditor({ controller, state, panelPreferences, section, setSection, closeGuard, finishClose, onThemePreview }: {
  controller: ReturnType<typeof useAppController>;
  state: AppState;
  panelPreferences: PanelPreferences;
  section: SettingsSection;
  setSection: (section: SettingsSection) => void;
  closeGuard: RefObject<((intent: CloseIntent) => void) | null>;
  finishClose: (intent: CloseIntent) => Promise<void>;
  onThemePreview: (theme: Theme | null) => void;
}) {
  const { now, login, updates, saving, panelBusy, pendingRecovery, pendingConnection,
    saveDisplayPreferences, savePanelPreferences, updateHistoryPreferences, setProviderEnabled,
    reconnect, cancelReconnect } = controller;
  const [form, setForm] = useState(() => {
    const baseline = createSettingsDraft(state.settings, panelPreferences.layout);
    return { baseline, draft: baseline };
  });
  const [submitting, setSubmitting] = useState(false);
  const [validation, setValidation] = useState<string | null>(null);
  const [savedNotice, setSavedNotice] = useState(false);
  const [confirmClose, setConfirmClose] = useState<CloseIntent | null>(null);
  const operation = useRef(false);
  const pendingClose = useRef<CloseIntent | null>(null);
  const content = useRef<HTMLDivElement>(null);
  const busy = submitting || saving || panelBusy;
  const dirty = isSettingsDraftDirty(form.draft, form.baseline);

  // Polling and other windows can change saved values while this draft is open.
  // Keep edited fields, and let untouched fields follow the latest backend state.
  useEffect(() => {
    if (busy) return;
    const baseline = createSettingsDraft(state.settings, panelPreferences.layout);
    setForm((current) => isSettingsDraftDirty(baseline, current.baseline)
      ? { baseline, draft: reconcileSettingsDraft(current.draft, current.baseline, baseline) } : current);
  }, [state.settings, panelPreferences.layout, busy]);

  useEffect(() => { onThemePreview(form.draft.preferences.theme); }, [form.draft.preferences.theme, onThemePreview]);
  useEffect(() => () => onThemePreview(null), [onThemePreview]);
  useEffect(() => { content.current?.scrollTo({ top: 0 }); }, [section]);

  const requestClose = useCallback((intent: CloseIntent) => {
    if (operation.current || busy) {
      if (pendingClose.current !== "quit") pendingClose.current = intent;
      return;
    }
    if (dirty) setConfirmClose(intent);
    else void finishClose(intent);
  }, [busy, dirty, finishClose]);
  useEffect(() => {
    if (!busy && pendingClose.current) {
      const intent = pendingClose.current;
      pendingClose.current = null;
      requestClose(intent);
    }
  }, [busy, requestClose]);
  useLayoutEffect(() => {
    closeGuard.current = requestClose;
    return () => { closeGuard.current = null; };
  }, [closeGuard, requestClose]);

  function edit(patch: Partial<SettingsDraft>) {
    setSavedNotice(false);
    setValidation(null);
    setForm((current) => ({ ...current, draft: { ...current.draft, ...patch } }));
  }

  async function save(closeAfter?: CloseIntent) {
    if (operation.current || busy) return;
    setConfirmClose(null);
    const result = validateSettingsDraft(form.draft, state.settings.providers);
    if (result.error !== null) {
      setValidation(result.error);
      setSection("display");
      window.requestAnimationFrame(() => content.current?.querySelector<HTMLInputElement>(`input[name="${result.field}"]`)?.focus());
      return;
    }
    operation.current = true;
    setSubmitting(true);
    setValidation(null);
    setSavedNotice(false);
    try {
      await saveDisplayPreferences(result.preferences);
      await savePanelPreferences({ layout: result.layout });
      const saved = createSettingsDraft(result.preferences, result.layout);
      setForm({ baseline: saved, draft: saved });
      setSavedNotice(true);
      if (closeAfter) {
        const intent = pendingClose.current === "quit" ? "quit" : closeAfter;
        pendingClose.current = null;
        await finishClose(intent);
      }
    } catch (caught: unknown) {
      setValidation(errorMessage(caught));
      setSection("display");
    } finally {
      operation.current = false;
      setSubmitting(false);
    }
  }

  function discard() {
    const baseline = createSettingsDraft(state.settings, panelPreferences.layout);
    setForm({ baseline, draft: baseline });
    setValidation(null);
    setSavedNotice(false);
  }

  return (
    <>
      <div ref={content} className="settings-section-content">
        {controller.error && <p className="notice global-error" role="alert">{controller.error}</p>}
        {state.settings_error && <p className="notice global-error" role="status">{state.settings_error}</p>}
        <section role="tabpanel" id="settings-panel-display" aria-labelledby="settings-tab-display" hidden={section !== "display"}>
          <fieldset className="settings-display-fields" disabled={busy}>
            <legend className="visually-hidden">Display preferences</legend>
            <DisplaySettings state={state} now={now} draft={form.draft.preferences}
              setDraft={(preferences) => edit({ preferences })} layoutDraft={form.draft.layout}
              setLayoutDraft={(layout) => edit({ layout })} threshold={form.draft.threshold}
              setThreshold={(threshold) => edit({ threshold })} interval={form.draft.interval}
              setIntervalValue={(interval) => edit({ interval })} busy={busy} onThemePreview={onThemePreview} />
          </fieldset>
        </section>
        <section role="tabpanel" id="settings-panel-accounts" aria-labelledby="settings-tab-accounts" hidden={section !== "accounts"}>
          {section === "accounts" && <AccountsSettings state={state} now={now}
            pendingRecovery={pendingRecovery} pendingConnection={pendingConnection}
            onSetEnabled={(id, enabled) => void setProviderEnabled(id, enabled)}
            onReconnect={(id, action) => void reconnect(id, action)} onCancel={(id) => void cancelReconnect(id)} />}
        </section>
        <section role="tabpanel" id="settings-panel-history" aria-labelledby="settings-tab-history" hidden={section !== "history"}>
          {section === "history" && <HistorySettings settings={state.settings} onChange={updateHistoryPreferences} />}
        </section>
        <section role="tabpanel" id="settings-panel-app" aria-labelledby="settings-tab-app" hidden={section !== "app"}>
          {section === "app" && <>
            <Startup login={login} mode="settings" dismissed={state.settings.launch_at_login_prompt_dismissed} />
            <Updates updates={updates} now={now} />
          </>}
        </section>
      </div>
      <footer className="settings-window-footer">
        <p className="settings-save-status" role={validation ? "alert" : "status"}>{validation ?? (busy ? "Saving display changes…" : dirty ? "Unsaved display changes" : savedNotice ? "Display changes saved" : "Only display changes need saving.")}</p>
        <div className="settings-footer-actions">
          <button className="text-button" disabled={busy || !dirty} onClick={discard}>Discard changes</button>
          <button className="primary-button" disabled={busy || !dirty} onClick={() => void save()}>Save changes</button>
        </div>
      </footer>
      {confirmClose && <UnsavedChanges intent={confirmClose} busy={busy} onKeepEditing={() => setConfirmClose(null)}
        onDiscard={() => { discard(); setConfirmClose(null); void finishClose(confirmClose); }}
        onSave={() => void save(confirmClose)} />}
    </>
  );
}

export function SettingsWindow() {
  const controller = useAppController();
  const { state, panelPreferences, panelReady, setError } = controller;
  const [section, setSection] = useState<SettingsSection>(() => {
    const requested = new URLSearchParams(window.location.search).get("section");
    return isSettingsSection(requested) ? requested : "display";
  });
  const [themePreview, setThemePreview] = useState<Theme | null>(null);
  const closeGuard = useRef<((intent: CloseIntent) => void) | null>(null);
  const theme = themePreview ?? state?.settings.theme ?? "system";
  const hasState = state !== null;

  const finishClose = useCallback(async (intent: CloseIntent) => {
    try {
      if (native) {
        if (intent === "quit") await invoke("confirm_quit_app");
        else await invoke("close_settings_window", { showPanel: intent === "back" });
      }
      else if (preview) {
        const parameters = new URLSearchParams(window.location.search);
        parameters.delete("view");
        parameters.delete("section");
        window.location.assign(`?${parameters}`);
      }
    } catch (caught: unknown) { setError(errorMessage(caught)); }
  }, [setError]);
  const requestClose = useCallback((intent: CloseIntent = "close") => {
    if (closeGuard.current) closeGuard.current(intent);
    else void finishClose(intent);
  }, [finishClose]);

  useEffect(() => {
    if (!native) return;
    let disposed = false;
    void invoke("set_settings_section", { section })
      .catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    return () => { disposed = true; };
  }, [section, setError]);

  useEffect(() => {
    if (!native || !hasState) return;
    let disposed = false;
    void invoke("set_settings_appearance", { theme })
      .catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    return () => { disposed = true; };
  }, [theme, hasState, setError]);

  useEffect(() => {
    if (!native) return;
    let disposed = false;
    let stopClose: (() => void) | undefined;
    let stopSection: (() => void) | undefined;
    let stopQuit: (() => void) | undefined;
    const takeQuitRequest = async () => {
      try {
        if (!disposed && await invoke<boolean>("take_settings_quit_request") && !disposed) requestClose("quit");
      } catch (caught: unknown) { if (!disposed) setError(errorMessage(caught)); }
    };
    void getCurrentWindow().onCloseRequested((event) => {
      event.preventDefault();
      if (!disposed) requestClose();
    }).then((stop) => { if (disposed) stop(); else stopClose = stop; })
      .catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    void listen<unknown>("settings-section-requested", (event) => {
      if (!disposed && isSettingsSection(event.payload)) setSection(event.payload);
    }).then((stop) => { if (disposed) stop(); else stopSection = stop; })
      .catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    void listen("settings-quit-requested", () => void takeQuitRequest()).then((stop) => {
      if (disposed) stop();
      else { stopQuit = stop; void takeQuitRequest(); }
    }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    return () => { disposed = true; stopClose?.(); stopSection?.(); stopQuit?.(); };
  }, [requestClose, setError]);

  useEffect(() => {
    const keyDown = (event: KeyboardEvent) => {
      if (event.metaKey && event.key === ",") event.preventDefault();
      if (event.key === "Escape" && !event.defaultPrevented && !document.querySelector("dialog[open]")) {
        event.preventDefault();
        requestClose();
      }
    };
    window.addEventListener("keydown", keyDown);
    return () => window.removeEventListener("keydown", keyDown);
  }, [requestClose]);

  return (
    <div className="settings-window" data-theme={theme}>
      <aside className="settings-sidebar">
        <div className="brand settings-brand"><BrandMark /><h1>Delta-V</h1></div>
        <p className="settings-sidebar-title">Settings</p>
        <nav aria-label="Settings sections">
          <div role="tablist" aria-label="Settings sections" aria-orientation="vertical" onKeyDown={(event) => {
            const step = event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
            if (!step && event.key !== "Home" && event.key !== "End") return;
            event.preventDefault();
            const next = event.key === "Home" ? "display" : event.key === "End" ? "app"
              : settingsSections[(settingsSections.indexOf(section) + step + settingsSections.length) % settingsSections.length];
            if (next) {
              setSection(next);
              document.getElementById(`settings-tab-${next}`)?.focus();
            }
          }}>
            {settingsSections.map((id) => <button key={id} role="tab" id={`settings-tab-${id}`}
              aria-selected={section === id} aria-controls={`settings-panel-${id}`} tabIndex={section === id ? 0 : -1}
              onClick={() => setSection(id)}>{sectionTitles[id]}</button>)}
          </div>
        </nav>
        <button className="footer-button settings-back" onClick={() => requestClose("back")}>
          <Icon name="back" />Back to panel
        </button>
      </aside>
      <main className="settings-workspace">
        <header className="settings-section-header">
          <h2>{sectionTitles[section]}</h2>
          <p>{sectionDescriptions[section]}</p>
          {preview && <p>Sample data · Browser preview</p>}
        </header>
        {panelReady && state && panelPreferences ? <SettingsEditor controller={controller} state={state}
          panelPreferences={panelPreferences} section={section} setSection={setSection} closeGuard={closeGuard}
          finishClose={finishClose} onThemePreview={setThemePreview} /> : <div className="settings-section-content">
          <p role="status">{controller.error ? "Settings could not be loaded." : "Loading settings…"}</p>
          {controller.error && <><p className="settings-validation" role="alert">{controller.error}</p>
            <button className="text-button" onClick={() => void controller.retryInitialRead()}>Try again</button></>}
        </div>}
      </main>
    </div>
  );
}
