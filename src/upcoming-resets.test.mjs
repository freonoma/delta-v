import assert from "node:assert/strict";
import test from "node:test";
import { upcomingResets } from "./upcoming-resets.ts";

const now = 1_800_000_000;
const settings = { providers: "both", claude_enabled: true, codex_enabled: true };
const quota = (id, reset = now + 3600, overrides = {}) => ({
  id, label: id, kind: "quota", used_fraction: 0.9, resets_at: reset,
  window_seconds: 18_000, provenance: "official", enabled: true,
  amount: null, detail: null, ...overrides,
});
const provider = (id, limits, overrides = {}) => ({
  id, snapshot: { provider: id, limits, plan: null, fetched_at: now - 60, warnings: [] },
  error: null, recovery: null, refreshing: false, stale: false, next_retry_at: null,
  ...overrides,
});
const rowsFor = (limits, overrides = {}, at = now) => upcomingResets([provider("claude", limits, overrides)], settings, at);

test("reset rows retain reported identity and timestamps without deriving allowance", () => {
  const session = quota("session", now + 3600, { label: "Five hours" });
  assert.deepEqual(rowsFor([session]), [{
    provider: "claude", limitId: "session", label: "Five hours", windowSeconds: 18_000,
    fetchedAt: now - 60, resetAt: now + 3600, state: "upcoming", stale: false,
  }]);
});

test("future, exact-boundary, elapsed, missing and epoch-zero resets stay distinct", () => {
  const rows = rowsFor([
    quota("future", now + 1), quota("boundary", now), quota("past", now - 1),
    quota("missing", null), quota("epoch", 0), quota("before-epoch", -1),
  ]);
  assert.deepEqual(rows.map((row) => [row.limitId, row.state, row.resetAt]), [
    ["future", "upcoming", now + 1], ["boundary", "elapsed", now], ["past", "elapsed", now - 1],
    ["epoch", "elapsed", 0], ["before-epoch", "elapsed", -1], ["missing", "missing", null],
  ]);
});

test("passing a reported reset never advances it by the window length or invents a refill", () => {
  const session = quota("session", now, { used_fraction: 1 });
  const before = rowsFor([session], {}, now - 1)[0];
  const boundary = rowsFor([session])[0];
  const later = rowsFor([session], {}, now + 10 * 18_000)[0];
  assert.equal(before.state, "upcoming");
  assert.deepEqual(boundary, { ...before, state: "elapsed" });
  assert.deepEqual(later, boundary);
  assert.equal(session.used_fraction, 1);
  assert.equal(session.resets_at, now);
});

test("stale readings retain all timing states and a future reset does not make them fresh", () => {
  for (const [reset, state] of [[now + 1, "upcoming"], [now, "elapsed"], [null, "missing"], [NaN, "invalid"]]) {
    const [row] = rowsFor([quota("session", reset)], { stale: true });
    assert.equal(row.stale, true);
    assert.equal(row.state, state);
  }
});

test("a failed check marks cached rows stale even if the stale flag has not arrived", () => {
  const [row] = rowsFor([quota("session")], { error: { kind: "network", message: "Unavailable" } });
  assert.equal(row.stale, true);
  assert.equal(row.state, "upcoming");
});

test("refreshing and usage-check deadlines neither replace resets nor clear stale state", () => {
  const limits = [quota("session", now - 1), quota("weekly", null)];
  const expected = rowsFor(limits, { stale: true });
  for (const retry of [null, now - 1, now, now + 3600]) {
    assert.deepEqual(rowsFor(limits, { stale: true, refreshing: true, next_retry_at: retry }), expected);
  }
});

test("missing and invalid usage percentages do not hide a reported reset", () => {
  for (const used of [null, NaN, Infinity, -Infinity, -0.1, 1.1, 0, 1]) {
    const [row] = rowsFor([quota("session", now + 1, { used_fraction: used })]);
    assert.equal(row.state, "upcoming");
    assert.equal(row.resetAt, now + 1);
  }
});

test("disabled, non-quota and non-official limits do not supply provider reset rows", () => {
  const limits = [
    quota("disabled", now + 1, { enabled: false }),
    ...["credits", "spend", "unknown"].map((kind) => quota(kind, now + 1, { kind })),
    ...["local_estimate", "unknown"].map((provenance) => quota(provenance, now + 1, { provenance })),
  ];
  assert.deepEqual(rowsFor(limits), []);
});

