import assert from "node:assert/strict";
import test from "node:test";
import {
  completeNotificationPreview, createNotificationPreview, notificationActionAllowed,
  notificationAvailability, notificationFailure, notificationReadError, notificationStatusText,
  notificationTestNotice, readNotificationPreview,
} from "./notification-state.ts";

test("a new preview starts off without claiming permission was granted", () => {
  for (const search of ["", "?notifications=unexpected"]) {
    const state = createNotificationPreview(search);
    assert.equal(state.enabled, false);
    assert.equal(notificationAvailability(state), "not_determined");
    assert.equal(notificationActionAllowed(state, "enable"), true);
    assert.equal(notificationActionAllowed(state, "test"), false);
  }
});

test("saved enabled preference remains separate from permission and delivery failures", () => {
  for (const example of ["enabled", "denied", "alerts_disabled", "read_error", "delivery_error"]) {
    const state = createNotificationPreview(`?notifications=${example}`);
    assert.equal(state.enabled, true, example);
    assert.equal(notificationActionAllowed(state, "disable"), true, example);
  }
  const unavailable = { ...createNotificationPreview("?notifications=unavailable"), enabled: true };
  assert.equal(notificationActionAllowed(unavailable, "disable"), true);
  assert.equal(notificationActionAllowed(unavailable, "enable"), false);
});

test("permission failures block enable and test but retain a route to System Settings", () => {
  for (const example of ["denied", "alerts_disabled", "unavailable", "read_error"]) {
    const state = createNotificationPreview(`?notifications=${example}`);
    assert.equal(notificationActionAllowed(state, "enable"), false, example);
    assert.equal(notificationActionAllowed(state, "test"), false, example);
    assert.equal(notificationActionAllowed(state, "open"), true, example);
    assert.ok(notificationStatusText(state));
  }
});

test("a prior delivery error does not suppress valid permission or another explicit test", () => {
  const state = createNotificationPreview("?notifications=delivery_error");
  assert.equal(notificationAvailability(state), "ready");
  assert.equal(notificationActionAllowed(state, "test"), true);
  const tested = completeNotificationPreview(state, "test", "");
  assert.equal(tested.error, null);
  assert.equal(tested.state.error, null);
  assert.equal(tested.state.enabled, true);
});

test("provisional permission can deliver only when alerts and app availability allow it", () => {
  const enabled = createNotificationPreview("?notifications=enabled");
  const state = { ...enabled, permission: { ...enabled.permission, authorization: "provisional" } };
  assert.equal(notificationActionAllowed(state, "test"), true);
  assert.equal(notificationAvailability({ ...state, permission: { ...state.permission, alerts_enabled: false } }), "alerts_disabled");
  assert.equal(notificationAvailability({ ...state, permission: { ...state.permission, available: false } }), "unavailable");
  assert.equal(notificationAvailability({ ...state, permission: { ...state.permission, authorization: "unknown" } }), "unknown");
});

test("preview enable waits for an explicit result and never mutates its input", () => {
  const before = createNotificationPreview("");
  const success = completeNotificationPreview(before, "enable", "");
  assert.equal(before.enabled, false);
  assert.equal(before.permission.authorization, "not_determined");
  assert.equal(success.state.enabled, true);
  assert.equal(success.state.permission.authorization, "authorized");
  const denied = completeNotificationPreview(before, "enable", "?notifications_enable_result=denied");
  assert.equal(denied.state.enabled, false);
  assert.equal(notificationAvailability(denied.state), "denied");
  assert.match(denied.error, /System Settings/);
});

test("a failed save preserves the saved switch value", () => {
  for (const [example, action] of [["off", "enable"], ["enabled", "disable"]]) {
    const before = createNotificationPreview(`?notifications=${example}`);
    const result = completeNotificationPreview(before, action, "?notifications_result=save_error");
    assert.equal(result.state.enabled, before.enabled);
    assert.match(result.error, /save/);
    assert.equal(result.notice, null);
  }
});

