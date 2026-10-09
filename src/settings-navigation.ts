import { invoke } from "@tauri-apps/api/core";
import { native, preview } from "./ui-state";

export const settingsSections = ["display", "accounts", "history", "app"] as const;
export type SettingsSection = typeof settingsSections[number];

export function isSettingsSection(value: unknown): value is SettingsSection {
  return settingsSections.some((section) => section === value);
}

export async function openSettings(section?: SettingsSection): Promise<void> {
  if (native) await invoke("open_settings_window", { section: section ?? null });
  else if (preview) {
    const parameters = new URLSearchParams(window.location.search);
    parameters.set("view", "settings");
    parameters.set("section", section ?? "display");
    window.location.assign(`?${parameters}`);
  }
}
