import { useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { getHistoryPreview } from "./history-preview";
import { exportDateLabel, exportFilename, renderHistoryPng } from "./history-export-image";
import type { HistoryExportSeries, HistoryImageOptions } from "./history-export-image";
import type { Settings } from "./types";
import "./HistoryExport.css";

const native = isTauri();
const preview = import.meta.env.DEV && !native;
const names = { claude: "Claude", codex: "Codex" };
type ExportSelection = { kind: "image" | "csv"; options: HistoryImageOptions };

function errorMessage(error: unknown): string {
  return typeof error === "string" ? error : error instanceof Error ? error.message : "Could not export history. Please try again.";
}

function dataUrl(bytes: Uint8Array, type: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => typeof reader.result === "string" ? resolve(reader.result) : reject(new Error("Could not prepare the export."));
    reader.onerror = () => reject(new Error("Could not prepare the export."));
    reader.readAsDataURL(new Blob([new Uint8Array(bytes).buffer], { type }));
  });
}

async function downloadPreview(bytes: Uint8Array, format: "csv" | "png", filename: string) {
  const link = document.createElement("a");
  link.href = await dataUrl(bytes, format === "png" ? "image/png" : "text/csv;charset=utf-8");
  link.download = filename;
  link.click();
}

function ExportDialog({ selection, threshold, onClose }: { selection: ExportSelection; threshold: number; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const mounted = useRef(false);
  const operation = useRef(false);
  const [image, setImage] = useState<{ bytes: Uint8Array; url: string } | null>(null);
  const [rendering, setRendering] = useState(selection.kind === "image");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const isImage = selection.kind === "image";
  const { options } = selection;

  useEffect(() => {
    mounted.current = true;
    const element = dialog.current;
    if (element && !element.open) element.showModal();
    let disposed = false;
    if (isImage) {
      void renderHistoryPng(options).then(async (bytes) => {
        const url = await dataUrl(bytes, "image/png");
        if (!disposed) setImage({ bytes, url });
      }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); })
        .finally(() => { if (!disposed) setRendering(false); });
    }
    return () => { disposed = true; mounted.current = false; };
  }, [isImage, options]);

  async function exportFile(copy = false) {
    if (operation.current || (isImage && !image)) return;
    operation.current = true; setPending(true); setError(null); setNotice(null);
    try {
      const format = isImage ? "png" : "csv";
      const filename = `Sample-${exportFilename(options.series, format)}`;
      const requests = options.series.map((series) => series.result.request);
      let saved: string | null;
      if (preview) {
        if (copy) throw new Error("Copy image is available in the macOS app.");
        const bytes = image?.bytes ?? getHistoryPreview().csv(requests, threshold);
        await downloadPreview(bytes, format, filename);
        saved = filename;
      } else if (isImage && image) {
        saved = copy
          ? (await invoke("copy_history_png", { bytes: Array.from(image.bytes) }), "clipboard")
          : await invoke<string | null>("save_history_png", { bytes: Array.from(image.bytes) });
      } else saved = await invoke<string | null>("export_history_csv", { requests: options.series.map(({ result }) => ({
        request: result.request, timezone: result.timezone,
        first_date: result.days[0]?.date ?? null, last_date: result.days.at(-1)?.date ?? null,
      })) });
      if (mounted.current && saved) setNotice(copy ? "Image copied." : preview ? "Sample export downloaded." : "File saved.");
    } catch (caught: unknown) {
      if (mounted.current) setError(errorMessage(caught));
    } finally {
      operation.current = false;
      if (mounted.current) setPending(false);
    }
  }

  return <dialog ref={dialog} className="disconnect-dialog history-export-dialog" aria-labelledby="history-export-title" aria-busy={pending || rendering}
    onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}
    onCancel={(event) => { event.preventDefault(); if (!pending) onClose(); }} onClose={() => { if (!pending) onClose(); }}>
    <div className="history-export-title"><h3 id="history-export-title">{isImage ? "Chart image" : "Export readings"}</h3>
      <button className="text-button" autoFocus disabled={pending} onClick={onClose}>Close</button></div>
    <div className="history-export-body">
      {isImage ? <>
        {rendering && <p role="status">Preparing the chart image</p>}
        {image && <img className="history-export-image" src={image.url} alt={`Usage history chart for ${options.series.map((series) => names[series.result.request.provider]).join(" and ")}. ${exportDateLabel(options.series[0]!.result)}.`} />}
      </> : <>
        <p>Save the original readings for this selection as a CSV file.</p>
        <ul className="history-export-selection">{options.series.map((series) => <li key={series.result.request.provider}>
          <strong>{names[series.result.request.provider]} · {series.label}</strong>
          <span>{series.accountLabel} · {exportDateLabel(series.result)}</span>
          <span>{series.result.timezone}</span>
        </li>)}</ul>
        <p>Includes timestamps, exact usage fractions, reset times and opaque account keys. No names, conversations or sign-in tokens.</p>
        <p>CSV exports contain individual readings, including for 7 days, 30 days and All time. Empty periods do not create rows.</p>
      </>}
      {preview && <p className="history-export-description">Browser preview: exports contain sample data.</p>}
    </div>
    {error && <p className="history-error" role="alert">{error}</p>}
    {notice && <p className="history-export-notice" role="status">{notice}</p>}
    <div className="dialog-actions">
      {isImage && <button className="text-button" disabled={!native || pending || !image} onClick={() => void exportFile(true)}>Copy image</button>}
      <button className="primary-button" disabled={pending || rendering || (isImage && !image)} onClick={() => void exportFile()}>
        {pending ? "Exporting" : isImage ? "Save PNG…" : "Save CSV…"}
      </button>
    </div>
  </dialog>;
}

