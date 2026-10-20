import assert from "node:assert/strict";
import test from "node:test";
import {
  availabilityFailure, availabilityPermissionMessage, availabilityQuota, createAvailabilityPreparation,
  currentAvailabilityWatch, emptyAvailabilityStatus, exhaustedAvailabilityQuota,
} from "./availability-state.ts";
import {
  completeAvailabilityPreview, createAvailabilityPreview, readAvailabilityPreview,
} from "./availability-preview.ts";
import { createNotificationPreview } from "./notification-state.ts";

const now = 1000;

test("watch previews share permission with low alerts without enabling them", () => {
  for (const mode of ["ready", "pending", "denied", "unavailable", "read_error"]) {
    const search = `?watches=${mode}`;
    const watches = createAvailabilityPreview(search, now);
    const low = createNotificationPreview(search);
    assert.deepEqual(low.permission, watches.permission);
    assert.equal(low.enabled, false);
  }
  assert.equal(createNotificationPreview("?watches=ready&notifications=enabled").enabled, true);
});
const quota = (id = "session") => ({
  id, label: "5-hour", enabled: true, kind: "quota", provenance: "official",
  used_fraction: 1, resets_at: 5000, window_seconds: 18000, amount: null, detail: null,
});
function provider() {
  return {
    id: "claude", reading_id: "claude:preview:1",
    snapshot: { provider: "claude", fetched_at: 990, limits: [quota()], plan: null, warnings: [] },
    error: null, recovery: null, refreshing: false, stale: false, next_retry_at: null,
  };
}
function app() {
  return {
    revision: 1, settings: { claude_enabled: true, codex_enabled: true, providers: "both", notifications_enabled: false },
    providers: [provider()], paused: false, settings_error: null, cooldown_error: null,
  };
}
function status() {
  return {
    ...emptyAvailabilityStatus(),
    providers: [{
      provider: "claude", reading_id: "claude:preview:1", preparing: false,
      quotas: [{ limit_id: "session", window_seconds: 18000, readiness: "ready" }],
    }],
    watches: [{
      id: "1", provider: "claude", limit_id: "session", label: "5-hour",
      window_seconds: 18000, armed_at: 900, account_match: "current",
    }],
  };
}
const arm = { kind: "arm", provider: "claude", limitId: "session", readingId: "claude:preview:1" };

test("visible readiness and existing watches must match the current accepted reading", () => {
  const current = provider();
  const state = status();
  assert.equal(availabilityQuota(state, current, current.snapshot.limits[0], now)?.readiness, "ready");
  assert.equal(currentAvailabilityWatch(state, current, current.snapshot.limits[0])?.id, "1");
  for (const reading_id of [null, undefined, "claude:preview:2"]) {
    const changed = { ...current, reading_id };
    assert.equal(availabilityQuota(state, changed, changed.snapshot.limits[0], now), null);
    assert.equal(currentAvailabilityWatch(state, changed, changed.snapshot.limits[0]), undefined);
  }
});

test("only current-account watches match quota controls, without hiding other saved watches", () => {
  const current = provider();
  for (const account_match of ["different", "unverified"]) {
    const state = status();
    state.watches[0].account_match = account_match;
    assert.equal(currentAvailabilityWatch(state, current, current.snapshot.limits[0]), undefined);
    assert.equal(state.watches.length, 1);
    assert.ok(availabilityQuota(state, current, current.snapshot.limits[0], now));
  }
  const state = status();
  state.watches[0].window_seconds = 604800;
  assert.equal(currentAvailabilityWatch(state, current, current.snapshot.limits[0]), undefined);
});

test("only exact exhaustion allows an action, including fractions that round to zero remaining", () => {
  for (const used_fraction of [0, 0.999, 0.999999999999, 1.00001, NaN, Infinity, null]) {
    const current = provider();
    current.snapshot.limits[0].used_fraction = used_fraction;
    assert.equal(exhaustedAvailabilityQuota(current, current.snapshot.limits[0], now), false, String(used_fraction));
  }
});

test("elapsed, missing, invalid and unavailable quota readings stay unavailable", () => {
  const changes = [
    { resets_at: null }, { resets_at: now }, { resets_at: now - 1 }, { resets_at: Number.MAX_SAFE_INTEGER },
    { resets_at: NaN }, { enabled: false }, { provenance: "local_estimate" }, { kind: "credits" },
    { window_seconds: 0 }, { id: "additional:0" }, { id: "quota:row:1" },
  ];
  for (const change of changes) {
    const current = provider();
    Object.assign(current.snapshot.limits[0], change);
    assert.equal(exhaustedAvailabilityQuota(current, current.snapshot.limits[0], now), false, JSON.stringify(change));
  }
  for (const change of [{ error: { kind: "network", message: "Failed" } }, { recovery: "checking" }]) {
    const current = { ...provider(), ...change };
    assert.equal(availabilityQuota(status(), current, current.snapshot.limits[0], now), null);
  }
});

