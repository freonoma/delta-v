import assert from "node:assert/strict";
import test from "node:test";
import { allowancePace } from "./pacing.ts";

const now = 1_800_000_000;
const quota = (overrides = {}) => ({
  id: "session", label: "5-hour", kind: "quota", used_fraction: 0.5,
  resets_at: now + 9000, window_seconds: 18_000, provenance: "official", enabled: true,
  amount: null, detail: null, ...overrides,
});
const provider = (limit, overrides = {}) => ({
  id: "claude", snapshot: { provider: "claude", limits: [limit], plan: null, fetched_at: now, warnings: [] },
  error: null, recovery: null, refreshing: false, stale: false, next_retry_at: null,
  ...overrides,
});
const paceFor = (limit = quota(), overrides = {}, at = now, blocked = false) =>
  allowancePace(limit, provider(limit, overrides), at, blocked);
const unavailable = (reason) => ({ state: "unavailable", reason });

test("a half-used five-hour window halfway through has an even guide and a remaining budget", () => {
  assert.deepEqual(paceFor(), {
    state: "ready", observedAt: now, resetAt: now + 9000, windowSeconds: 18_000,
    remainingSeconds: 9000, usedFraction: 0.5, remainingFraction: 0.5,
    elapsedFraction: 0.5, guideRemainingFraction: 0.5, deltaFraction: 0,
    remainingPerHour: 20, remainingPerDay: 480,
  });
});

test("the guide compares allowance use with elapsed time, independently for each provider", () => {
  for (const id of ["claude", "codex"]) {
    for (const [used, difference] of [[0.25, -0.25], [0.5, 0], [0.75, 0.25]]) {
      const limit = quota({ used_fraction: used });
      const state = provider(limit, { id });
      state.snapshot.provider = id;
      const result = allowancePace(limit, state, now);
      assert.equal(result.state, "ready");
      assert.equal(result.elapsedFraction, 0.5);
      assert.equal(result.deltaFraction, difference);
    }
  }
});

test("weekly budgets use quota percentage points per day, not elapsed-window consumption", () => {
  const limit = quota({ id: "weekly", label: "Weekly", used_fraction: 0.6,
    window_seconds: 604_800, resets_at: now + 345_600 });
  const result = paceFor(limit);
  assert.equal(result.state, "ready");
  assert.equal(result.elapsedFraction, 3 / 7);
  assert.equal(result.guideRemainingFraction, 4 / 7);
  assert.equal(result.remainingPerDay, 10);
  assert.equal(result.remainingPerHour, 10 / 24);
});

test("zero and fully consumed allowance remain exact, including at the window start", () => {
  for (const used of [0, 1]) {
    const result = paceFor(quota({ used_fraction: used, resets_at: now + 18_000 }));
    assert.equal(result.state, "ready");
    assert.equal(result.elapsedFraction, 0);
    assert.equal(result.guideRemainingFraction, 1);
    assert.equal(result.remainingFraction, 1 - used);
    assert.equal(result.remainingPerHour, (1 - used) * 20);
  }
});

test("the same snapshot never becomes better paced as time passes, and expires at reset", () => {
  const limit = quota();
  const state = provider(limit);
  const first = allowancePace(limit, state, now);
  assert.deepEqual(allowancePace(limit, state, now + 300), first);
  assert.deepEqual(allowancePace(limit, state, limit.resets_at - 1), first);
  assert.deepEqual(allowancePace(limit, state, limit.resets_at), unavailable("elapsed"));
  assert.deepEqual(allowancePace(limit, state, limit.resets_at + 18_000), unavailable("elapsed"));
});

test("the guide uses the sample time for both elapsed fraction and remaining budget", () => {
  const limit = quota();
  const state = provider(limit);
  state.snapshot.fetched_at = now - 9000;
  const result = allowancePace(limit, state, now);
  assert.equal(result.state, "ready");
  assert.equal(result.observedAt, now - 9000);
  assert.equal(result.elapsedFraction, 0);
  assert.equal(result.remainingSeconds, 18_000);
  assert.equal(result.remainingPerHour, 10);
});

test("disabled quotas, other balances and unofficial readings cannot supply a pace guide", () => {
  for (const overrides of [
    { enabled: false }, ...["spend", "credits", "unknown"].map((kind) => ({ kind })),
    ...["unknown", "local_estimate"].map((provenance) => ({ provenance })),
  ]) assert.deepEqual(paceFor(quota(overrides)), unavailable("unsupported"));
});