test("successful disable stays off even when the permission read fails", () => {
  const before = createNotificationPreview("?notifications=enabled");
  const result = completeNotificationPreview(before, "disable", "?notifications_disable_result=read_error");
  assert.equal(result.state.enabled, false);
  assert.equal(result.state.permission, null);
  assert.equal(result.state.error, notificationReadError);
  assert.equal(result.error, null);
});

test("test failure preserves enabled choice without inventing a success notice", () => {
  const before = createNotificationPreview("?notifications=enabled");
  const result = completeNotificationPreview(before, "test", "?notifications_test_result=delivery_error");
  assert.equal(result.state.enabled, true);
  assert.equal(result.error, result.state.error);
  assert.match(result.error, /Could not send/);
  assert.equal(result.notice, null);
  const denied = completeNotificationPreview(before, "test", "?notifications_test_result=denied");
  assert.equal(denied.state.enabled, true);
  assert.equal(notificationActionAllowed(denied.state, "test"), false);
  assert.equal(denied.notice, null);
});

test("preview operations are explicit simulations and native wording only claims acceptance", () => {
  const state = createNotificationPreview("?notifications=enabled");
  const testResult = completeNotificationPreview(state, "test", "");
  assert.match(testResult.notice, /simulated/);
  assert.match(testResult.notice, /No macOS notification was sent/);
  const opened = completeNotificationPreview(state, "open", "");
  assert.match(opened.notice, /simulated/);
  assert.match(notificationTestNotice, /accepted.*request/);
  assert.match(notificationTestNotice, /Focus/);
  assert.doesNotMatch(notificationTestNotice, /delivered|was shown/);
});

test("operation-specific preview results override the shared result", () => {
  const state = createNotificationPreview("?notifications=enabled");
  const result = completeNotificationPreview(state, "test", "?notifications_result=error&notifications_test_result=enabled");
  assert.equal(result.error, null);
  assert.match(result.notice, /simulated/);
});

test("passive preview reads only change to an explicitly selected state", () => {
  const state = createNotificationPreview("?notifications=denied");
  assert.strictEqual(readNotificationPreview(state, ""), state);
  const restored = readNotificationPreview(state, "?notifications_read_result=enabled");
  assert.equal(notificationAvailability(restored), "ready");
  assert.equal(state.permission.authorization, "denied");
  const failed = readNotificationPreview(state, "?notifications_read_result=read_error");
  assert.equal(failed.enabled, true);
  assert.equal(failed.permission, null);
  const disabled = completeNotificationPreview(state, "disable", "").state;
  const readAfterDisable = readNotificationPreview(disabled, "?notifications_read_result=enabled");
  assert.equal(readAfterDisable.enabled, false);
  assert.equal(notificationAvailability(readAfterDisable), "ready");
});

test("follow-up preview reads preserve a test outcome when permission lookup then fails", () => {
  const state = createNotificationPreview("?notifications=enabled");
  const successful = completeNotificationPreview(state, "test", "");
  const refreshed = readNotificationPreview(successful.state, "?notifications_read_result=read_error");
  assert.equal(refreshed.enabled, true);
  assert.equal(refreshed.error, notificationReadError);
  assert.equal(successful.error, null);
  assert.match(successful.notice, /simulated/);
  const failed = completeNotificationPreview(state, "test", "?notifications_test_result=error");
  const recoveredRead = readNotificationPreview(failed.state, "?notifications_read_result=enabled");
  assert.equal(recoveredRead.error, null);
  assert.match(failed.error, /Could not send/);
  assert.equal(failed.notice, null);
});

test("untyped command failures have an action-specific recovery message", () => {
  for (const action of ["enable", "disable", "test", "open"]) {
    const message = notificationFailure(action, { internal: "unexpected transport detail" });
    assert.ok(message.length > 0);
    assert.doesNotMatch(message, /unexpected transport detail/);
  }
  assert.match(notificationFailure("open", null), /System Settings/);
  assert.equal(notificationFailure("test", "Wait before sending another test."), "Wait before sending another test.");
});