test("the provider picker and connection settings control which providers contribute rows", () => {
  const providers = [provider("claude", [quota("session")]), provider("codex", [quota("session")])];
  for (const selected of ["claude", "codex"]) {
    assert.deepEqual(upcomingResets(providers, { ...settings, providers: selected }, now).map((row) => row.provider), [selected]);
  }
  assert.deepEqual(upcomingResets(providers, { ...settings, claude_enabled: false }, now).map((row) => row.provider), ["codex"]);
  assert.deepEqual(upcomingResets(providers, { ...settings, codex_enabled: false }, now).map((row) => row.provider), ["claude"]);
  assert.deepEqual(upcomingResets(providers, { ...settings, claude_enabled: false, codex_enabled: false }, now), []);
});

test("missing snapshots and recovery cannot expose a previous account's reset rows", () => {
  assert.deepEqual(rowsFor([quota("session")], { snapshot: null }), []);
  for (const recovery of ["renewing", "signing_in", "checking", "cancelling"]) {
    assert.deepEqual(rowsFor([quota("session")], { recovery }), []);
  }
  assert.deepEqual(upcomingResets([], settings, now), []);
  assert.deepEqual(rowsFor([]), []);
});

test("model and additional quota windows survive compact and menu-bar preferences", () => {
  const providers = [provider("claude", [quota("session"), quota("weekly"), quota("seven_day_sonnet")]),
    provider("codex", [quota("rate_limit:primary_window"), quota("additional:primary_window", now + 1, { window_seconds: null })])];
  const display = { ...settings, claude_windows: ["session"], codex_windows: [], tracked_limit: "claude:session" };
  const rows = upcomingResets(providers, display, now);
  assert.equal(rows.length, 5);
  assert.equal(rows[0].limitId, "additional:primary_window");
  assert.equal(rows[0].windowSeconds, null);
  assert.ok(rows.some((row) => row.limitId === "seven_day_sonnet"));
});

test("invalid reset timestamps are retained as invalid without becoming missing or future", () => {
  for (const reset of [NaN, Infinity, -Infinity, Number.MAX_VALUE, 8_640_000_000_001, -8_640_000_000_001]) {
    const [row] = rowsFor([quota("session", reset)]);
    assert.equal(row.state, "invalid");
    assert.equal(row.resetAt, reset);
  }
});

test("an invalid clock fails explicitly rather than reclassifying reported resets", () => {
  for (const at of [NaN, Infinity, -Infinity, Number.MAX_VALUE]) {
    assert.throws(() => rowsFor([quota("session")], {}, at), RangeError);
  }
});

test("ordering is chronological for future resets and retains elapsed, missing and invalid groups", () => {
  const rows = rowsFor([
    quota("invalid", NaN), quota("missing", null), quota("old", now - 10),
    quota("later", now + 10), quota("recent", now - 1), quota("next", now + 1),
  ]);
  assert.deepEqual(rows.map((row) => row.limitId), ["next", "later", "recent", "old", "missing", "invalid"]);
});

test("equal timestamps retain separate providers and quota windows with deterministic ties", () => {
  const providers = [provider("codex", [quota("session")]),
    provider("claude", [quota("weekly"), quota("session", now + 3600, { window_seconds: 604_800 }), quota("session")])];
  const rows = upcomingResets(providers, settings, now);
  assert.deepEqual(rows.map((row) => [row.provider, row.limitId, row.windowSeconds]), [
    ["claude", "session", 18_000], ["claude", "session", 604_800], ["claude", "weekly", 18_000], ["codex", "session", 18_000],
  ]);
  const reversed = providers.toReversed().map((state) => ({
    ...state, snapshot: { ...state.snapshot, limits: state.snapshot.limits.toReversed() },
  }));
  assert.deepEqual(upcomingResets(reversed, settings, now), rows);
});

function freeze(value) {
  if (value && typeof value === "object") {
    for (const child of Object.values(value)) freeze(child);
    Object.freeze(value);
  }
  return value;
}

test("sorting and returned row edits cannot mutate input snapshots or settings", () => {
  const providers = freeze([provider("claude", [quota("later", now + 10), quota("next", now + 1)])]);
  const frozenSettings = freeze({ ...settings });
  const original = structuredClone(providers);
  const rows = upcomingResets(providers, frozenSettings, now);
  rows[0].label = "Changed label";
  rows[0].resetAt = 0;
  assert.deepEqual(providers, original);
  assert.deepEqual(frozenSettings, settings);
  assert.equal(upcomingResets(providers, frozenSettings, now)[0].label, "next");
});