test("an elapsed sibling does not block a valid exhausted quota in the accepted reading", () => {
  const current = provider();
  current.stale = true;
  current.snapshot.limits.push({ ...quota("weekly"), resets_at: now - 1 });
  assert.ok(availabilityQuota(status(), current, current.snapshot.limits[0], now));
  assert.equal(availabilityQuota(status(), current, current.snapshot.limits[1], now), null);
});

test("duplicate and parser-disambiguated quota identities cannot offer an action", () => {
  for (const extra of [quota(), quota("session:row:1")]) {
    const current = provider();
    current.snapshot.limits.push(extra);
    assert.equal(availabilityQuota(status(), current, current.snapshot.limits[0], now), null);
  }
});

test("preparing identity never arms automatically and only an explicit retry can save", () => {
  const search = "?watches=identity&watches_verify=ready";
  const initial = createAvailabilityPreview(search, now);
  const prepared = completeAvailabilityPreview(initial, { kind: "prepare", provider: "claude" }, app(), search, now);
  assert.deepEqual(initial.prepared, []);
  assert.deepEqual(prepared.model.prepared, ["claude"]);
  const passive = readAvailabilityPreview(prepared.model, app(), search, now);
  assert.equal(passive.state.providers[0].quotas[0].readiness, "needs_identity");
  assert.equal(passive.state.watches.length, 0);
  const tooSoon = completeAvailabilityPreview(passive.model, arm, app(), search, now);
  assert.match(tooSoon.error, /verified account/);
  const checked = readAvailabilityPreview(passive.model, app(), search, now, true);
  assert.equal(checked.state.providers[0].quotas[0].readiness, "ready");
  assert.equal(checked.state.watches.length, 0);
  const saved = completeAvailabilityPreview(checked.model, arm, app(), search, now);
  assert.equal(saved.error, null);
  assert.equal(saved.model.watches.length, 1);
  assert.deepEqual(saved.model.prepared, []);
});

test("an explicit prepare result may verify identity without automatically saving a watch", () => {
  const search = "?watches=identity&watches_prepare_result=ready";
  const result = completeAvailabilityPreview(createAvailabilityPreview(search, now),
    { kind: "prepare", provider: "claude" }, app(), search, now);
  const state = readAvailabilityPreview(result.model, app(), search, now).state;
  assert.equal(state.providers[0].quotas[0].readiness, "ready");
  assert.equal(state.watches.length, 0);
});

test("save failures do not invent a saved watch or remove cancellation targets", () => {
  const initial = createAvailabilityPreview("?watches=ready", now);
  const failed = completeAvailabilityPreview(initial, arm, app(), "?watches_arm_result=save_error", now);
  assert.match(failed.error, /save/);
  assert.strictEqual(failed.model, initial);
  const pending = createAvailabilityPreview("?watches=pending", now);
  const cancelled = completeAvailabilityPreview(pending, { kind: "cancel", id: "1" }, app(),
    "?watches_cancel_result=save_error", now);
  assert.match(cancelled.error, /unchanged/);
  assert.equal(cancelled.model.watches.length, 2);
});

test("permission failures do not block cancellation, including hidden and disconnected watches", () => {
  for (const mode of ["denied", "unavailable", "read_error"]) {
    const search = `?watches=${mode}`;
    const initial = createAvailabilityPreview(search, now);
    const current = app();
    current.settings.providers = "codex";
    current.settings.claude_enabled = false;
    current.providers[0].snapshot = null;
    const state = readAvailabilityPreview(initial, current, search, now).state;
    assert.equal(state.watches.length, 2);
    assert.equal(state.watches[0].account_match, "unverified");
    const cancelled = completeAvailabilityPreview(initial, { kind: "cancel", id: "1" }, current, search, now);
    assert.equal(cancelled.error, null, mode);
    assert.equal(cancelled.model.watches.length, 1);
  }
});

