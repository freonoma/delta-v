import assert from "node:assert/strict";
import test from "node:test";
import {
  eligibleQuota, featuredQuota, hiddenLowQuota, miniLimits,
  quotaPercent, remainingPercent, usedPercent, wholePercent,
} from "./usage.ts";

const now = 1_800_000_000;
const quota = (id, used = 0.3, overrides = {}) => ({
  id, label: id, kind: "quota", used_fraction: used, resets_at: now + 3_600,
  window_seconds: 18_000, provenance: "official", enabled: true,
  amount: null, detail: null, ...overrides,
});

test("a missing first selection does not promote the second window", () => {
  const weekly = quota("weekly");
  assert.deepEqual(miniLimits("claude", [weekly], ["session", "weekly"], "auto", now), [undefined, weekly]);
});

test("explicit selection keeps its order and does not add unselected windows", () => {
  const session = quota("session");
  const weekly = quota("weekly", 0.95);
  assert.deepEqual(miniLimits("claude", [session, weekly], ["session"], "auto", now), [session]);
  assert.deepEqual(miniLimits("claude", [session, weekly], ["weekly", "session", "other"], "auto", now), [weekly, session]);
});

test("disabled and non-quota selections stay unavailable", () => {
  const session = quota("session", 0.3, { enabled: false });
  const credit = quota("credit", 0.3, { kind: "credits" });
  assert.deepEqual(miniLimits("claude", [session, credit], ["session", "credit"], "auto", now), [undefined, undefined]);
  assert.deepEqual(miniLimits("claude", [session, credit], [], "auto", now), []);
});

test("default Claude selection keeps the session first and weekly second", () => {
  const session = quota("session", 0.2);
  const weekly = quota("weekly", 0.5);
  assert.deepEqual(miniLimits("claude", [session, weekly], [], "auto", now), [session, weekly]);
});

test("a weekly-only provider has one correctly identified window", () => {
  const weekly = quota("rate_limit:primary_window", 0.5, { label: "Weekly", window_seconds: 604_800 });
  assert.deepEqual(miniLimits("codex", [weekly], [], "auto", now), [weekly]);
});

test("a tighter model quota remains the second default window", () => {
  const session = quota("session", 0.2);
  const weekly = quota("weekly", 0.5);
  const scoped = quota("seven_day_sonnet", 0.9);
  const limits = [session, weekly, scoped];
  assert.deepEqual(miniLimits("claude", limits, [], "auto", now), [session, scoped]);
  assert.deepEqual(miniLimits("claude", limits, [], "claude:weekly", now), [session, weekly]);
});

test("a missing tracked window does not select a different featured quota", () => {
  const session = quota("session");
  assert.equal(featuredQuota("claude", [session], "claude:missing", now), undefined);
  assert.equal(featuredQuota("claude", [session], "codex:missing", now), session);
});

test("hidden warnings follow the tightest hidden quota even after expansion", () => {
  const session = quota("session", 0.2);
  const weekly = quota("weekly", 0.85);
  const scoped = quota("seven_day_sonnet", 0.97);
  const limits = [session, weekly, scoped];
  assert.equal(hiddenLowQuota(limits, [session], 20, now), scoped);
  assert.equal(hiddenLowQuota(limits, [session, weekly], 20, now), scoped);
  assert.equal(hiddenLowQuota(limits, [session, scoped], 20, now), weekly);
  assert.equal(hiddenLowQuota(limits, [session, weekly, scoped], 20, now), undefined);
});

test("a missing visible slot cannot conceal a hidden warning", () => {
  const weekly = quota("weekly", 1);
  assert.equal(hiddenLowQuota([weekly], [undefined], 20, now), weekly);
});

test("low warnings use unrounded remaining percentage and a strict threshold", () => {
  const boundary = quota("weekly", 0.8);
  const below = quota("weekly", 0.801);
  assert.equal(hiddenLowQuota([boundary], [], 20, now), undefined);
  assert.equal(hiddenLowQuota([below], [], 20, now), below);
  assert.equal(hiddenLowQuota([quota("weekly", 1)], [], 0, now), undefined);
});

test("a reset boundary invalidates the number without replacing the chosen window", () => {
  const expired = quota("session", 0.9, { resets_at: now });
  const weekly = quota("weekly", 0.3);
  assert.equal(eligibleQuota(expired, now), false);
  assert.deepEqual(miniLimits("claude", [expired, weekly], ["session", "weekly"], "auto", now), [expired, weekly]);
  assert.equal(featuredQuota("claude", [expired, weekly], "auto", now), weekly);
  assert.equal(hiddenLowQuota([expired], [], 20, now), undefined);
  assert.equal(eligibleQuota(quota("session", 0.9, { resets_at: now + 1 }), now), true);
});

test("an absent reset timestamp is not an expired reading", () => {
  assert.equal(eligibleQuota(quota("session", 0.9, { resets_at: null }), now), true);
});

test("invalid, disabled and non-official numbers do not become valid zeroes", () => {
  const invalid = [
    quota("missing", null), quota("nan", NaN), quota("infinite", Infinity),
    quota("negative", -0.1), quota("overflow", 1.01),
    quota("disabled", 1, { enabled: false }),
    quota("estimate", 1, { provenance: "local_estimate" }),
    quota("unknown", 1, { provenance: "unknown" }),
    quota("credits", 1, { kind: "credits" }),
  ];
  for (const limit of invalid) assert.equal(eligibleQuota(limit, now), false, limit.id);
  assert.equal(featuredQuota("claude", invalid, "auto", now), undefined);
  assert.equal(hiddenLowQuota(invalid, [], 20, now), undefined);
  assert.equal(usedPercent(invalid[0]), null);
  assert.equal(remainingPercent(invalid[1]), null);
});

test("zero and fully consumed official quotas are valid readings", () => {
  assert.equal(eligibleQuota(quota("session", 0), now), true);
  assert.equal(eligibleQuota(quota("session", 1), now), true);
  assert.equal(remainingPercent(quota("session", 0)), 100);
  assert.equal(remainingPercent(quota("session", 1)), 0);
});

test("percentage modes preserve existing rounding at floating point boundaries", () => {
  const remaining = remainingPercent(quota("session", 0.58));
  assert.equal(remaining, 42);
  assert.equal(wholePercent(quotaPercent(remaining, "remaining"), "remaining"), 42);
  assert.equal(wholePercent(quotaPercent(remaining, "used"), "used"), 58);
  assert.equal(wholePercent(19.9, "remaining"), 20);
  assert.equal(wholePercent(80.1, "used"), 80);
});
