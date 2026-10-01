import assert from "node:assert/strict";
import test from "node:test";
import {
  chartCount, chartDomain, chartIndexAt, chartKeyIndex, chartMarkerIndices,
  chartPaths, chartPercent, chartPoint, chartTimeTicks, chartX, chartY,
  dailyChart, PLOT_BOTTOM, PLOT_HEIGHT, PLOT_LEFT, PLOT_RIGHT, PLOT_TOP,
  timelineRuns, timestampLabel,
} from "./history-chart.ts";

const start = Date.parse("2026-10-01T00:00:00Z") / 1000;
const point = (at, used, breaks = []) => ({ observed_at: start + at, used_fraction: used, resets_at: start + 18_000, break_before: breaks });
const day = (offset, peak = null, count = peak ? 1 : 0) => ({
  date: `2026-10-${String(offset + 1).padStart(2, "0")}`, starts_at: start + offset * 86_400,
  ends_at: start + (offset + 1) * 86_400, peak, sample_count: count,
});
const result = (points = [], overrides = {}) => ({
  request: { provider: "claude", account_key: "test-account", limit: { id: "session", window_seconds: 18_000, provenance: "official" }, range: { kind: "today" } },
  timezone: "UTC", from: start, until: start + 12 * 3600, days: [day(0)], points,
  observations: { peak: null, days_with_readings: 0, days_below_threshold: 0, days_in_range: 1, threshold_remaining: 20 },
  ...overrides,
});
const daily = (days, kind = "days7") => {
  const query = result([], { days });
  query.request.range = { kind };
  return query;
};

test("used and remaining percentages preserve exact zero, full usage and rounding boundaries", () => {
  assert.equal(chartPercent(0, "used"), 0);
  assert.equal(chartPercent(0, "remaining"), 100);
  assert.equal(chartPercent(1, "used"), 100);
  assert.equal(chartPercent(1, "remaining"), 0);
  assert.equal(chartPercent(0.58, "remaining"), 42);
  assert.equal(chartPercent(0.58, "used"), 58);
  assert.equal(chartY(0, "remaining"), PLOT_TOP);
  assert.equal(chartY(1, "remaining"), PLOT_HEIGHT - PLOT_BOTTOM);
});

test("an intraday chart reserves the whole calendar day without extending saved readings", () => {
  const query = result([point(3600, 0.2), point(7200, 0.3)]);
  assert.deepEqual(chartDomain(query), [start, start + 86_400]);
  assert.equal(chartX(query, 1, 340), PLOT_LEFT + 2 / 24 * (340 - PLOT_LEFT - PLOT_RIGHT));
  const paths = chartPaths(query, 340, "used");
  assert.equal((paths.line.match(/M/g) ?? []).length, 1);
  assert.equal((paths.line.match(/L/g) ?? []).length, 1);
});

test("every recorded break keeps adjacent readings in separate paths", () => {
  const reasons = ["missing_time", "reset_changed", "reset_boundary", "usage_decreased", "limit_unavailable"];
  const samples = [point(0, 0.1), point(60, 0.2)];
  for (const [index, reason] of reasons.entries()) {
    samples.push(point((index + 1) * 120, 0.1, [reason]), point((index + 1) * 120 + 60, 0.2));
  }
  const query = result(samples);
  assert.deepEqual(timelineRuns(query, 340), [[0, 1], [2, 3], [4, 5], [6, 7], [8, 9], [10, 11]]);
  assert.equal((chartPaths(query, 340, "used").line.match(/M/g) ?? []).length, 6);
});

test("a reset gap survives even when it occurs within a single pixel", () => {
  const query = result([point(10, 0.9), point(11, 1), point(12, 0, ["reset_boundary"]), point(13, 0.1)]);
  assert.deepEqual(timelineRuns(query, 340), [[0, 1], [2, 3]]);
  assert.equal((chartPaths(query, 340, "remaining").line.match(/M/g) ?? []).length, 2);
});

test("one reading is a dot without an invented line or a zero baseline", () => {
  for (const fraction of [0, 0.5, 1]) {
    const query = result([point(3600, fraction)]);
    assert.equal(chartPaths(query, 340, "remaining").line, "");
    assert.notEqual(chartPaths(query, 340, "remaining").marks, "");
    assert.equal(chartIndexAt(query, 0), 0);
    assert.equal(chartIndexAt(query, 1), 0);
  }
});

test("daily maxima remain independent and missing dates remain selectable", () => {
  const query = daily([day(0, point(3600, 0.2)), day(1), day(2, point(180_000, 0.8))]);
  assert.equal(dailyChart(query), true);
  assert.equal(chartCount(query), 3);
  assert.equal(chartPaths(query, 340, "used").line, "");
  assert.notEqual(chartPaths(query, 340, "used").empty, "");
  assert.equal(chartIndexAt(query, 0.5), 1);
  assert.equal(chartPoint(query, 1), null);
  assert.equal(chartIndexAt(query, 1), 2);
  assert.equal(chartPoint(query, 2).observed_at, start + 180_000);
});

