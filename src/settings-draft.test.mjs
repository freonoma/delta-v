import assert from "node:assert/strict";
import test from "node:test";
import {
  createSettingsDraft, isSettingsDraftDirty, reconcileSettingsDraft, validateSettingsDraft,
} from "./settings-draft.ts";

function settings(changes = {}) {
  return {
    providers: "both", claude_enabled: true, codex_enabled: true,
    launch_at_login_prompt_dismissed: false, history_recording: false, history_retention: "forever",
    tracked_limit: "codex:weekly", threshold: 20, refresh_seconds: 60,
    theme: "system", percentage_mode: "remaining", claude_windows: ["session"], codex_windows: [],
    ...changes,
  };
}

test("drafts and save payloads contain only form-owned preferences and independent arrays", () => {
  const source = settings();
  const draft = createSettingsDraft(source, "columns");
  assert.deepEqual(draft, {
    preferences: {
      tracked_limit: "codex:weekly", threshold: 20, refresh_seconds: 60,
      theme: "system", percentage_mode: "remaining", claude_windows: ["session"], codex_windows: [],
    },
    threshold: "20", interval: "60", layout: "columns",
  });
  source.claude_windows.push("weekly");
  assert.deepEqual(draft.preferences.claude_windows, ["session"]);
  const result = validateSettingsDraft(draft, "both");
  assert.equal(result.error, null);
  assert.deepEqual(result.preferences, draft.preferences);
  draft.preferences.claude_windows.push("model");
  assert.deepEqual(result.preferences.claude_windows, ["session"]);
});

test("dirty checks compare each owned field and preserve window order", () => {
  const baseline = createSettingsDraft(settings(), "columns");
  assert.equal(isSettingsDraftDirty(createSettingsDraft(settings(), "columns"), baseline), false);
  for (const change of [
    { tracked_limit: "auto" }, { theme: "dark" }, { percentage_mode: "used" },
    { claude_windows: [] }, { codex_windows: ["weekly"] },
  ]) {
    const draft = { ...baseline, preferences: { ...baseline.preferences, ...change } };
    assert.equal(isSettingsDraftDirty(draft, baseline), true);
  }
  for (const change of [{ layout: "stacked" }, { threshold: "10" }, { interval: "120" }]) {
    assert.equal(isSettingsDraftDirty({ ...baseline, ...change }, baseline), true);
  }
  const windows = createSettingsDraft(settings({ claude_windows: ["session", "weekly"] }), "columns");
  assert.equal(isSettingsDraftDirty({
    ...windows, preferences: { ...windows.preferences, claude_windows: ["weekly", "session"] },
  }, windows), true);
});

test("valid numeric equivalents are clean but invalid input stays dirty", () => {
  const baseline = createSettingsDraft(settings(), "columns");
  for (const threshold of [" 20 ", "020", "20.0", "2e1"]) {
    assert.equal(isSettingsDraftDirty({ ...baseline, threshold, interval: " 60.0 " }, baseline), false);
  }
  for (const threshold of ["", " ", "NaN", "Infinity", "20.1", "101", "-1"]) {
    assert.equal(isSettingsDraftDirty({ ...baseline, threshold }, baseline), true);
  }
});

test("validation rejects invalid thresholds before intervals and accepts inclusive limits", () => {
  const draft = createSettingsDraft(settings(), "columns");
  for (const threshold of ["", " ", "no", "Infinity", "-1", "100.1", "101", "1.5"]) {
    assert.deepEqual(validateSettingsDraft({ ...draft, threshold, interval: "invalid" }, "both"), {
      error: "Enter a whole-number remaining threshold from 0 to 100.", field: "threshold",
    });
  }
  for (const interval of ["", " ", "no", "Infinity", "29", "901", "60.5"]) {
    assert.deepEqual(validateSettingsDraft({ ...draft, interval }, "both"), {
      error: "Enter a refresh interval from 30 to 900 seconds.", field: "interval",
    });
  }
  for (const [threshold, interval] of [["0", "30"], ["100", "900"], [" 20.0 ", "6e1"]]) {
    const result = validateSettingsDraft({ ...draft, threshold, interval }, "both");
    assert.equal(result.error, null);
    assert.equal(result.preferences.threshold, Number(threshold));
    assert.equal(result.preferences.refresh_seconds, Number(interval));
  }
});

test("validation normalizes tracking against the latest provider selection", () => {
  const draft = createSettingsDraft(settings(), "stacked");
  assert.equal(validateSettingsDraft(draft, "claude").preferences.tracked_limit, "auto");
  assert.equal(validateSettingsDraft(draft, "codex").preferences.tracked_limit, "codex:weekly");
  assert.equal(validateSettingsDraft(draft, "both").preferences.tracked_limit, "codex:weekly");
  assert.equal(validateSettingsDraft(draft, "both").layout, "stacked");
  assert.equal(draft.preferences.tracked_limit, "codex:weekly");
});

