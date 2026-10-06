import { useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import type { useUpdates } from "./useUpdates";
import { updateErrorMessage, type CheckedRelease } from "./update-state";

const native = isTauri();
const releasesUrl = "https://github.com/freonoma/delta-v/releases";

function releaseMessage(release: CheckedRelease, previous: boolean): string {
  if (previous) {
    return release.status === "available"
      ? `Previous result: version ${release.latest_version} was available.`
      : `Previous result: the latest release was ${release.latest_version}.`;
  }
  switch (release.status) {
    case "available": return `Version ${release.latest_version} is available.`;
    case "current": return "No newer release found.";
    case "ahead": return `Your installed version is newer than the latest release (${release.latest_version}).`;
  }
}

function localTime(timestamp: number): string | null {
  const date = new Date(timestamp * 1000);
  return Number.isFinite(date.getTime()) ? date.toLocaleString() : null;
}

export function Updates({ updates, now }: { updates: ReturnType<typeof useUpdates>; now: number }) {
  const { state, version, readingVersion, reading, pending, error, check } = updates;
  const [opening, setOpening] = useState(false);
  const [openError, setOpenError] = useState<string | null>(null);
  const openingRef = useRef(false);
  const checking = pending || state.checking;
  const failure = error ?? (state.error ? updateErrorMessage(state.error) : null);
  const last = state.last_success;
  const waiting = state.next_check_at !== null && state.next_check_at > now;
  const retryTime = waiting && state.next_check_at !== null ? localTime(state.next_check_at) : null;

  async function openReleases() {
    if (openingRef.current) return;
    openingRef.current = true;
    setOpening(true);
    setOpenError(null);
    try { await invoke("open_releases"); }
    catch { setOpenError("Could not open your browser. Please try again."); }
    finally { openingRef.current = false; setOpening(false); }
  }

  return (
    <section className="updates-settings" aria-label="Updates">
      <h3>Updates</h3>
      <p className="updates-version">{readingVersion ? "Reading installed version…"
        : version !== null ? `Installed version: ${version}` : "Installed version unavailable."}</p>
      <p className="updates-description" id="updates-privacy">Update checks contact GitHub only when you choose Check for updates. No account credentials or usage history are sent.</p>
      <button type="button" className="primary-button updates-check" aria-describedby="updates-privacy"
        disabled={reading || checking || waiting} onClick={() => void check()}>
        {checking ? "Checking for updates…" : "Check for updates"}
      </button>
      <div className="updates-result" role="status" aria-live="polite" aria-atomic="true">
        {checking ? <p>Checking for updates…</p>
          : reading ? <p>Reading update status…</p>
          : failure ? <p className="updates-error">{failure}</p>
          : last ? <p>{releaseMessage(last, false)}</p>
          : <p>Not checked yet.</p>}
        {last && <>
          {(checking || failure) && <p className="updates-previous">{releaseMessage(last, true)}</p>}
          <p className="updates-checked">Last successful check: {localTime(last.checked_at) ?? "time unavailable"}.</p>
        </>}
        {waiting && <p className="updates-wait">{retryTime ? `You can check again after ${retryTime}.` : "GitHub asked Delta-V to wait before checking again."}</p>}
      </div>
      <p className="updates-description">Updates are installed manually. Replacing the app keeps your settings and recorded history.</p>
      <a className="updates-link" href={releasesUrl} target="_blank" rel="noopener noreferrer"
        aria-label="View releases on GitHub (opens in browser)" aria-disabled={opening}
        onClick={(event) => {
          if (native) {
            event.preventDefault();
            void openReleases();
          }
        }}>{opening ? "Opening browser…" : "View releases on GitHub"}</a>
      {openError && <p className="updates-error" role="alert">{openError}</p>}
    </section>
  );
}
