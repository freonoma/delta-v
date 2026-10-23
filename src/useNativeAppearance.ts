import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Theme, UiStyle } from "./types";
import { errorMessage, native } from "./ui-state";

export function useNativeAppearance({ target, theme, uiStyle, ready, onError }: {
  target: "panel" | "settings";
  theme: Theme;
  uiStyle: UiStyle;
  ready: boolean;
  onError: (message: string) => void;
}) {
  useEffect(() => {
    if (!native || !ready) return;
    let disposed = false;
    const applyAppearance = () => {
      void invoke(target === "panel" ? "set_panel_appearance" : "set_settings_appearance", { theme, uiStyle })
        .catch((caught: unknown) => { if (!disposed) onError(errorMessage(caught)); });
    };
    const transparency = window.matchMedia("(prefers-reduced-transparency: reduce)");
    const colorScheme = window.matchMedia("(prefers-color-scheme: dark)");
    applyAppearance();
    transparency.addEventListener("change", applyAppearance);
    colorScheme.addEventListener("change", applyAppearance);
    return () => {
      disposed = true;
      transparency.removeEventListener("change", applyAppearance);
      colorScheme.removeEventListener("change", applyAppearance);
    };
  }, [target, theme, uiStyle, ready, onError]);
}