test("missing, invalid and out-of-range fractions are rejected without clamping", () => {
  for (const used_fraction of [null, NaN, Infinity, -Infinity, -0.01, 1.01]) {
    assert.deepEqual(paceFor(quota({ used_fraction })), unavailable("usage"));
  }
});

test("missing, nonpositive, nonfinite and impossible window lengths are rejected", () => {
  for (const window_seconds of [null, 0, -1, NaN, Infinity, -Infinity, Number.MAX_VALUE]) {
    assert.deepEqual(paceFor(quota({ window_seconds })), unavailable("window"));
  }
  // The reported window cannot begin after the observation it is describing.
  assert.deepEqual(paceFor(quota({ window_seconds: 8999 })), unavailable("window"));
});

test("missing and unrepresentable resets remain unavailable rather than assuming a reset", () => {
  for (const resets_at of [null, NaN, Infinity, -Infinity, Number.MAX_VALUE, 8_640_000_000_001]) {
    assert.deepEqual(paceFor(quota({ resets_at })), unavailable("reset"));
  }
  for (const resets_at of [now, now - 1, 0]) {
    assert.deepEqual(paceFor(quota({ resets_at })), unavailable("elapsed"));
  }
});

test("invalid clocks, invalid observation times and clocks moved behind the sample fail closed", () => {
  for (const at of [NaN, Infinity, -Infinity, Number.MAX_VALUE, now - 1]) {
    assert.deepEqual(paceFor(quota(), {}, at), unavailable("clock"));
  }
  for (const observed of [NaN, Infinity, -Infinity, Number.MAX_VALUE, now + 1]) {
    const limit = quota();
    const state = provider(limit);
    state.snapshot.fetched_at = observed;
    assert.deepEqual(allowancePace(limit, state, now), unavailable("clock"));
  }
});

test("snapshots from before this window do not create a pace guide", () => {
  const limit = quota();
  const state = provider(limit);
  state.snapshot.fetched_at = limit.resets_at - limit.window_seconds - 1;
  assert.deepEqual(allowancePace(limit, state, now), unavailable("window"));
});

test("stale, failed, missing and mismatched provider snapshots suppress guidance", () => {
  const limit = quota();
  const mismatch = { ...provider(limit).snapshot, provider: "codex" };
  for (const overrides of [
    { stale: true }, { error: { kind: "network", message: "Unavailable" } },
    { snapshot: null }, { snapshot: mismatch },
  ]) assert.deepEqual(paceFor(limit, overrides), unavailable("freshness"));
});

test("recovery and pending connection actions cannot retain previous-account guidance", () => {
  for (const recovery of ["renewing", "signing_in", "checking", "cancelling"]) {
    assert.deepEqual(paceFor(quota(), { recovery }), unavailable("blocked"));
  }
  assert.deepEqual(paceFor(quota(), {}, now, true), unavailable("blocked"));
});

test("routine refreshing and retry timing do not replace or advance an accepted guide", () => {
  const expected = paceFor();
  for (const next_retry_at of [null, now - 1, now + 600]) {
    assert.deepEqual(paceFor(quota(), { refreshing: true, next_retry_at }), expected);
  }
});

test("an expired sibling is not inspected when the target and provider are fresh", () => {
  const limit = quota();
  const state = provider(limit);
  state.snapshot.limits.push(quota({ id: "weekly", resets_at: now - 1 }));
  assert.deepEqual(allowancePace(limit, state, now), paceFor(limit));
  // Honor the backend's freshness decision even if a sibling caused it.
  state.stale = true;
  assert.deepEqual(allowancePace(limit, state, now), unavailable("freshness"));
});

test("pace calculations neither mutate a snapshot nor expose mutable source values", () => {
  const limit = Object.freeze(quota());
  const state = provider(limit);
  Object.freeze(state.snapshot.limits);
  Object.freeze(state.snapshot);
  Object.freeze(state);
  const original = structuredClone(state);
  const result = allowancePace(limit, state, now);
  assert.equal(result.state, "ready");
  result.usedFraction = 1;
  result.resetAt = 0;
  assert.deepEqual(state, original);
  assert.equal(allowancePace(limit, state, now).usedFraction, 0.5);
});
