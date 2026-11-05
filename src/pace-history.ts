import type { HistoryCatalog, HistoryPoint, HistoryQuery, HistoryRequest, ProviderState } from "./types";
import type { ReadyPace } from "./pacing";

// This raw range is used only by the pace popup; daily summaries keep their
// existing meaning in the History screen and exports.
export type PaceHistoryRequest = Omit<HistoryRequest, "range"> & {
  range: { kind: "window"; resets_at: number; observed_at: number };
};
export type PaceHistoryQuery = Omit<HistoryQuery, "request"> & { request: PaceHistoryRequest };

export function paceHistoryAccount(catalog: HistoryCatalog, provider: ProviderState, observedAt: number): string | null {
  const current = catalog.current_accounts.find((account) => account.provider === provider.id);
  return current?.account_key && provider.reading_id && current.reading_id === provider.reading_id
    && current.verified_at === observedAt ? current.account_key : null;
}

export function paceHistoryPoints(pace: ReadyPace, saved: HistoryPoint[]): HistoryPoint[] {
  const start = pace.resetAt - pace.windowSeconds;
  const points: HistoryPoint[] = [];
  let interrupted = false;
  for (const point of saved) {
    const valid = Number.isFinite(point.observed_at) && point.observed_at >= start
      && point.observed_at <= pace.observedAt && Number.isFinite(point.used_fraction)
      && point.used_fraction >= 0 && point.used_fraction <= 1
      && point.resets_at !== null && Math.abs(point.resets_at - pace.resetAt) <= 1;
    const previous = points.at(-1);
    if (!valid || (previous && point.observed_at <= previous.observed_at)
      || (point.observed_at === pace.observedAt && point.used_fraction !== pace.usedFraction)) {
      interrupted = true;
      continue;
    }
    const breaks = [...point.break_before];
    if (interrupted) breaks.push("limit_unavailable");
    if (previous && point.observed_at - previous.observed_at > 1800) breaks.push("missing_time");
    if (previous && point.used_fraction < previous.used_fraction) breaks.push("usage_decreased");
    points.push({ ...point, break_before: [...new Set(breaks)] });
    interrupted = false;
  }
  const previous = points.at(-1);
  if (previous?.observed_at !== pace.observedAt) {
    // A live reading can precede its disk write. Keep it separate because the
    // query cannot tell us whether an unavailable sample followed the last point.
    const breaks: HistoryPoint["break_before"] = previous ? ["limit_unavailable"] : [];
    if (interrupted) breaks.push("limit_unavailable");
    if (previous && pace.observedAt - previous.observed_at > 1800) breaks.push("missing_time");
    if (previous && pace.usedFraction < previous.used_fraction) breaks.push("usage_decreased");
    points.push({ observed_at: pace.observedAt, used_fraction: pace.usedFraction,
      resets_at: pace.resetAt, break_before: breaks });
  }
  return points;
}

export function paceGraph(pace: ReadyPace, saved: HistoryPoint[], width: number) {
  const left = 32, right = Math.max(left + 1, width - 8), top = 9, bottom = 91;
  const start = pace.resetAt - pace.windowSeconds;
  const x = (at: number) => left + (at - start) / pace.windowSeconds * (right - left);
  const y = (used: number) => top + used * (bottom - top);
  const points = paceHistoryPoints(pace, saved);
  const runs: HistoryPoint[][] = [];
  for (const point of points) {
    if (!runs.length || point.break_before.length) runs.push([]);
    runs.at(-1)?.push(point);
  }
  // Preserve each run's extrema while bounding the SVG by its rendered width.
  const reduced = runs.map((run) => {
    const buckets: HistoryPoint[][] = [];
    let column = -Infinity;
    for (const point of run) {
      const next = Math.floor(x(point.observed_at));
      if (column !== next) { buckets.push([]); column = next; }
      buckets.at(-1)?.push(point);
    }
    return buckets.flatMap((bucket) => {
      const first = bucket[0], last = bucket.at(-1);
      if (!first || !last) return [];
      let low = first, high = first;
      for (const point of bucket) {
        if (point.used_fraction < low.used_fraction) low = point;
        if (point.used_fraction > high.used_fraction) high = point;
      }
      return [...new Set([first, low, high, last])].sort((a, b) => a.observed_at - b.observed_at);
    });
  });
  const location = (point: HistoryPoint) => `${x(point.observed_at).toFixed(2)},${y(point.used_fraction).toFixed(2)}`;
  const line = reduced.filter((run) => run.length > 1)
    .map((run) => run.map((point, index) => `${index ? "L" : "M"}${location(point)}`).join(" ")).join(" ");
  const dots = reduced.flatMap((run) => run.length === 1 ? run : []).map((point) => ({
    x: x(point.observed_at), y: y(point.used_fraction), at: point.observed_at,
  }));
  return { line, dots, left, right, top, bottom, currentX: x(pace.observedAt), currentY: y(pace.usedFraction),
    sampleCount: points.length };
}

export function previewPaceHistory(pace: ReadyPace, sparse: boolean): HistoryPoint[] {
  const start = pace.resetAt - pace.windowSeconds;
  const elapsed = pace.observedAt - start;
  const points: HistoryPoint[] = [];
  const count = Math.min(5000, Math.ceil(elapsed / 900));
  let interrupted = false;
  for (let index = 0; index <= count; index++) {
    const fraction = count ? index / count : 1;
    if (sparse && fraction > 0.35 && fraction < 0.65) { interrupted = true; continue; }
    points.push({ observed_at: Math.round(start + elapsed * fraction),
      used_fraction: pace.usedFraction * Math.pow(fraction, 1.3), resets_at: pace.resetAt,
      break_before: interrupted ? ["missing_time"] : [] });
    interrupted = false;
  }
  return points;
}
