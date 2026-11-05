import assert from "node:assert/strict";
import test from "node:test";
import { paceGraph, paceHistoryAccount, paceHistoryPoints } from "./pace-history.ts";

const pace = { state: "ready", observedAt: 10000, resetAt: 19000, windowSeconds: 18000,
  remainingSeconds: 9000, usedFraction: 0.7, remainingFraction: 0.3, elapsedFraction: 0.5,
  guideRemainingFraction: 0.5, deltaFraction: 0.2, remainingPerHour: 12, remainingPerDay: 288 };
const point = (at, used, breaks = [], reset = 19000) => ({ observed_at: at, used_fraction: used, resets_at: reset, break_before: breaks });

test("history account is bound to the displayed reading, including same-second account changes", () => {
  const provider = { id: "claude", reading_id: "reading-A" };
  const account = { provider: "claude", account_key: "account-A", verified_at: 10000, reading_id: "reading-A" };
  const catalog = value => ({ accounts: [], current_accounts: [value] });
  assert.equal(paceHistoryAccount(catalog(account), provider, 10000), "account-A");
  assert.equal(paceHistoryAccount(catalog({ ...account, account_key: "account-B", reading_id: "reading-B" }), provider, 10000), null);
  assert.equal(paceHistoryAccount(catalog({ ...account, verified_at: 10001 }), provider, 10000), null);
  assert.equal(paceHistoryAccount(catalog({ ...account, reading_id: null }), provider, 10000), null);
  assert.equal(paceHistoryAccount(catalog(account), { ...provider, reading_id: null }, 10000), null);
  assert.equal(paceHistoryAccount(catalog({ ...account, provider: "codex" }), provider, 10000), null);
});

test("a lone current reading produces one dot and no fabricated historical path", () => {
  const graph = paceGraph(pace, [], 276);
  assert.equal(graph.line, "");
  assert.equal(graph.sampleCount, 1);
  assert.equal(graph.dots[0].at, pace.observedAt);
});

test("saved history ends at its last observation; an unwritten current reading stays separate", () => {
  const points = [point(8000, 0.4), point(9000, 0.6)];
  const normalized = paceHistoryPoints(pace, points);
  assert.deepEqual(normalized.at(-1).break_before, ["limit_unavailable"]);
  const graph = paceGraph(pace, points, 276);
  assert.equal((graph.line.match(/M/g) ?? []).length, 1);
  assert.equal(graph.dots.length, 1);
  assert.equal(graph.dots[0].at, 10000);
  assert.equal(points.length, 2);
});

test("recorded current point connects only across observed continuity", () => {
  const saved = [point(7000, 0.3), point(8000, 0.4), point(9000, 0.6, ["missing_time"]), point(10000, 0.7)];
  const graph = paceGraph(pace, saved, 276);
  assert.equal((graph.line.match(/M/g) ?? []).length, 2);
  assert.equal(graph.sampleCount, 4);
});

test("other reset windows, future points and invalid fractions cannot enter the curve", () => {
  const points = paceHistoryPoints(pace, [point(900, 0), point(7000, 0.3), point(7500, 0.4, [], 20000),
    point(8000, 0.4, [], 19001), point(9000, NaN), point(10000, 0.7), point(11000, 0.9)]);
  assert.deepEqual(points.map(p => p.observed_at), [7000, 8000, 10000]);
  assert.ok(points[1].break_before.includes("limit_unavailable"));
  assert.ok(points[2].break_before.includes("limit_unavailable"));
});

test("large gaps and usage decreases start separate runs", () => {
  const points = paceHistoryPoints(pace, [point(2000, 0.1), point(5000, 0.4), point(6000, 0.2), point(10000, 0.7)]);
  assert.ok(points[1].break_before.includes("missing_time"));
  assert.ok(points[2].break_before.includes("usage_decreased"));
  assert.ok(points[3].break_before.includes("missing_time"));
});

test("dense history retains extrema with bounded output and never extends past the reading", () => {
  const saved = Array.from({ length: 9001 }, (_, i) => point(1000 + i, i / 9000 * 0.7));
  const graph = paceGraph(pace, saved, 276);
  assert.ok(graph.line.length < 20000);
  assert.equal(graph.dots.length, 0);
  assert.equal(graph.currentX, (graph.left + graph.right) / 2);
});
