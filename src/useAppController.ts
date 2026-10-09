import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppState, DisplayPreferences, HistoryState, PanelPreferences, PanelPreferencesState, ProviderId, ProviderSelection, ReconnectAction, RecoveryPhase } from "./types";
import { createPreview } from "./preview";
import { latestSnapshot, previewDisplayPreferences, previewPanelPreferences, previewProviderSelection } from "./preference-state";
import { providerEnabled, signInBlocked } from "./provider-display";
import { errorMessage, native, preview } from "./ui-state";
import { useLoginItem } from "./Startup";
import { useUpdates } from "./useUpdates";

export const defaultPanelPreferences: PanelPreferences = { pinned: false, mini: false, expanded: false, layout: "columns" };

// Shared data and commands have no usage-panel sizing, focus or dismissal behavior.
export function useAppController() {
  const [state, setState] = useState<AppState | null>(() => preview ? createPreview(window.location.search) : null);
  const [error, setError] = useState<string | null>(null);
  const [panelPreferences, setPanelPreferences] = useState<PanelPreferences | null>(preview ? defaultPanelPreferences : null);
  const panelPreferencesRef = useRef(panelPreferences);
  const panelSnapshotRef = useRef<PanelPreferencesState | null>(preview
    ? { revision: 0, preferences: defaultPanelPreferences, error: null } : null);
  const panelCommandPending = useRef(false);
  const panelReadGeneration = useRef(0);
  const [panelPending, setPanelPending] = useState(false);
  const [saving, setSaving] = useState(false);
  const [pendingRecovery, setPendingRecovery] = useState<Partial<Record<ProviderId, RecoveryPhase>>>({});
  const [pendingConnection, setPendingConnection] = useState<Partial<Record<ProviderId, boolean>>>({});
  const [now, setNow] = useState(Math.floor(Date.now() / 1000));
  const recoveryCommands = useRef(new Set<ProviderId>());
  const connectionCommands = useRef(new Set<ProviderId>());
  const previewTimers = useRef<Partial<Record<ProviderId, number>>>({});
  const panelReady = state !== null && panelPreferences !== null;
  const panelBusy = !panelReady || panelPending;
  const applyAppState = useCallback((next: AppState) => {
    setState((current) => latestSnapshot(current, next));
  }, []);
  const applyPanelState = useCallback((next: PanelPreferencesState) => {
    const accepted = latestSnapshot(panelSnapshotRef.current, next);
    if (accepted !== next) return accepted.preferences;
    const previousError = panelSnapshotRef.current?.error;
    panelSnapshotRef.current = next;
    panelPreferencesRef.current = next.preferences;
    setPanelPreferences(next.preferences);
    setError((current) => next.error ?? (current === previousError ? null : current));
    return next.preferences;
  }, []);
  const dismissStartupPrompt = useCallback(() => {
    // Native preference changes arrive in revisioned usage-updated snapshots.
    if (!preview) return;
    setState((current) => current ? {
      ...current, settings: { ...current.settings, launch_at_login_prompt_dismissed: true },
    } : current);
  }, []);
  const login = useLoginItem(dismissStartupPrompt);
  const updates = useUpdates();
  const updateHistoryPreferences = useCallback((history: HistoryState) => {
    if (!preview) return;
    setState((current) => {
      if (!current || (current.settings.history_recording === history.recording && current.settings.history_retention === history.retention)) return current;
      return { ...current, settings: { ...current.settings, history_recording: history.recording, history_retention: history.retention } };
    });
  }, []);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    const recoveryTimers = previewTimers.current;
    return () => {
      window.clearInterval(timer);
      for (const recoveryTimer of Object.values(recoveryTimers)) window.clearTimeout(recoveryTimer);
    };
  }, []);

  useEffect(() => {
    if (!native) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let unlistenPanel: (() => void) | undefined;
    void listen<PanelPreferencesState>("panel-preferences-updated", (event) => {
      if (!disposed) applyPanelState(event.payload);
    }).then(async (stop) => {
      if (disposed) { stop(); return; }
      unlistenPanel = stop;
      const saved = await invoke<PanelPreferencesState>("get_panel_preferences");
      if (!disposed) applyPanelState(saved);
    }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    void listen<AppState>("usage-updated", (event) => {
      if (!disposed) applyAppState(event.payload);
    }).then(async (stop) => {
      if (disposed) { stop(); return; }
      unlisten = stop;
      const initial = await invoke<AppState>("get_state");
      if (!disposed) applyAppState(initial);
    }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    return () => { disposed = true; unlisten?.(); unlistenPanel?.(); };
  }, [applyAppState, applyPanelState]);

  const savePanelPreferences = useCallback(async (changes: Partial<PanelPreferences>) => {
    const current = panelPreferencesRef.current;
    if (!current) throw new Error("Panel preferences are still loading. Try again in a moment.");
    if (panelCommandPending.current) throw new Error("Wait for the panel change to finish, then try again.");
    panelCommandPending.current = true;
    setPanelPending(true);
    setError(null);
    try {
      const saved = preview ? {
        revision: (panelSnapshotRef.current?.revision ?? 0) + 1,
        preferences: previewPanelPreferences(current, changes), error: null,
      } : await invoke<PanelPreferencesState>("save_panel_preferences", { patch: changes });
      return applyPanelState(saved);
    } finally {
      panelCommandPending.current = false;
      setPanelPending(false);
    }
  }, [applyPanelState]);

  const changePanelPreferences = useCallback(async (changes: Partial<PanelPreferences>) => {
    if (panelCommandPending.current || !panelPreferencesRef.current) return false;
    try {
      await savePanelPreferences(changes);
      return true;
    } catch (caught: unknown) {
      setError(errorMessage(caught));
      return false;
    }
  }, [savePanelPreferences]);

  const saveDisplayPreferences = useCallback(async (preferences: DisplayPreferences) => {
    setSaving(true);
    try {
      if (preview) setState((current) => current ? {
        ...current, revision: current.revision + 1,
        settings: previewDisplayPreferences(current.settings, preferences),
      } : current);
      else {
        const saved = await invoke<AppState>("save_display_preferences", { preferences });
        applyAppState(saved);
      }
      setError(null);
    } finally {
      setSaving(false);
    }
  }, [applyAppState]);

  async function selectProviders(providers: ProviderSelection) {
    if (!state || saving || panelBusy) return;
    setSaving(true);
    try {
      if (preview) setState((current) => current ? {
        ...current, revision: current.revision + 1,
        settings: previewProviderSelection(current.settings, providers),
      } : current);
      else applyAppState(await invoke<AppState>("set_provider_selection", { providers }));
      setError(null);
    }
    catch (caught: unknown) { setError(errorMessage(caught)); }
    finally { setSaving(false); }
  }

  async function refresh() {
    if (recoveryCommands.current.size > 0 || connectionCommands.current.size > 0 || state?.providers.some((provider) => provider.recovery !== null)) return;
    setError(null);
    if (preview) {
      setState((current) => current ? { ...current, providers: current.providers.map((provider) => ({ ...provider, snapshot: providerEnabled(current.settings, provider.id) && provider.snapshot ? { ...provider.snapshot, fetched_at: Math.floor(Date.now() / 1000) } : null })) } : current);
      return;
    }
    try { await invoke("refresh_usage"); }
    catch (caught: unknown) { setError(errorMessage(caught)); }
  }

  function setPreviewRecovery(providerId: ProviderId, recovery: RecoveryPhase | null) {
    setState((current) => current ? {
      ...current,
      providers: current.providers.map((provider) => provider.id === providerId ? { ...provider, recovery } : provider),
    } : current);
  }

  function simulateRecovery(providerId: ProviderId, action: ReconnectAction) {
    setState((current) => current ? {
      ...current, providers: current.providers.map((provider) => provider.id === providerId ? {
        ...provider, snapshot: null, error: null, recovery: action === "renew" ? "renewing" : "signing_in", stale: true,
      } : provider),
    } : current);
    previewTimers.current[providerId] = window.setTimeout(() => {
      setPreviewRecovery(providerId, "checking");
      previewTimers.current[providerId] = window.setTimeout(() => {
        const sample = createPreview().providers.find((provider) => provider.id === providerId);
        if (sample?.snapshot) {
          const connected = { ...sample, snapshot: { ...sample.snapshot, fetched_at: Math.floor(Date.now() / 1000) } };
          setState((current) => current && providerEnabled(current.settings, providerId) ? {
            ...current,
            providers: current.providers.map((provider) => provider.id === providerId ? connected : provider),
          } : current);
        }
        delete previewTimers.current[providerId];
      }, 900);
    }, action === "renew" ? 1500 : 5000);
  }

  function clearPendingRecovery(provider: ProviderId) {
    recoveryCommands.current.delete(provider);
    setPendingRecovery((current) => {
      const next = { ...current };
      delete next[provider];
      return next;
    });
  }

  async function reconnect(provider: ProviderId, action: ReconnectAction) {
    const current = state?.providers.find((candidate) => candidate.id === provider);
    if (!state || !current || !providerEnabled(state.settings, provider) || state.paused || current.recovery || current.refreshing
      || recoveryCommands.current.has(provider) || connectionCommands.current.has(provider)) return;
    if (action === "sign_in" && signInBlocked(current, now)) return;
    recoveryCommands.current.add(provider);
    setPendingRecovery((pending) => ({ ...pending, [provider]: action === "renew" ? "renewing" : "signing_in" }));
    setError(null);
    try {
      if (preview) simulateRecovery(provider, action);
      else if (native) await invoke("reconnect_provider", { provider, action });
    } catch (caught: unknown) {
      setError(errorMessage(caught));
    } finally {
      clearPendingRecovery(provider);
    }
  }

  async function cancelReconnect(provider: ProviderId) {
    const current = state?.providers.find((candidate) => candidate.id === provider);
    if (!current?.recovery || current.recovery === "checking" || current.recovery === "cancelling" || recoveryCommands.current.has(provider)) return;
    recoveryCommands.current.add(provider);
    setPendingRecovery((pending) => ({ ...pending, [provider]: "cancelling" }));
    setError(null);
    try {
      if (preview) {
        window.clearTimeout(previewTimers.current[provider]);
        setPreviewRecovery(provider, "cancelling");
        previewTimers.current[provider] = window.setTimeout(() => {
          setState((currentState) => currentState ? {
            ...currentState, providers: currentState.providers.map((candidate) => candidate.id === provider ? {
              ...candidate, recovery: null,
              error: providerEnabled(currentState.settings, provider) ? { kind: "recovery", message: "Sign-in was cancelled. Reconnect when you are ready." } : null,
            } : candidate),
          } : currentState);
          delete previewTimers.current[provider];
        }, 300);
      } else if (native) await invoke("cancel_reconnect", { provider });
    } catch (caught: unknown) {
      setError(errorMessage(caught));
    } finally {
      clearPendingRecovery(provider);
    }
  }

  async function setProviderEnabled(providerId: ProviderId, enabled: boolean) {
    if (!state || providerEnabled(state.settings, providerId) === enabled || connectionCommands.current.has(providerId)) return;
    const provider = state.providers.find((candidate) => candidate.id === providerId);
    if (!provider || (enabled && provider.recovery !== null)) return;
    connectionCommands.current.add(providerId);
    setPendingConnection((pending) => ({ ...pending, [providerId]: true }));
    setError(null);
    try {
      if (preview) {
        window.clearTimeout(previewTimers.current[providerId]);
        delete previewTimers.current[providerId];
        const sample = createPreview().providers.find((candidate) => candidate.id === providerId);
        const recovering = provider.recovery !== null;
        setState((current) => current ? {
          ...current,
          settings: { ...current.settings, [providerId === "claude" ? "claude_enabled" : "codex_enabled"]: enabled },
          providers: current.providers.map((candidate) => candidate.id !== providerId ? candidate : enabled && sample ? sample : {
            ...candidate, snapshot: null, error: null, refreshing: false, stale: true,
            next_retry_at: null, recovery: recovering ? "cancelling" : null,
          }),
        } : current);
        if (recovering) previewTimers.current[providerId] = window.setTimeout(() => {
          setPreviewRecovery(providerId, null);
          delete previewTimers.current[providerId];
        }, 300);
      } else if (native) await invoke("set_provider_enabled", { provider: providerId, enabled });
    } catch (caught: unknown) {
      setError(errorMessage(caught));
    } finally {
      connectionCommands.current.delete(providerId);
      setPendingConnection((pending) => {
        const next = { ...pending };
        delete next[providerId];
        return next;
      });
    }
  }

  async function retryInitialRead() {
    const panelRead = ++panelReadGeneration.current;
    setError(null);
    try {
      const [initial, saved] = await Promise.all([
        invoke<AppState>("get_state"), invoke<PanelPreferencesState>("get_panel_preferences"),
      ]);
      if (panelRead !== panelReadGeneration.current) return;
      applyAppState(initial);
      applyPanelState(saved);
    } catch (caught: unknown) {
      if (panelRead === panelReadGeneration.current) setError(errorMessage(caught));
    }
  }

  return {
    state, error, setError, now, login, updates,
    panelPreferences, panelReady, panelBusy, panelCommandPending,
    saving, pendingRecovery, pendingConnection,
    saveDisplayPreferences, selectProviders, savePanelPreferences, changePanelPreferences,
    updateHistoryPreferences, refresh, reconnect, cancelReconnect, setProviderEnabled, retryInitialRead,
  };
}
