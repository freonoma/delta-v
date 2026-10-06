import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { version as previewVersion } from "../package.json";
import { beginUpdateCheck, completeUpdatePreview, createUpdatePreview, emptyUpdateState } from "./update-state";
import type { UpdateState } from "./update-state";

const native = isTauri();
const preview = import.meta.env.DEV && !native;
const now = () => Math.floor(Date.now() / 1000);

export function useUpdates() {
  const [state, setState] = useState<UpdateState>(() => preview
    ? createUpdatePreview(window.location.search, previewVersion, now()) : emptyUpdateState());
  const [version, setVersion] = useState<string | null>(preview ? previewVersion : null);
  const [readingVersion, setReadingVersion] = useState(native);
  const [reading, setReading] = useState(native);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = useRef(state);
  const mounted = useRef(false);
  const lifetime = useRef(0);
  const revision = useRef(0);
  const operation = useRef<number | null>(null);

  const publish = useCallback((next: UpdateState) => {
    current.current = next;
    setState(next);
  }, []);

  useEffect(() => {
    mounted.current = true;
    const life = ++lifetime.current;
    const read = ++revision.current;
    setPending(false);
    setReading(native);
    setReadingVersion(native);
    if (native) {
      void invoke<string>("get_app_version").then((value) => {
        if (mounted.current && life === lifetime.current) setVersion(value);
      }).catch(() => {
        if (mounted.current && life === lifetime.current) setVersion(null);
      }).finally(() => {
        if (mounted.current && life === lifetime.current) setReadingVersion(false);
      });
      void invoke<UpdateState>("get_update_state").then((next) => {
        if (mounted.current && life === lifetime.current && read === revision.current) {
          publish(next);
          setError(null);
        }
      }).catch(() => {
        if (mounted.current && life === lifetime.current && read === revision.current) {
          setError("Could not read update check status. Try again.");
        }
      }).finally(() => {
        if (mounted.current && life === lifetime.current && read === revision.current) setReading(false);
      });
    }
    return () => {
      mounted.current = false;
      lifetime.current += 1;
      revision.current += 1;
      operation.current = null;
    };
  }, [publish]);

  // A reopened webview can inherit a check that another invocation started.
  // These reads only inspect memory; they never start a GitHub request.
  useEffect(() => {
    if (!native || !state.checking || pending) return;
    let disposed = false;
    let timer: number | undefined;
    let attempts = 0;
    const poll = async () => {
      if (disposed || operation.current !== null || !current.current.checking) return;
      const read = revision.current;
      try {
        const next = await invoke<UpdateState>("get_update_state");
        if (!disposed && mounted.current && read === revision.current) {
          publish(next);
          setError(null);
        }
      } catch {
        if (!disposed && mounted.current && read === revision.current) {
          setError("Could not read update check status. Try again.");
        }
      }
      if (disposed || read !== revision.current || !current.current.checking) return;
      if (++attempts >= 30) {
        publish({ ...current.current, checking: false });
        setError("Could not confirm the update check. Try again.");
      } else timer = window.setTimeout(() => void poll(), 1000);
    };
    timer = window.setTimeout(() => void poll(), 1000);
    return () => { disposed = true; window.clearTimeout(timer); };
  }, [state.checking, pending, publish]);

  const check = useCallback(async () => {
    if (!mounted.current || operation.current !== null || (!native && !preview)) return;
    const next = beginUpdateCheck(current.current, now());
    if (!next) return;
    const life = lifetime.current;
    const action = ++revision.current;
    operation.current = action;
    publish(next);
    setReading(false);
    setPending(true);
    setError(null);
    try {
      let result: UpdateState;
      if (preview) {
        await new Promise<void>((resolve) => window.setTimeout(resolve, 800));
        result = completeUpdatePreview(next, window.location.search, previewVersion, now());
      } else {
        result = await invoke<UpdateState>("check_for_updates");
      }
      if (mounted.current && life === lifetime.current && action === revision.current) publish(result);
    } catch {
      if (mounted.current && life === lifetime.current && action === revision.current) {
        publish({ ...current.current, checking: false });
        setError("Could not finish the update check. Try again, or view releases on GitHub.");
      }
    } finally {
      if (operation.current === action) operation.current = null;
      if (mounted.current && life === lifetime.current && action === revision.current) setPending(false);
    }
  }, [publish]);

  return { state, version, readingVersion, reading, pending, error, check };
}
