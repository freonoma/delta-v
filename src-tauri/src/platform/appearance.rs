use std::{cell::RefCell, collections::HashMap};

use tauri::{
    WebviewWindow,
    window::{Effect, EffectState, EffectsBuilder},
};
use tauri_nspanel::{
    objc2::{MainThreadMarker, rc::autoreleasepool},
    objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSColor, NSWindow, NSWorkspace,
    },
};

use crate::settings::{Theme, UiStyle};

const UPDATE_ERROR: &str = "Could not update the usage panel appearance. Try again.";

thread_local! {
    // Only the main thread touches native views. Keep the material installed
    // across theme updates rather than replacing it and resetting its state.
    static APPLIED_MATERIALS: RefCell<HashMap<String, (usize, bool)>> = RefCell::new(HashMap::new());
}

pub fn forget_window(label: &str) {
    APPLIED_MATERIALS.with(|applied| {
        applied.borrow_mut().remove(label);
    });
}

pub async fn apply(window: WebviewWindow, theme: Theme, ui_style: UiStyle) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Panel appearance can only be changed in the usage panel.".into());
    }
    let target = window.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let _ = sender.send(apply_on_main(&target, theme, ui_style));
        })
        .map_err(|_| UPDATE_ERROR.to_owned())?;
    receiver.await.map_err(|_| UPDATE_ERROR.to_owned())?
}

pub fn apply_on_main(
    window: &WebviewWindow,
    theme: Theme,
    ui_style: UiStyle,
) -> Result<(), String> {
    if !matches!(window.label(), "main" | "settings") {
        return Err(UPDATE_ERROR.into());
    }
    let is_settings = window.label() == "settings";
    let _main_thread = MainThreadMarker::new().ok_or(UPDATE_ERROR)?;
    autoreleasepool(|_| {
        let pointer = window.ns_window().map_err(|_| UPDATE_ERROR)?;
        // Tauri owns this window. Borrow it only on the main thread and within
        // this pool; its NSWindow appearance also controls the native material.
        let native = unsafe { pointer.cast::<NSWindow>().as_ref() }.ok_or(UPDATE_ERROR)?;
        let appearance = match theme {
            Theme::System => None,
            Theme::Light => Some(
                NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua })
                    .ok_or(UPDATE_ERROR)?,
            ),
            Theme::Dark => Some(
                NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua })
                    .ok_or(UPDATE_ERROR)?,
            ),
        };
        // A window-level override leaves Settings' unsaved preview independent.
        native.setAppearance(appearance.as_deref());

        let reduce_transparency =
            NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency();
        let glass = ui_style == UiStyle::Glass && !reduce_transparency;
        // Settings uses a transparent webview in both styles so an unsaved
        // style preview can add/remove glass without recreating the window.
        native.setOpaque(is_settings && !glass);
        let background = if is_settings && !glass {
            opaque_background(theme)
        } else if reduce_transparency && ui_style == UiStyle::Glass {
            NSColor::windowBackgroundColor()
        } else {
            NSColor::clearColor()
        };
        native.setBackgroundColor(Some(&background));

        let material = (pointer as usize, glass);
        APPLIED_MATERIALS.with(|applied| {
            let mut applied = applied.borrow_mut();
            if applied.get(window.label()) != Some(&material) {
                let effects = glass.then(|| {
                    EffectsBuilder::new()
                        // Tauri uses NSGlassEffectView on macOS 26+ and falls
                        // back to NSVisualEffectView on older supported macOS.
                        .effects([Effect::LiquidGlassRegular, Effect::Popover])
                        // The fallback remains active while other apps focus.
                        .state(EffectState::Active)
                        .radius(16.0)
                        .build()
                });
                window.set_effects(effects).map_err(|_| UPDATE_ERROR)?;
                applied.insert(window.label().to_owned(), material);
            }
            Ok::<_, &str>(())
        })?;

        Ok(())
    })
}

fn opaque_background(theme: Theme) -> tauri_nspanel::objc2::rc::Retained<NSColor> {
    // Preserve the Legacy native title bar and provide a solid accessibility
    // fallback even before the webview receives a transparency change.
    match theme {
        Theme::System => NSColor::windowBackgroundColor(),
        Theme::Light => NSColor::colorWithSRGBRed_green_blue_alpha(
            248.0 / 255.0,
            249.0 / 255.0,
            251.0 / 255.0,
            1.0,
        ),
        Theme::Dark => NSColor::colorWithSRGBRed_green_blue_alpha(
            27.0 / 255.0,
            32.0 / 255.0,
            40.0 / 255.0,
            1.0,
        ),
    }
}
