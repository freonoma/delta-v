use tauri::WebviewWindow;
use tauri_nspanel::{
    objc2::{MainThreadMarker, rc::autoreleasepool},
    objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSColor, NSWindow,
    },
};

use crate::settings::Theme;

const WINDOW: &str = "settings";

#[derive(Debug, thiserror::Error)]
enum AppearanceError {
    #[error("Appearance can only be changed in Settings.")]
    WrongWindow,
    #[error("Could not update Settings appearance. Try again.")]
    Update,
}

pub async fn apply(window: WebviewWindow, theme: Theme) -> Result<(), String> {
    require_settings_window(&window).map_err(|error| error.to_string())?;
    let target = window.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let _ = sender.send(apply_on_main(&target, theme));
        })
        .map_err(|_| AppearanceError::Update.to_string())?;
    receiver
        .await
        .map_err(|_| AppearanceError::Update.to_string())?
}

pub fn apply_on_main(window: &WebviewWindow, theme: Theme) -> Result<(), String> {
    apply_native(window, theme).map_err(|error| error.to_string())
}

fn require_settings_window(window: &WebviewWindow) -> Result<(), AppearanceError> {
    if window.label() == WINDOW {
        Ok(())
    } else {
        Err(AppearanceError::WrongWindow)
    }
}

fn apply_native(window: &WebviewWindow, theme: Theme) -> Result<(), AppearanceError> {
    require_settings_window(window)?;
    let _main_thread = MainThreadMarker::new().ok_or(AppearanceError::Update)?;
    autoreleasepool(|_| {
        let pointer = window.ns_window().map_err(|_| AppearanceError::Update)?;
        // Tauri returns this window's autoreleased NSWindow. Borrow it only on
        // the main thread and within this autorelease pool.
        let native =
            unsafe { pointer.cast::<NSWindow>().as_ref() }.ok_or(AppearanceError::Update)?;
        let appearance = match theme {
            Theme::System => None,
            Theme::Light => Some(
                NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua })
                    .ok_or(AppearanceError::Update)?,
            ),
            Theme::Dark => Some(
                NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua })
                    .ok_or(AppearanceError::Update)?,
            ),
        };
        // Tauri's theme setter changes NSApplication on macOS. A window-level
        // override keeps unsaved previews local; None restores inheritance.
        native.setAppearance(appearance.as_deref());
        // A transparent native title bar draws this window's background, not
        // the webview's CSS. Match Delta-V's surface for explicit appearances.
        let background = match theme {
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
        };
        native.setBackgroundColor(Some(&background));
        #[cfg(debug_assertions)]
        eprintln!(
            "Settings appearance: requested={theme:?}, effective={}",
            native.effectiveAppearance().name()
        );
        Ok(())
    })
}