test("provider normalization adopts untouched tracking without resetting an unsaved theme", () => {
  const baseline = createSettingsDraft(settings(), "columns");
  const draft = { ...baseline, preferences: { ...baseline.preferences, theme: "dark" } };
  const next = createSettingsDraft(settings({ providers: "claude", tracked_limit: "auto", history_recording: true }), "columns");
  const reconciled = reconcileSettingsDraft(draft, baseline, next);
  assert.deepEqual(reconciled, { ...next, preferences: { ...next.preferences, theme: "dark" } });
  assert.equal(isSettingsDraftDirty(reconciled, next), true);
  assert.equal("providers" in reconciled.preferences, false);
  assert.equal("history_recording" in reconciled.preferences, false);
});

test("reconciliation adopts each clean field independently and preserves edited fields", () => {
  const baseline = createSettingsDraft(settings(), "columns");
  const draft = {
    ...baseline, threshold: "invalid", interval: " 60.0 ",
    preferences: { ...baseline.preferences, claude_windows: ["weekly"], tracked_limit: "claude:weekly" },
  };
  const next = createSettingsDraft(settings({
    threshold: 25, refresh_seconds: 120, theme: "light", percentage_mode: "used", tracked_limit: "auto",
    claude_windows: ["model"], codex_windows: ["weekly"],
  }), "stacked");
  const reconciled = reconcileSettingsDraft(draft, baseline, next);
  assert.deepEqual(reconciled, {
    ...next, threshold: "invalid",
    preferences: { ...next.preferences, claude_windows: ["weekly"], tracked_limit: "claude:weekly" },
  });
  reconciled.preferences.claude_windows.push("session");
  reconciled.preferences.codex_windows.push("session");
  assert.deepEqual(draft.preferences.claude_windows, ["weekly"]);
  assert.deepEqual(next.preferences.codex_windows, ["weekly"]);
});

test("external mini layout changes preserve a local layout edit", () => {
  const baseline = createSettingsDraft(settings(), "columns");
  const draft = { ...baseline, layout: "stacked" };
  const next = createSettingsDraft(settings({ theme: "light" }), "columns");
  const reconciled = reconcileSettingsDraft(draft, baseline, next);
  assert.equal(reconciled.layout, "stacked");
  assert.equal(reconciled.preferences.theme, "light");
  const acknowledgement = createSettingsDraft(settings({ theme: "light" }), "stacked");
  assert.equal(isSettingsDraftDirty(reconcileSettingsDraft(reconciled, next, acknowledgement), acknowledgement), false);
});

test("advancing the baseline keeps local edits across repeated usage updates", () => {
  const baseline = createSettingsDraft(settings(), "columns");
  const draft = { ...baseline, threshold: "15", interval: "120" };
  const next = createSettingsDraft(settings({ threshold: 25, refresh_seconds: 90, theme: "light" }), "columns");
  const first = reconcileSettingsDraft(draft, baseline, next);
  const latest = createSettingsDraft(settings({ threshold: 30, refresh_seconds: 180, theme: "dark" }), "columns");
  const second = reconcileSettingsDraft(first, next, latest);
  assert.equal(second.threshold, "15");
  assert.equal(second.interval, "120");
  assert.equal(second.preferences.theme, "dark");
  const result = validateSettingsDraft(second, "both");
  assert.equal(result.preferences.threshold, 15);
  assert.equal(result.preferences.refresh_seconds, 120);
});

test("save acknowledgements normalize submitted fields while preserving later edits", () => {
  const submitted = createSettingsDraft(settings({ theme: "dark", threshold: 15 }), "stacked");
  const acknowledged = createSettingsDraft(settings({ theme: "dark", threshold: 15, tracked_limit: "auto" }), "stacked");
  const clean = reconcileSettingsDraft(submitted, submitted, acknowledged);
  assert.deepEqual(clean, acknowledged);
  assert.equal(isSettingsDraftDirty(clean, acknowledged), false);
  const editedDuringSave = {
    ...submitted, threshold: "10", layout: "columns",
    preferences: { ...submitted.preferences, theme: "light", codex_windows: ["weekly"] },
  };
  const reconciled = reconcileSettingsDraft(editedDuringSave, submitted, acknowledged);
  assert.deepEqual(reconciled, {
    ...editedDuringSave, preferences: { ...editedDuringSave.preferences, tracked_limit: "auto" },
  });
  assert.equal(isSettingsDraftDirty(reconciled, acknowledged), true);
});
