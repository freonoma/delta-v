import type { HistoryPoint, HistoryQuery, PercentageMode } from "./types";

export const PLOT_HEIGHT = 144;
export const PLOT_LEFT = 32;
export const PLOT_RIGHT = 12;
export const PLOT_TOP = 10;
export const PLOT_BOTTOM = 26;

export function dailyChart(result: HistoryQuery): boolean {
  return result.request.range.kind !== "today" && result.request.range.kind !== "day";
}

export function chartPercent(fraction: number, mode: PercentageMode): number {
  const used = Math.min(100, Math.max(0, fraction * 100));
  return Math.round((mode === "used" ? used : 100 - used) * 1e9) / 1e9;
}

export function chartDomain(result: HistoryQuery): readonly [number, number] {
  const day = result.days[0];
  if (!dailyChart(result) && day) return [day.starts_at, day.ends_at];
  return [result.from, Math.max(result.from + 1, result.until)];
}

export function chartCount(result: HistoryQuery): number {
  return dailyChart(result) ? result.days.length : result.points.length;
}

export function chartPoint(result: HistoryQuery, index: number): HistoryPoint | null {
  return dailyChart(result) ? result.days[index]?.peak ?? null : result.points[index] ?? null;
}

export function chartX(result: HistoryQuery, index: number, width: number): number {
  const span = Math.max(1, width - PLOT_LEFT - PLOT_RIGHT);
  if (dailyChart(result)) return PLOT_LEFT + (index + 0.5) / Math.max(1, result.days.length) * span;
  const [from, until] = chartDomain(result);
  const timestamp = result.points[index]?.observed_at ?? from;
  return PLOT_LEFT + Math.min(1, Math.max(0, (timestamp - from) / (until - from))) * span;
}

export function chartY(fraction: number, mode: PercentageMode): number {
  return PLOT_TOP + (1 - chartPercent(fraction, mode) / 100) * (PLOT_HEIGHT - PLOT_TOP - PLOT_BOTTOM);
}

export function chartIndexAt(result: HistoryQuery, position: number): number {
  const count = chartCount(result);
  if (count === 0) return -1;
  const fraction = Math.min(1, Math.max(0, position));
  if (dailyChart(result)) return Math.min(count - 1, Math.floor(fraction * count));
  const [from, until] = chartDomain(result);
  const timestamp = from + fraction * (until - from);
  let lower = 0;
  let upper = count - 1;
  while (lower < upper) {
    const middle = Math.floor((lower + upper) / 2);
    const point = result.points[middle];
    if (point && point.observed_at < timestamp) lower = middle + 1;
    else upper = middle;
  }
  const right = result.points[lower];
  const left = result.points[lower - 1];
  return right && left && timestamp - left.observed_at <= right.observed_at - timestamp ? lower - 1 : lower;
}

export function chartKeyIndex(index: number, key: string, count: number): number | null {
  if (count === 0) return null;
  let next: number;
  switch (key) {
    case "ArrowLeft": case "ArrowDown": next = index - 1; break;
    case "ArrowRight": case "ArrowUp": next = index + 1; break;
    case "PageDown": next = index + 10; break;
    case "PageUp": next = index - 10; break;
    case "Home": next = 0; break;
    case "End": next = count - 1; break;
    default: return null;
  }
  return Math.max(0, Math.min(count - 1, next));
}

interface Bucket {
  column: number;
  first: number;
  last: number;
  low: number;
  high: number;
  lowValue: number;
  highValue: number;
}

function startBucket(column: number, index: number, value: number): Bucket {
  return { column, first: index, last: index, low: index, high: index, lowValue: value, highValue: value };
}

function extendBucket(bucket: Bucket, index: number, value: number): void {
  bucket.last = index;
  if (value < bucket.lowValue) { bucket.low = index; bucket.lowValue = value; }
  if (value > bucket.highValue) { bucket.high = index; bucket.highValue = value; }
}

function bucketIndices(bucket: Bucket): number[] {
  return [...new Set([bucket.first, bucket.low, bucket.high, bucket.last])].sort((a, b) => a - b);
}