export function HistoryExport({ series, settings, active }: { series: HistoryExportSeries[] | null; settings: Settings; active: boolean }) {
  const menu = useRef<HTMLDetailsElement>(null);
  const trigger = useRef<HTMLElement>(null);
  const [selection, setSelection] = useState<ExportSelection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);
  const ready = series !== null && series.some((item) => item.result.observations.peak !== null);

  useEffect(() => {
    if (!active) { setSelection(null); if (menu.current) menu.current.open = false; }
  }, [active]);

  useEffect(() => {
    function outside(event: PointerEvent) {
      if (event.target instanceof Node && !menu.current?.contains(event.target) && menu.current) menu.current.open = false;
    }
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, []);

  function begin(kind: ExportSelection["kind"]) {
    if (!ready || !series) return;
    if (menu.current) menu.current.open = false;
    setError(null);
    const theme = settings.theme === "system" ? window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light" : settings.theme;
    setSelection({ kind, options: { series: structuredClone(series), percentageMode: settings.percentage_mode, theme, createdAt: Math.floor(Date.now() / 1000), sampleData: preview } });
  }

  async function showFolder() {
    if (opening) return;
    if (menu.current) menu.current.open = false;
    setOpening(true); setError(null);
    try {
      if (preview) setError("The browser preview has no history folder. Open the macOS app to inspect saved readings.");
      else await invoke("show_history_folder");
    } catch (caught: unknown) { setError(errorMessage(caught)); }
    finally { setOpening(false); }
  }

  return <div className="history-export" onKeyDown={(event) => {
    if (event.key === "Escape" && menu.current?.open) { event.stopPropagation(); menu.current.open = false; trigger.current?.focus(); }
  }}>
    <details ref={menu} className="history-export-menu">
      <summary ref={trigger}>Export <svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="m4 6 4 4 4-4" /></svg></summary>
      <div className="history-export-options" role="group" aria-label="Export history">
        <button disabled={!ready} onClick={() => begin("image")}>Chart image…</button>
        <button disabled={!ready} onClick={() => begin("csv")}>Export readings…</button>
        {!ready && <p>Choose an account and a window with saved readings for each selected provider.</p>}
        <button className="history-export-folder" disabled={opening} onClick={() => void showFolder()}>Show history folder</button>
      </div>
    </details>
    {error && <p className="history-export-error" role="alert">{error}</p>}
    {selection && <ExportDialog selection={selection} threshold={settings.threshold} onClose={() => { setSelection(null); trigger.current?.focus(); }} />}
  </div>;
}
