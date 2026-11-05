import assert from "node:assert/strict";
import test from "node:test";
import { allowancePace } from "./pacing.ts";
import { paceBudget, paceDuration, paceExplanation, paceMarkerPercent, pacePercent, paceStatus, paceUnavailable } from "./pacing-display.ts";

const now = 1_800_000_000;
function pace(used = 0.5, secondsLeft = 9000, duration = 18000) {
  const limit = { id: "session", label: "5-hour", kind: "quota", used_fraction: used, resets_at: now + secondsLeft,
    window_seconds: duration, provenance: "official", enabled: true, amount: null, detail: null };
  const provider = { id: "claude", snapshot: { provider: "claude", limits: [limit], plan: null, fetched_at: now, warnings: [] },
    error: null, recovery: null, refreshing: false, stale: false, next_retry_at: null };
  const result = allowancePace(limit, provider, now);
  assert.equal(result.state, "ready");
  return result;
}

test("watch status requires a material ten-point excess after settling", () => {
  for (const used of [0, 0.49, 0.5, 0.6]) assert.deepEqual(paceStatus(pace(used)), { label: "On track", tone: "near" });
  assert.equal(paceStatus(pace(0.6001)).label, "Watch usage");
  assert.equal(paceStatus(pace(1, 1)).label, "Used up");
  assert.equal(paceStatus(pace(0.999, 1)).tone, "near");
});

test("fresh weekly and session windows stay neutral during settling, including heavy bursts", () => {
  assert.equal(paceStatus(pace(0.07, 594000, 604800)).label, "Just started");
  assert.equal(paceStatus(pace(0.7, 18000 - 1799)).label, "Just started");
  assert.equal(paceStatus(pace(0.7, 18000 - 1800)).label, "Watch usage");
  assert.equal(paceStatus(pace(0.7, 604800 - 21599, 604800)).label, "Just started");
  assert.equal(paceStatus(pace(0.7, 604800 - 21600, 604800)).label, "Watch usage");
  assert.equal(paceStatus(pace(1, 17999)).label, "Used up");
});

test("popup describes remaining allowance and time in ordinary units", () => {
  assert.equal(paceExplanation(pace(0.88, 324000, 604800)), "12% left for the next 3 days, 18 hours.");
  assert.equal(paceExplanation(pace(0.07, 594000, 604800)), "93% left for the next 6 days, 21 hours.");
  assert.equal(paceExplanation(pace(0.5)), "50% left for the next 2 hours, 30 minutes.");
  assert.equal(paceExplanation(pace(1)), "This window’s allowance is used up.");
  assert.equal(paceDuration(1), "1 minute");
  assert.equal(paceDuration(3600), "1 hour");
  assert.equal(paceDuration(86400), "1 day");
});

test("the guide marker reverses in Remaining mode while pace meaning stays the same", () => {
  const guide = pace(0.5, 13500);
  assert.equal(paceMarkerPercent(guide, "used"), 25);
  assert.equal(paceMarkerPercent(guide, "remaining"), 75);
  assert.equal(paceStatus(guide).tone, "above");
  assert.equal(paceMarkerPercent(pace(0, 18000), "used"), 0);
  assert.equal(paceMarkerPercent(pace(0, 18000), "remaining"), 100);
});

test("budget uses the remaining allowance, with day/hour/remaining units that fit the horizon", () => {
  assert.equal(paceBudget(pace(0.88, 324000, 604800)), "About 3.2% of your weekly allowance per day.");
  assert.equal(paceBudget(pace(0.6, 4 * 86400, 7 * 86400)), "About 10% of your weekly allowance per day.");
  assert.equal(paceBudget(pace(0.6, 86400, 7 * 86400)), "About 40% of your weekly allowance per day.");
  assert.equal(paceBudget(pace(0.5)), "About 20% of your allowance for this window per hour.");
  assert.equal(paceBudget(pace(0.5, 3600)), "About 50% of your allowance for this window per hour.");
  assert.equal(paceBudget(pace(0.5, 3599)), "50% of allowance left");
  assert.equal(paceBudget(pace(1)), "No allowance left");
});

test("formatting preserves tiny positive allowance and does not round near-full to full", () => {
  assert.equal(pacePercent(0), "0%");
  assert.equal(pacePercent(0.001), "<0.1%");
  assert.equal(pacePercent(99.999), ">99.9%");
  assert.equal(pacePercent(100), "100%");
  assert.equal(pacePercent(12.3456), "12.3%");
  assert.equal(paceBudget(pace(0.99999)), "About <0.1% of your allowance for this window per hour.");
});

test("unavailable states explain the missing prerequisite without promising a refill", () => {
  assert.equal(paceUnavailable("freshness"), "Waiting for fresh usage");
  assert.equal(paceUnavailable("elapsed"), "Waiting for reset update");
  assert.equal(paceUnavailable("window"), "Window timing unavailable");
  assert.equal(paceUnavailable("reset"), "Reset time unavailable");
  for (const reason of ["unsupported", "blocked", "usage", "clock"]) assert.ok(paceUnavailable(reason));
});
