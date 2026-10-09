import type { DisplayPreferences, MiniLayout, ProviderSelection } from "./types";
import { displayPreferences, visibleTrackedLimit } from "./preference-state.ts";

export interface SettingsDraft {
  preferences: DisplayPreferences;
  layout: MiniLayout;
  threshold: string;
  interval: string;
}

export type SettingsDraftValidation =
  | { preferences: DisplayPreferences; layout: MiniLayout; error: null }
  | { error: string; field: "threshold" | "interval" };

export function createSettingsDraft(settings: DisplayPreferences, layout: MiniLayout): SettingsDraft {
  return {
    preferences: displayPreferences(settings),
    layout,
    threshold: String(settings.threshold),
    interval: String(settings.refresh_seconds),
  };
}

function integerInRange(text: string, minimum: number, maximum: number): number | null {
  if (!text.trim()) return null;
  const value = Number(text);
  return Number.isInteger(value) && value >= minimum && value <= maximum ? value : null;
}

function sameNumberText(left: string, right: string, minimum: number, maximum: number): boolean {
  if (left === right) return true;
  const leftValue = integerInRange(left, minimum, maximum);
  const rightValue = integerInRange(right, minimum, maximum);
  return leftValue !== null && rightValue !== null && leftValue === rightValue;
}

function sameWindows(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

export function isSettingsDraftDirty(draft: SettingsDraft, baseline: SettingsDraft): boolean {
  const current = draft.preferences;
  const saved = baseline.preferences;
  return current.tracked_limit !== saved.tracked_limit
    || current.theme !== saved.theme
    || current.percentage_mode !== saved.percentage_mode
    || !sameWindows(current.claude_windows, saved.claude_windows)
    || !sameWindows(current.codex_windows, saved.codex_windows)
    || draft.layout !== baseline.layout
    || !sameNumberText(draft.threshold, baseline.threshold, 0, 100)
    || !sameNumberText(draft.interval, baseline.interval, 30, 900);
}

export function validateSettingsDraft(draft: SettingsDraft, providers: ProviderSelection): SettingsDraftValidation {
  const threshold = integerInRange(draft.threshold, 0, 100);
  if (threshold === null) {
    return { error: "Enter a whole-number remaining threshold from 0 to 100.", field: "threshold" };
  }
  const interval = integerInRange(draft.interval, 30, 900);
  if (interval === null) {
    return { error: "Enter a refresh interval from 30 to 900 seconds.", field: "interval" };
  }
  return {
    preferences: {
      ...displayPreferences(draft.preferences),
      tracked_limit: visibleTrackedLimit(draft.preferences.tracked_limit, providers),
      threshold,
      refresh_seconds: interval,
    },
    layout: draft.layout,
    error: null,
  };
}

export function reconcileSettingsDraft(draft: SettingsDraft, baseline: SettingsDraft, nextBaseline: SettingsDraft): SettingsDraft {
  const current = draft.preferences;
  const saved = baseline.preferences;
  const next = nextBaseline.preferences;
  return {
    preferences: {
      tracked_limit: current.tracked_limit === saved.tracked_limit ? next.tracked_limit : current.tracked_limit,
      theme: current.theme === saved.theme ? next.theme : current.theme,
      percentage_mode: current.percentage_mode === saved.percentage_mode ? next.percentage_mode : current.percentage_mode,
      claude_windows: [...(sameWindows(current.claude_windows, saved.claude_windows) ? next.claude_windows : current.claude_windows)],
      codex_windows: [...(sameWindows(current.codex_windows, saved.codex_windows) ? next.codex_windows : current.codex_windows)],
      // The editable text is authoritative until validation creates the save payload.
      threshold: next.threshold,
      refresh_seconds: next.refresh_seconds,
    },
    layout: draft.layout === baseline.layout ? nextBaseline.layout : draft.layout,
    threshold: sameNumberText(draft.threshold, baseline.threshold, 0, 100) ? nextBaseline.threshold : draft.threshold,
    interval: sameNumberText(draft.interval, baseline.interval, 30, 900) ? nextBaseline.interval : draft.interval,
  };
}