export function timelineRuns(result: HistoryQuery, width: number): number[][] {
  const runs: number[][] = [];
  let run: number[] = [];
  let bucket: Bucket | null = null;
  const flush = () => {
    if (bucket) run.push(...bucketIndices(bucket));
    bucket = null;
  };
  for (let index = 0; index < result.points.length; index++) {
    const point = result.points[index];
    if (!point) continue;
    if (point.break_before.length > 0) {
      flush();
      if (run.length > 0) runs.push(run);
      run = [];
    }
    const column = Math.floor(chartX(result, index, width));
    if (bucket?.column !== column) {
      flush();
      bucket = startBucket(column, index, point.used_fraction);
    } else {
      extendBucket(bucket, index, point.used_fraction);
    }
  }
  flush();
  if (run.length > 0) runs.push(run);
  return runs;
}

export function chartMarkerIndices(result: HistoryQuery, width: number): number[] {
  const markers: number[] = [];
  let bucket: Bucket | null = null;
  for (let index = 0; index < chartCount(result); index++) {
    const point = chartPoint(result, index);
    if (!point) continue;
    const column = Math.floor(chartX(result, index, width) / 4);
    if (bucket?.column !== column) {
      if (bucket) markers.push(...bucketIndices(bucket));
      bucket = startBucket(column, index, point.used_fraction);
    } else {
      extendBucket(bucket, index, point.used_fraction);
    }
  }
  if (bucket) markers.push(...bucketIndices(bucket));
  return markers;
}

function coordinate(value: number): string { return value.toFixed(2); }

export interface ChartPaths { line: string; marks: string; empty: string; }

export function chartPaths(result: HistoryQuery, width: number, mode: PercentageMode): ChartPaths {
  const location = (index: number): string => {
    const point = chartPoint(result, index);
    return `${coordinate(chartX(result, index, width))},${coordinate(chartY(point?.used_fraction ?? 0, mode))}`;
  };
  // Each reset or gap starts a new path, even when both readings share a screen pixel.
  const line = dailyChart(result) ? "" : timelineRuns(result, width)
    .filter((run) => run.length > 1)
    .map((run) => run.map((index, offset) => `${offset === 0 ? "M" : "L"}${location(index)}`).join(""))
    .join("");
  const radius = dailyChart(result) ? 2.6 : 1.8;
  const marks = chartMarkerIndices(result, width).map((index) =>
    `M${location(index)}m-${radius},0a${radius},${radius} 0 1,0 ${radius * 2},0a${radius},${radius} 0 1,0 -${radius * 2},0`,
  ).join("");
  const emptyColumns = new Set<number>();
  if (dailyChart(result)) result.days.forEach((day, index) => {
    if (!day.peak) emptyColumns.add(Math.floor(chartX(result, index, width) / 4) * 4 + 2);
  });
  const baseline = PLOT_HEIGHT - PLOT_BOTTOM;
  const empty = [...emptyColumns].map((x) => `M${x},${baseline + 5}v2`).join("");
  return { line, marks, empty };
}

export function timestampLabel(timestamp: number, timezone: string, locale?: string): string {
  return new Intl.DateTimeFormat(locale, {
    timeZone: timezone, year: "numeric", month: "short", day: "numeric",
    hour: "2-digit", minute: "2-digit", second: "2-digit", timeZoneName: "shortOffset",
  }).format(new Date(timestamp * 1000));
}

export function chartTimeTicks(result: HistoryQuery, width: number): { x: number; label: string }[] {
  const [from, until] = chartDomain(result);
  const format = new Intl.DateTimeFormat(undefined, {
    timeZone: result.timezone, hour: "2-digit", minute: "2-digit", hourCycle: "h23",
  });
  const interval = width < 380 ? 8 : 6;
  const timestamps = [from];
  for (let timestamp = from + 900; timestamp < until; timestamp += 900) {
    const parts = format.formatToParts(timestamp * 1000);
    const hour = Number(parts.find((part) => part.type === "hour")?.value);
    const minute = Number(parts.find((part) => part.type === "minute")?.value);
    if (hour % interval === 0 && minute === 0) timestamps.push(timestamp);
  }
  timestamps.push(until);
  return timestamps.map((timestamp) => ({
    x: PLOT_LEFT + (timestamp - from) / (until - from) * (width - PLOT_LEFT - PLOT_RIGHT),
    label: format.format(timestamp * 1000),
  }));
}
