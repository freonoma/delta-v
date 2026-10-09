import type { DisplayPreferences, PanelPreferences, ProviderSelection, Settings } from "./types";

// A command reply or initial read can arrive after a newer event.
export function latestSnapshot<T extends { revision: number }>(current: T | null, incoming: T): T {
  return current && incoming.revision < current.revision ? current : incoming;
}

export function visibleTrackedLimit(tracked: string, providers: ProviderSelection): string {
  return providers === "both" || tracked === "auto" || tracked.startsWith(`${providers}:`)
    ? tracked : "auto";
}

export function displayPreferences(settings: DisplayPreferences): DisplayPreferences {
  return {
    tracked_limit: settings.tracked_limit,
    threshold: settings.threshold,
    refresh_seconds: settings.refresh_seconds,
    theme: settings.theme,
    percentage_mode: settings.percentage_mode,
    claude_windows: [...settings.claude_windows],
    codex_windows: [...settings.codex_windows],
  };
}

// Browser previews follow the same field ownership and normalization as native saves.
export function previewDisplayPreferences(current: Settings, changes: DisplayPreferences): Settings {
  return {
    ...current,
    ...displayPreferences(changes),
    tracked_limit: visibleTrackedLimit(changes.tracked_limit, current.providers),
  };
}

export function previewProviderSelection(current: Settings, providers: ProviderSelection): Settings {
  return { ...current, providers, tracked_limit: visibleTrackedLimit(current.tracked_limit, providers) };
}

export function previewPanelPreferences(current: PanelPreferences, patch: Partial<PanelPreferences>): PanelPreferences {
  const next = { ...current, ...patch };
  return next.pinned ? next : { ...next, mini: false, expanded: false };
}