test("empty ranges remain empty rather than producing a sample", () => {
  for (const query of [result(), daily([], "all_time")]) {
    assert.equal(chartCount(query), 0);
    assert.equal(chartIndexAt(query, 0.5), -1);
    assert.equal(chartKeyIndex(-1, "Home", 0), null);
    assert.deepEqual(chartPaths(query, 340, "used"), { line: "", marks: "", empty: "" });
  }
});

test("pointer lookup selects the closest original time, including sparse endpoints", () => {
  const query = result([point(3600, 0.1), point(7200, 0.2), point(43_200, 0.3)]);
  assert.equal(chartIndexAt(query, -1), 0);
  assert.equal(chartIndexAt(query, 1.1), 2);
  assert.equal(chartIndexAt(query, 5500 / 86_400), 1);
  assert.equal(chartIndexAt(query, 5400 / 86_400), 0);
});

test("keyboard navigation reaches each original reading and stops at range edges", () => {
  assert.equal(chartKeyIndex(0, "ArrowLeft", 100_000), 0);
  assert.equal(chartKeyIndex(99_999, "ArrowRight", 100_000), 99_999);
  assert.equal(chartKeyIndex(300, "ArrowRight", 100_000), 301);
  assert.equal(chartKeyIndex(300, "Home", 100_000), 0);
  assert.equal(chartKeyIndex(300, "End", 100_000), 99_999);
  assert.equal(chartKeyIndex(300, "PageUp", 100_000), 290);
  assert.equal(chartKeyIndex(300, "PageDown", 100_000), 310);
  assert.equal(chartKeyIndex(0, "Tab", 10), null);
});

test("dense runs retain original extrema and endpoints with bounded per-pixel geometry", () => {
  const samples = Array.from({ length: 100_000 }, (_, index) => point(index * 0.8, 0.5));
  samples[12345] = point(12345 * 0.8, 0);
  samples[67890] = point(67890 * 0.8, 1);
  const query = result(samples);
  const runs = timelineRuns(query, 340);
  const retained = runs.flat();
  assert.equal(runs.length, 1);
  assert.ok(retained.includes(12345));
  assert.ok(retained.includes(67890));
  assert.equal(retained[0], 0);
  assert.equal(retained.at(-1), 99_999);
  assert.ok(retained.length <= 4 * (340 - PLOT_LEFT - PLOT_RIGHT + 1));
  const markers = chartMarkerIndices(query, 340);
  assert.ok(markers.includes(12345));
  assert.ok(markers.includes(67890));
  assert.ok(markers.length <= 4 * (Math.ceil((340 - PLOT_LEFT - PLOT_RIGHT) / 4) + 1));
  assert.equal(chartCount(query), 100_000);
});

test("a history made only of gaps never gains a connecting line", () => {
  const query = result(Array.from({ length: 1000 }, (_, index) => point(index * 60, index % 2, ["missing_time"])));
  assert.equal(chartPaths(query, 340, "used").line, "");
  assert.equal(timelineRuns(query, 340).length, 1000);
});

test("all-time daily geometry stays bounded while every original date is inspectable", () => {
  const days = Array.from({ length: 36_600 }, (_, index) => ({
    ...day(index), peak: index % 3 === 0 ? point(index * 86_400, index % 2) : null,
  }));
  const query = daily(days, "all_time");
  assert.ok(chartMarkerIndices(query, 560).length <= 4 * (Math.ceil((560 - PLOT_LEFT - PLOT_RIGHT) / 4) + 1));
  assert.equal(chartCount(query), 36_600);
  assert.equal(chartIndexAt(query, (31_111 + 0.5) / days.length), 31_111);
  assert.equal(chartKeyIndex(0, "End", days.length), 36_599);
  assert.equal(chartPaths(query, 560, "used").line, "");
});

test("DST day widths use actual calendar boundaries and axis ticks stay at local hours", () => {
  const begins = Date.parse("2026-10-24T22:00:00Z") / 1000;
  const ends = Date.parse("2026-10-25T23:00:00Z") / 1000;
  const query = result([], { timezone: "Europe/Warsaw", from: begins, until: ends, days: [{ date: "2026-10-25", starts_at: begins, ends_at: ends, sample_count: 0, peak: null }] });
  assert.equal(chartDomain(query)[1] - chartDomain(query)[0], 25 * 3600);
  const ticks = chartTimeTicks(query, 560);
  assert.deepEqual(ticks.map((tick) => tick.label), ["00:00", "06:00", "12:00", "18:00", "00:00"]);
  assert.equal(ticks[1].x, PLOT_LEFT + 7 / 25 * (560 - PLOT_LEFT - PLOT_RIGHT));
});

test("repeated DST hours remain distinguishable by their original UTC offsets", () => {
  const earlier = timestampLabel(Date.parse("2026-10-25T00:30:00Z") / 1000, "Europe/Warsaw", "en-GB");
  const later = timestampLabel(Date.parse("2026-10-25T01:30:00Z") / 1000, "Europe/Warsaw", "en-GB");
  assert.match(earlier, /02:30:00.*GMT\+2/);
  assert.match(later, /02:30:00.*GMT\+1/);
  assert.notEqual(earlier, later);
});
