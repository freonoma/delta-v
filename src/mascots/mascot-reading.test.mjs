import assert from "node:assert/strict";
import test from "node:test";
import { confirmedRefill, mascotReading, refillBaseline } from "./mascot-reading.ts";

const week = 7 * 24 * 3600;
const deadline = 1_760_000_000;
const settings = { claude_enabled: true, codex_enabled: true, threshold: 20, tracked_limit: "auto" };

function observation({
  used = 0.88, reset = deadline, fetched = deadline - 60, now = fetched, id = String(fetched),
  account = "preview", provider = "claude", limitId = "weekly", duration = week, stale = false,
  preferences = settings,
} = {}) {
  const limit = { id: limitId, kind: "quota", enabled: true, provenance: "official",
    used_fraction: used, resets_at: reset, window_seconds: duration };
  const state = { id: provider, reading_id: id, reading_scope: account, stale, error: null, recovery: null,
    snapshot: { provider, fetched_at: fetched, limits: [limit] } };
  return { state, reading: mascotReading(state, preferences, now) };
}

function reading(changes) {
  return observation(changes).reading;
}

const refill = { used: 0.03, fetched: deadline + 60, reset: deadline + week };

test("poses follow the featured allowance and its configured low threshold", () => {
  assert.equal(reading().pose, "low");
  assert.equal(reading({ used: 0.8 }).pose, "normal");
  assert.equal(reading({ used: 1 }).pose, "exhausted");
  assert.equal(reading({ used: 1, stale: true }).pose, "unavailable");
  assert.equal(reading({ now: deadline }).pose, "unavailable");
  assert.equal(reading({ used: null }).pose, "unavailable");
  assert.equal(reading({ preferences: { ...settings, claude_enabled: false } }).pose, "unavailable");
  assert.equal(reading({ preferences: { ...settings, tracked_limit: "claude:other" } }).pose, "unavailable");
  assert.equal(reading({ used: 0.88, preferences: { ...settings, threshold: 10 } }).pose, "normal");
});

test("a fresh observation of the next weekly window confirms the hop once", () => {
  const before = reading();
  const after = reading(refill);
  assert.equal(confirmedRefill(null, after), false);
  assert.equal(confirmedRefill(before, after), true);
  assert.equal(confirmedRefill(after, after), false);
  assert.equal(confirmedRefill(after, reading({ ...refill, fetched: refill.fetched + 60 })), false);
});

test("an accepted receipt survives the native stale event while its replacement is in flight", () => {
  const before = reading();
  const expired = observation({ stale: true, now: deadline });
  const next = observation(refill);
  assert.equal(expired.reading.pose, "unavailable");
  assert.equal(confirmedRefill(before, expired.reading), false);
  const baseline = refillBaseline(before, expired.reading, expired.state);
  assert.equal(confirmedRefill(baseline, next.reading), true);
  assert.equal(confirmedRefill(refillBaseline(baseline, next.reading, next.state), next.reading), false);

  assert.equal(refillBaseline(null, expired.reading, expired.state).sample, null);
  for (const change of [
    { reading_id: "another-receipt" }, { reading_scope: "another-account" },
    { error: { kind: "network" } }, { recovery: "signing_in" },
  ]) {
    assert.equal(refillBaseline(before, expired.reading, { ...expired.state, ...change }).sample, null);
  }
});

test("idle uses the featured percentage even when the reading cannot confirm a refill", () => {
  const withoutReset = reading({ used: 0.8675, reset: null });
  assert.equal(withoutReset.remaining, 13.25);
  assert.equal(withoutReset.sample, null);
  assert.equal(reading({ used: 1 }).remaining, 0);
  assert.equal(reading({ stale: true }).remaining, null);
});

test("stale data, clock ticks and corrections do not signal a reset", () => {
  const before = reading();
  assert.equal(confirmedRefill(before, reading({ now: deadline })), false);
  assert.equal(confirmedRefill(reading({ stale: true }), reading(refill)), false);
  assert.equal(confirmedRefill(before, reading({ ...refill, stale: true })), false);
  assert.equal(confirmedRefill(before, reading({ ...refill, id: before.sample.readingId })), false);
  assert.equal(confirmedRefill(before, reading({ used: 0.1, fetched: deadline - 1 })), false);
  assert.equal(confirmedRefill(before, reading({ ...refill, reset: deadline + 100 })), false);
  assert.equal(confirmedRefill(before, reading({ ...refill, used: 0.95 })), false);
  assert.equal(confirmedRefill(reading({ ...refill }), before), false);
});

test("both readings must identify the same account and quota", () => {
  const before = reading();
  for (const changes of [
    { provider: "codex" }, { account: "another" }, { account: null }, { limitId: "other" },
    { id: null }, { duration: 5 * 3600, reset: deadline + 5 * 3600 }, { reset: null },
  ]) {
    assert.equal(confirmedRefill(before, reading({ ...refill, ...changes })), false);
  }
});
