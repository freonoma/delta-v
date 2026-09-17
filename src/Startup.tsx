import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createLoginItemPreview } from "./preview";
import type { LoginItemState } from "./types";

const native = isTauri();
const preview = import.meta.env.DEV && !native;

function failureMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Could not change launch at login. Please try again.";
}

export function useLoginItem(onDismiss: () => void) {
  const [state, setState] = useState<LoginItemState | null>(() => preview ? createLoginItemPreview(window.location.search) : null);
  const [reading, setReading] = useState(native);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showApproval, setShowApproval] = useState(false);
  const operation = useRef(false);
  const readVersion = useRef(0);
  const mounted = useRef(false);

  const readState = useCallback(async () => {
    if (!native) return;
    const version = ++readVersion.current;
    setReading(true);
    try {
      const next = await invoke<LoginItemState>("get_login_item_state");
      if (mounted.current && version === readVersion.current) {
        setState(next);
        setError(null);
      }
    } catch (caught: unknown) {
      if (mounted.current && version === readVersion.current) {
        setState(null);
        setError(failureMessage(caught));
      }
    } finally {
      if (mounted.current && version === readVersion.current) setReading(false);
    }
  }, []);

  const refresh = useCallback(() => {
    if (!operation.current) void readState();
  }, [readState]);

  useEffect(() => {
    mounted.current = true;
    refresh();
    window.addEventListener("focus", refresh);
    let disposed = false;
    let unlisten: (() => void) | undefined;
    if (native) void listen("popover-reset", refresh).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    }).catch((caught: unknown) => { if (!disposed) setError(failureMessage(caught)); });
    return () => {
      disposed = true;
      mounted.current = false;
      readVersion.current += 1;
      window.removeEventListener("focus", refresh);
      unlisten?.();
    };
  }, [refresh]);

  async function act(action: "enable" | "disable" | "dismiss" | "open") {
    if (operation.current || (!native && !preview)) return;
    operation.current = true;
    readVersion.current += 1;
    setReading(false);
    setPending(true);
    setError(null);
    try {
      if (action === "open") {
        if (native) await invoke("open_login_item_settings");
      } else if (action === "dismiss") {
        if (native) await invoke("dismiss_launch_at_login_prompt");
        onDismiss();
        setShowApproval(false);
      } else {
        let next: LoginItemState;
        if (preview) {
          const result = new URLSearchParams(window.location.search).get("startup_result");
          if (result === "error") throw new Error("macOS could not change launch at login. Please try again.");
          if (result === "read_error") {
            setState(null);
            throw new Error("Could not confirm the macOS setting. Please try again.");
          }
          next = { status: action === "disable" ? "disabled" : result === "approval_required" ? "approval_required" : "enabled", reason: null };
          if (result === "save_error") {
            setState(next);
            throw new Error("The macOS setting changed, but Delta-V could not save your choice. Please try again.");
          }
        } else {
          next = await invoke<LoginItemState>("set_launch_at_login", { enabled: action === "enable" });
        }
        setState(next);
        onDismiss();
        setShowApproval(next.status === "approval_required");
      }
    } catch (caught: unknown) {
      // Registration can succeed even if saving the prompt choice fails.
      if (action === "enable" || action === "disable") await readState();
      setError(failureMessage(caught));
    } finally {
      operation.current = false;
      setPending(false);
    }
  }

  return { state, reading, pending, error, showApproval, refresh, act };
}

export function Startup({ login, mode, dismissed }: {
  login: ReturnType<typeof useLoginItem>;
  mode: "prompt" | "settings";
  dismissed: boolean;
}) {
  const { state, reading, pending, error, showApproval, refresh, act } = login;
  const approval = state?.status === "approval_required";
  const busy = reading || pending;

  useEffect(() => {
    if (mode === "settings") refresh();
  }, [mode, refresh]);

  if (mode === "prompt") {
    const offer = !dismissed && (state?.status === "disabled" || error !== null);
    if (!offer && !(showApproval && approval)) return null;
    const enabled = state?.status === "enabled";
    return (
      <section className="startup-prompt" aria-label="Launch at login">
        <h2>{enabled ? "Launch at login is on" : approval ? "Allow launch at login" : "Launch Delta-V at login?"}</h2>
        <p>{enabled ? "Delta-V will open quietly in the menu bar. You can change this in Settings." : approval ? "macOS needs your approval. Allow Delta-V in Login Items to finish." : "Keep your usage in the menu bar whenever you start your Mac. You can change this later in Settings."}</p>
        {error && <p className="startup-error" role="alert">{error}</p>}
        <div className="startup-actions">
          <button className="text-button" disabled={busy} onClick={() => void act("dismiss")}>{enabled ? "Done" : "Not now"}</button>
          {!enabled && (approval || state?.status === "disabled" ? <button className="primary-button" disabled={busy} onClick={() => void act(approval ? "open" : "enable")}>
            {pending ? "Please wait" : approval ? "Open Login Items" : "Enable"}
          </button> : <button className="primary-button" disabled={busy} onClick={refresh}>Try again</button>)}
        </div>
      </section>
    );
  }

  const statusText = reading ? "Checking macOS…" : state?.status === "enabled" ? "Starts quietly in your menu bar."
    : approval ? "Waiting for approval in macOS."
    : state?.status === "unavailable" ? state.reason
    : state?.status === "disabled" ? "Open Delta-V automatically when you log in to your Mac."
    : "Could not read the macOS setting.";

  return (
    <section className="startup-settings" aria-label="Startup">
      <h3>Startup</h3>
      <div className="startup-setting-row">
        <div><span id="launch-at-login-label">Launch at login</span><p id="launch-at-login-description">{statusText}</p></div>
        <button type="button" role="switch" className="startup-switch" aria-labelledby="launch-at-login-label"
          aria-describedby="launch-at-login-description" aria-checked={state?.status === "enabled"}
          disabled={busy || !state || state.status === "unavailable" || approval}
          onClick={() => void act(state?.status === "enabled" ? "disable" : "enable")}><span /></button>
      </div>
      {approval && <div className="startup-actions">
        <button className="text-button" disabled={busy} onClick={() => void act("disable")}>Cancel request</button>
        <button className="text-button" disabled={busy} onClick={() => void act("open")}>Open Login Items</button>
      </div>}
      {error && <p className="startup-error" role="alert">{error}</p>}
      {!state && !busy && <button className="text-button" onClick={refresh}>Try again</button>}
      <p className="startup-caption">Changes apply immediately.</p>
    </section>
  );
}