test("original-account watches remain saved when a new current-account watch is armed", () => {
  const initial = createAvailabilityPreview("?watches=other_account", now);
  const result = completeAvailabilityPreview(initial, arm, app(), "?watches=other_account", now);
  assert.equal(result.error, null);
  assert.equal(result.model.watches.length, 3);
  assert.equal(result.model.watches[0].account_match, "different");
  assert.equal(result.model.watches[2].account_match, "current");
  const duplicate = completeAvailabilityPreview(result.model, arm, app(), "?watches=other_account", now);
  assert.equal(duplicate.model.watches.length, 3);
});

test("preview arming is bound to the visible reading and does not alter low-allowance settings", () => {
  const current = app();
  const initial = createAvailabilityPreview("?watches=ready", now);
  const stale = completeAvailabilityPreview(initial, { ...arm, readingId: "older" }, current, "", now);
  assert.match(stale.error, /reading changed/);
  assert.equal(stale.model.watches.length, 0);
  const saved = completeAvailabilityPreview(initial, arm, current, "", now);
  assert.equal(saved.error, null);
  assert.equal(current.settings.notifications_enabled, false);
});

test("readback exposes committed changes even when durability confirmation failed", () => {
  const initial = createAvailabilityPreview("?watches=pending", now);
  const result = completeAvailabilityPreview(initial, { kind: "cancel", id: "1" }, app(),
    "?watches_cancel_result=postcommit_error", now);
  assert.match(result.error, /durable storage/);
  assert.equal(readAvailabilityPreview(result.model, app(), "", now).state.watches.length, 1);
  assert.equal(initial.watches.length, 2);
});

test("permission wording covers unavailable, blocked and undecided states", () => {
  assert.equal(availabilityPermissionMessage(createAvailabilityPreview("?watches=ready", now).permission), null);
  assert.match(availabilityPermissionMessage(createAvailabilityPreview("", now).permission), /ask macOS/);
  assert.match(availabilityPermissionMessage(createAvailabilityPreview("?watches=denied", now).permission), /blocked/);
  assert.match(availabilityPermissionMessage(null), /Check again/);
  assert.match(availabilityFailure("cancel", { internal: "do not expose" }), /Could not cancel/);
});

test("abandoning an in-flight preparation clears demand before a later attempt starts", async () => {
  let finish;
  const waiting = new Promise((resolve) => { finish = resolve; });
  const calls = [];
  let attempts = 0;
  const preparations = createAvailabilityPreparation(async (provider) => {
    calls.push(`prepare:${provider}`);
    if (++attempts === 1) await waiting;
  }, async (provider) => { calls.push(`clear:${provider}`); });
  const first = preparations.prepare("claude");
  await Promise.resolve();
  const abandoned = preparations.abandon();
  const second = preparations.prepare("claude");
  finish();
  assert.equal(await first, false);
  assert.equal(await abandoned, true);
  assert.equal(await second, true);
  assert.equal(calls.at(-1), "prepare:claude");
  assert.ok(calls.indexOf("clear:claude") > calls.indexOf("prepare:claude"));
  const count = calls.length;
  await Promise.resolve();
  assert.equal(calls.length, count);
});

test("abandonment before preparation starts skips the obsolete request", async () => {
  const calls = [];
  const preparations = createAvailabilityPreparation(async (provider) => { calls.push(`prepare:${provider}`); },
    async (provider) => { calls.push(`clear:${provider}`); });
  const first = preparations.prepare("claude");
  const abandoned = preparations.abandon();
  assert.equal(await first, false);
  assert.equal(await abandoned, true);
  assert.deepEqual(calls, ["clear:claude"]);
});

test("failed cleanup is retryable and does not prevent a later explicit preparation", async () => {
  let fail = true;
  const calls = [];
  const preparations = createAvailabilityPreparation(async (provider) => { calls.push(`prepare:${provider}`); },
    async (provider) => { if (fail) throw new Error("synthetic clear failure"); calls.push(`clear:${provider}`); });
  await preparations.prepare("claude");
  await assert.rejects(preparations.clear("claude"), /synthetic clear/);
  fail = false;
  assert.equal(await preparations.clear("claude"), true);
  assert.equal(await preparations.prepare("claude"), true);
  assert.deepEqual(calls, ["prepare:claude", "clear:claude", "prepare:claude"]);
});

test("failed dismissal keeps ownership for the next dismissal to retry", async () => {
  let attempts = 0;
  const preparations = createAvailabilityPreparation(async () => {}, async () => {
    if (++attempts === 1) throw new Error("synthetic clear failure");
  });
  await preparations.prepare("claude");
  await assert.rejects(preparations.abandon(), /synthetic clear/);
  assert.equal(await preparations.abandon(), true);
  assert.equal(attempts, 2);
  await preparations.abandon();
  assert.equal(attempts, 2);
});
