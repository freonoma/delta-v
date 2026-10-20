import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppState, ProviderId } from "./types";
import {
  availabilityFailure, availabilityReadError, createAvailabilityPreparation, emptyAvailabilityStatus,
} from "./availability-state";
import type { AvailabilityAction, AvailabilityStatus } from "./availability-state";
import {
  completeAvailabilityPreview, createAvailabilityPreview, readAvailabilityPreview,
} from "./availability-preview";
import type { AvailabilityPreview, AvailabilityPreviewAction } from "./availability-preview";
import { native, preview } from "./ui-state";

export function useAvailabilityWatches(app: AppState | null) {
  const appRef = useRef(app);
  appRef.current = app;
  const previewModel = useRef<AvailabilityPreview | null>(null);
  if (preview && previewModel.current === null) {
    previewModel.current = createAvailabilityPreview(window.location.search, Math.floor(Date.now() / 1000));
  }
  const [state, setState] = useState<AvailabilityStatus>(() => preview && previewModel.current
    ? readAvailabilityPreview(previewModel.current, app, "", Math.floor(Date.now() / 1000)).state
    : emptyAvailabilityStatus());
  const [reading, setReading] = useState(native);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const current = useRef(state);
  const mounted = useRef(false);
  const lifetime = useRef(0);
  const revision = useRef(0);
  const operation = useRef<number | null>(null);
  const queuedRead = useRef(false);

  const publish = useCallback((next: AvailabilityStatus) => {
    current.current = next;
    setState(next);
  }, []);

  const simulate = useCallback((action: AvailabilityPreviewAction) => {
    const model = previewModel.current;
    if (!model) throw new Error("Availability notifications are available in the macOS app.");
    const result = completeAvailabilityPreview(
      model, action, appRef.current, window.location.search, Math.floor(Date.now() / 1000),
    );
    previewModel.current = result.model;
    if (result.error) throw new Error(result.error);
  }, []);

  const preparation = useRef<ReturnType<typeof createAvailabilityPreparation> | null>(null);
  if (preparation.current === null) {
    preparation.current = createAvailabilityPreparation(
      async (provider) => {
        if (native) await invoke("prepare_availability_watch", { provider });
        else if (preview) simulate({ kind: "prepare", provider });
      },
      async (provider) => {
        if (native) await invoke("clear_availability_preparation", { provider });
        else if (preview) simulate({ kind: "clear", provider });
      },
    );
  }
  const preparations = preparation.current;

  const read = useCallback(async (explicit: boolean): Promise<AvailabilityStatus> => {
    if (native) return invoke<AvailabilityStatus>("get_availability_watch_state");
    if (preview && previewModel.current) {
      const result = readAvailabilityPreview(
        previewModel.current, appRef.current, window.location.search, Math.floor(Date.now() / 1000), explicit,
      );
      previewModel.current = result.model;
      return result.state;
    }
    return current.current;
  }, []);

  const refresh = useCallback(async (explicit = true): Promise<boolean> => {
    if (!mounted.current || (!native && !preview)) return false;
    if (operation.current !== null) { queuedRead.current = true; return false; }
    const life = lifetime.current;
    const readRevision = ++revision.current;
    const valid = () => mounted.current && life === lifetime.current && readRevision === revision.current;
    setReading(true);
    if (explicit) setNotice(null);
    try {
      const next = await read(explicit);
      if (!valid()) return false;
      publish(next);
      setError((currentError) => explicit || currentError === availabilityReadError ? null : currentError);
      return true;
    } catch {
      if (valid()) {
        // Keep known cancellation targets, but do not offer actions from an old reading.
        publish({ ...current.current, providers: [], permission: null });
        setError((currentError) => currentError ?? availabilityReadError);
      }
      return false;
    } finally {
      if (valid()) setReading(false);
    }
  }, [publish, read]);

  useEffect(() => {
    mounted.current = true;
    lifetime.current += 1;
    const focus = () => { void refresh(false); };
    window.addEventListener("focus", focus);
    return () => {
      mounted.current = false;
      lifetime.current += 1;
      revision.current += 1;
      operation.current = null;
      window.removeEventListener("focus", focus);
      void preparations.abandon().catch(() => undefined);
    };
  }, [preparations, refresh]);

  useEffect(() => { void refresh(false); }, [app, refresh]);

  const act = useCallback(async (
    action: AvailabilityAction, work: () => Promise<boolean>, successNotice: string,
  ): Promise<boolean> => {
    if (!mounted.current || operation.current !== null || (!native && !preview)) return false;
    const life = lifetime.current;
    const actionRevision = ++revision.current;
    operation.current = actionRevision;
    const valid = () => mounted.current && life === lifetime.current && actionRevision === revision.current;
    setReading(false);
    setPending(true);
    setError(null);
    setNotice(null);
    let succeeded = false;
    let failure: string | null = null;
    try {
      succeeded = await work();
    } catch (caught: unknown) {
      failure = availabilityFailure(action, caught);
    }
    // A failed mutation may have replaced the file before directory sync failed.
    // Read back both successful and failed commands instead of guessing saved state.
    try {
      const next = await read(false);
      if (valid()) publish(next);
    } catch {
      if (valid()) publish({ ...current.current, providers: [], permission: null });
      failure ??= availabilityReadError;
    } finally {
      if (valid()) {
        setError((currentError) => failure ?? currentError);
        if (succeeded) setNotice(successNotice);
        setPending(false);
      }
      if (operation.current === actionRevision) {
        operation.current = null;
        if (queuedRead.current) {
          queuedRead.current = false;
          void refresh(false);
        }
      }
    }
    return succeeded;
  }, [publish, read, refresh]);

  const arm = useCallback((provider: ProviderId, limitId: string, readingId: string) => act("arm", async () => {
    if (native) await invoke<string>("arm_availability_watch", { provider, limitId, readingId });
    else simulate({ kind: "arm", provider, limitId, readingId });
    preparations.release(provider);
    return true;
  }, "Availability notification saved. A fresh reading must confirm that this quota has allowance."), [act, preparations, simulate]);

  const prepare = useCallback((provider: ProviderId) => act("prepare", () => preparations.prepare(provider),
    "Account preparation requested. After the next usage check, try Notify when available again."),
  [act, preparations]);

  const clearPreparation = useCallback((provider: ProviderId) => act("clear", () => preparations.clear(provider),
    "Account preparation stopped."), [act, preparations]);

  const cancel = useCallback((id: string) => act("cancel", async () => {
    if (native) await invoke("cancel_availability_watch", { id });
    else simulate({ kind: "cancel", id });
    return true;
  }, "Availability notification cancelled."), [act, simulate]);

  const abandonPreparation = useCallback(async (): Promise<boolean> => {
    const life = lifetime.current;
    let succeeded = true;
    try {
      await preparations.abandon();
    } catch (caught: unknown) {
      succeeded = false;
      if (mounted.current && life === lifetime.current) setError(availabilityFailure("clear", caught));
    }
    if (mounted.current && life === lifetime.current) await refresh(false);
    return succeeded;
  }, [preparations, refresh]);

  return { state, reading, pending, error, notice, refresh, arm, prepare, clearPreparation, cancel, abandonPreparation };
}
