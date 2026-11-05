import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { HistoryCatalog, HistoryPoint, Limit, ProviderState } from "./types";
import type { ReadyPace } from "./pacing";
import { paceGraph, paceHistoryAccount, previewPaceHistory } from "./pace-history";
import type { PaceHistoryQuery, PaceHistoryRequest } from "./pace-history";
import { native, preview } from "./ui-state";

export function PaceChart({ pace, provider, limit, historyEnabled }: {
  pace: ReadyPace; provider: ProviderState; limit: Limit; historyEnabled: boolean;
}) {
  const chart = useRef<SVGSVGElement>(null);
  const [width, setWidth] = useState(276);
  const [response, setResponse] = useState<{ key: string; points: HistoryPoint[]; note: string } | null>(null);
  const key = JSON.stringify([provider.id, provider.reading_id, limit.id, limit.window_seconds,
    limit.provenance, pace.observedAt, pace.resetAt, pace.usedFraction, historyEnabled]);

  useLayoutEffect(() => {
    const element = chart.current;
    if (!element) return;
    const measure = () => setWidth(Math.max(100, element.getBoundingClientRect().width));
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!historyEnabled) return;
    let disposed = false;
    let revision = 0;
    let stop: (() => void) | undefined;
    async function read(version: number) {
      if (preview) {
        const example = new URLSearchParams(window.location.search).get("history");
        if (example === "error") throw new Error("preview");
        return { points: ["ready", "sparse", "accounts"].includes(example ?? "")
          ? previewPaceHistory(pace, example === "sparse") : [], note: "Sample readings" };
      }
      const catalog = await invoke<HistoryCatalog>("get_history_catalog");
      if (disposed || version !== revision) return null;
      const accountKey = paceHistoryAccount(catalog, provider, pace.observedAt);
      if (!accountKey) {
        return { points: [], note: "Saved history needs a verified account. Showing the latest reading." };
      }
      const request: PaceHistoryRequest = {
        provider: provider.id, account_key: accountKey,
        limit: { id: limit.id, window_seconds: limit.window_seconds, provenance: limit.provenance },
        range: { kind: "window", resets_at: pace.resetAt, observed_at: pace.observedAt },
      };
      const result = await invoke<PaceHistoryQuery>("query_history", { request });
      return { points: result.points, note: result.points.length > 1 ? "" : "More readings will build the usage line." };
    }
    const failed = () => setResponse({ key, points: [], note: "Saved history could not be read. Showing the latest reading." });
    function reload() {
      if (disposed) return;
      const version = ++revision;
      // Clearing or pruning history also invalidates reads already in flight.
      setResponse(null);
      void read(version).then((result) => {
        if (!disposed && version === revision && result) setResponse({ key, ...result });
      }).catch(() => {
        if (!disposed && version === revision) failed();
      });
    }
    if (native) {
      void listen("history-state", reload).then((unlisten) => {
        if (disposed) unlisten();
        else { stop = unlisten; reload(); }
      }).catch(() => { if (!disposed) failed(); });
    } else reload();
    return () => { disposed = true; stop?.(); };
    // The key includes every quota/reading field used by this request.
  }, [key]);

  const points = historyEnabled && response?.key === key ? response.points : [];
  const note = !historyEnabled ? "Turn on History in Settings to see usage over time."
    : response?.key === key ? response.note : "Reading saved history…";
  const graph = paceGraph(pace, points, width);
  const showReadingLabel = graph.currentX - graph.left > 58 && graph.right - graph.currentX > 58;
  return <div className="pace-chart">
    <div className="pace-chart-heading">Allowance remaining</div>
    <svg ref={chart} viewBox={`0 0 ${width} 116`} role="img"
      aria-label={`${graph.sampleCount} ${graph.sampleCount === 1 ? "reading" : "readings"}. Solid marks show recorded allowance remaining. The dashed line spreads allowance evenly between resets.`}>
      {[0, 0.5, 1].map((used) => {
        const y = graph.top + used * (graph.bottom - graph.top);
        return <g key={used}><line className="pace-grid" x1={graph.left} x2={graph.right} y1={y} y2={y} />
          <text x={graph.left - 6} y={y + 3} textAnchor="end">{100 - used * 100}%</text></g>;
      })}
      <path className="pace-reference" d={`M${graph.left},${graph.top}L${graph.right},${graph.bottom}`} />
      {graph.line && <path className="pace-recorded" d={graph.line} />}
      {graph.dots.filter((dot) => dot.at !== pace.observedAt).map((dot) => <circle key={dot.at} className="pace-point" cx={dot.x} cy={dot.y} r={2} />)}
      <circle className="pace-point" cx={graph.currentX} cy={graph.currentY} r={3} />
      <text x={graph.left} y={111}>Start</text>
      {showReadingLabel && <text x={graph.currentX} y={111} textAnchor="middle">Last reading</text>}
      <text x={graph.right} y={111} textAnchor="end">Reset</text>
    </svg>
    <div className="pace-legend"><span><i />{graph.sampleCount > 1 ? "Recorded" : "Latest reading"}</span><span><i className="pace-legend-reference" />Steady use</span></div>
    {note && <p className="pace-chart-note" role="status">{note}</p>}
  </div>;
}
