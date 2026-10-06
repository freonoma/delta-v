import assert from "node:assert/strict";
import test from "node:test";
import { beginUpdateCheck, completeUpdatePreview, createUpdatePreview, emptyUpdateState, updateErrorMessage } from "./update-state.ts";

const now = 1_800_000_000;
const version = "0.3.0";

test("the initial preview makes no claim that a check has succeeded", () => {
  assert.deepEqual(createUpdatePreview("", version, now), emptyUpdateState());
  assert.deepEqual(createUpdatePreview("?updates=untrusted", version, now), emptyUpdateState());
});

test("checking is blocked until the existing request or cooldown ends", () => {
  const limited = createUpdatePreview("?updates=rate_limited", version, now);
  assert.equal(beginUpdateCheck(limited, now + 59), null);
  const checking = beginUpdateCheck(limited, now + 60);
  assert.equal(checking.checking, true);
  assert.equal(checking.error, null);
  assert.equal(checking.next_check_at, null);
  assert.equal(beginUpdateCheck(checking, now + 61), null);
});

test("starting a check preserves the prior result without changing its timestamp", () => {
  const previous = createUpdatePreview("?updates=current", version, now - 600);
  const checking = beginUpdateCheck(previous, now);
  assert.strictEqual(checking.last_success, previous.last_success);
  assert.equal(checking.last_success.checked_at, now - 600);
  assert.equal(previous.checking, false);
});

test("a successful check can still require waiting before another request", () => {
  const successful = { ...createUpdatePreview("?updates=current", version, now), next_check_at: now + 60 };
  assert.equal(successful.error, null);
  assert.equal(beginUpdateCheck(successful, now + 59), null);
  assert.strictEqual(beginUpdateCheck(successful, now + 60).last_success, successful.last_success);
});

test("failed checks retain the last success and never claim a fresh successful check", () => {
  const previous = createUpdatePreview("?updates=current", version, now - 600);
  for (const outcome of ["network", "rate_limited", "invalid_response", "service"]) {
    const result = completeUpdatePreview(previous, `?updates_result=${outcome}`, version, now);
    assert.strictEqual(result.last_success, previous.last_success);
    assert.equal(result.checking, false);
    assert.equal(result.error.kind, outcome);
    assert.equal(result.next_check_at, outcome === "rate_limited" ? now + 60 : null);
  }
});

test("a stale preview recovers to current after an explicit check by default", () => {
  const stale = createUpdatePreview("?updates=stale", version, now);
  assert.equal(stale.last_success.checked_at, now - 600);
  assert.equal(stale.error.kind, "network");
  const recovered = completeUpdatePreview(stale, "?updates=stale", version, now + 1);
  assert.equal(recovered.last_success.checked_at, now + 1);
  assert.equal(recovered.last_success.status, "current");
  assert.equal(recovered.error, null);
  assert.equal(recovered.next_check_at, null);
});

test("available and ahead fixtures remain distinct when the installed version changes", () => {
  for (const installed of ["0.3.0", "0.3.1", "2.4.5"]) {
    for (const outcome of ["available", "ahead"]) {
      const state = createUpdatePreview(`?updates=${outcome}`, installed, now);
      assert.equal(state.last_success.installed_version, installed);
      assert.notEqual(state.last_success.latest_version, installed);
      assert.equal(state.last_success.status, outcome);
    }
  }
});

test("error wording comes from known kinds rather than response bodies or HTTP details", () => {
  const errors = [
    { kind: "network" }, { kind: "invalid_response" },
    { kind: "service", status: 503, retry_at: null },
    { kind: "rate_limited", retry_at: now + 60 },
    { kind: "interrupted" }, { kind: "state_unavailable" },
  ];
  for (const error of errors) {
    const text = updateErrorMessage({ ...error, message: "private server response" });
    assert.equal(typeof text, "string");
    assert.ok(text.length > 0);
    assert.ok(!text.includes("private server response"));
    assert.ok(!text.includes("503"));
  }
});
