import { useEffect, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { version as previewVersion } from "../package.json";

const native = isTauri();
const preview = import.meta.env.DEV && !native;
const releasesUrl = "https://github.com/freonoma/delta-v/releases";

export function Updates() {
  const [version, setVersion] = useState<string | null>(preview ? previewVersion : null);
  const [reading, setReading] = useState(native);
  const [opening, setOpening] = useState(false);
  const [openError, setOpenError] = useState<string | null>(null);

  useEffect(() => {
    if (!native) return;
    let disposed = false;
    void invoke<string>("get_app_version")
      .then((value) => { if (!disposed) setVersion(value); })
      .catch(() => { if (!disposed) setVersion(null); })
      .finally(() => { if (!disposed) setReading(false); });
    return () => { disposed = true; };
  }, []);

  async function openReleases() {
    if (opening) return;
    setOpening(true);
    setOpenError(null);
    try { await invoke("open_releases"); }
    catch (error: unknown) {
      setOpenError(typeof error === "string" ? error : "Could not open your browser. Please try again.");
    } finally { setOpening(false); }
  }

  return (
    <section className="updates-settings" aria-label="Updates">
      <h3>Updates</h3>
      <p className="updates-version" role="status">{reading ? "Reading installed version…"
        : version !== null ? `Installed version: ${version}` : "Installed version unavailable."}</p>
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
