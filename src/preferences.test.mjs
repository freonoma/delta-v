import assert from "node:assert/strict";
import test from "node:test";
import {
  displayPreferences, latestSnapshot, previewDisplayPreferences,
  previewPanelPreferences, previewProviderSelection,
} from "./preference-state.ts";

const settings = {
  providers: "both", claude_enabled: true, codex_enabled: true,
  launch_at_login_prompt_dismissed: false, history_recording: false,
  history_retention: "forever", tracked_limit: "codex:weekly", threshold: 20,
  refresh_seconds: 60, theme: "system", percentage_mode: "remaining",
  claude_windows: [], codex_windows: [],
};

test("an old display draft cannot carry immediate preferences into its save payload", () => {
  const draft = { ...settings, theme: "dark", claude_windows: ["session"] };
  const payload = displayPreferences(draft);
  assert.deepEqual(payload, {
    tracked_limit: "codex:weekly", threshold: 20, refresh_seconds: 60,
    theme: "dark", percentage_mode: "remaining", claude_windows: ["session"], codex_windows: [],
  });
  draft.claude_windows.push("weekly");
  assert.deepEqual(payload.claude_windows, ["session"]);
});

test("display and provider changes preserve immediate preferences in either save order", () => {
  const draft = displayPreferences({ ...settings, theme: "dark", threshold: 15 });
  const current = {
    ...settings, claude_enabled: false, codex_enabled: false,
    launch_at_login_prompt_dismissed: true, history_recording: true, history_retention: "days30",
  };
  const displayFirst = previewProviderSelection(previewDisplayPreferences(current, draft), "claude");
  const providerFirst = previewDisplayPreferences(previewProviderSelection(current, "claude"), draft);
  assert.deepEqual(displayFirst, providerFirst);
  assert.deepEqual(providerFirst, {
    ...current, providers: "claude", tracked_limit: "auto", theme: "dark", threshold: 15,
  });
});

test("provider selection preserves newly saved display choices and compatible tracking", () => {
  const current = previewDisplayPreferences(settings, {
    ...displayPreferences(settings), threshold: 10, refresh_seconds: 120,
    percentage_mode: "used", codex_windows: ["weekly"],
  });
  assert.deepEqual(previewProviderSelection(current, "codex"), { ...current, providers: "codex" });
});

test("a layout patch cannot undo pin or mini changes from another view", () => {
  const panel = { pinned: true, mini: true, expanded: true, layout: "columns" };
  const layout = { layout: "stacked" };
  const unpinFirst = previewPanelPreferences(previewPanelPreferences(panel, { pinned: false }), layout);
  const layoutFirst = previewPanelPreferences(previewPanelPreferences(panel, layout), { pinned: false });
  assert.deepEqual(unpinFirst, layoutFirst);
  assert.deepEqual(unpinFirst, { pinned: false, mini: false, expanded: false, layout: "stacked" });
  assert.deepEqual(previewPanelPreferences(panel, layout), { ...panel, layout: "stacked" });
});

test("late reads, replies and events cannot undo a newer app snapshot", () => {
  const initialRead = { revision: 1, settings, providers: [], settings_error: "Old error", paused: false };
  const displayReply = { ...initialRead, revision: 2, settings: { ...settings, theme: "dark" }, settings_error: null };
  const providerEvent = { ...displayReply, revision: 4, settings: { ...displayReply.settings, providers: "claude" }, paused: true };
  // Both a read arriving after an event and a command reply arriving after an event.
  for (const order of [
    [providerEvent, displayReply, initialRead],
    [initialRead, providerEvent, displayReply],
    [displayReply, initialRead, providerEvent],
  ]) {
    assert.strictEqual(order.reduce(latestSnapshot, null), providerEvent);
  }
});

test("panel error recovery survives a delayed error event or command reply", () => {
  const initial = { revision: 0, preferences: { pinned: false, mini: false, expanded: false, layout: "columns" }, error: null };
  const failure = { ...initial, revision: 1, error: "Could not save the panel layout." };
  const recovered = { ...initial, revision: 2, preferences: { ...initial.preferences, pinned: true } };
  assert.strictEqual(latestSnapshot(null, initial), initial);
  assert.strictEqual([failure, recovered, failure, initial].reduce(latestSnapshot, null), recovered);
  // Panel reads and no-op writes may legitimately share the same revision.
  assert.deepEqual(latestSnapshot(recovered, { ...recovered }), recovered);
});
