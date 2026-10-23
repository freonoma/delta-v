use tauri::WebviewWindow;

use crate::settings::{Theme, UiStyle};

const WINDOW: &str = "settings";

#[derive(Debug, thiserror::Error)]
enum AppearanceError {
    #[error("Appearance can only be changed in Settings.")]
    WrongWindow,
    #[error("Could not update Settings appearance. Try again.")]
    Update,
}

pub async fn apply(window: WebviewWindow, theme: Theme, ui_style: UiStyle) -> Result<(), String> {
    require_settings_window(&window).map_err(|error| error.to_string())?;
    let target = window.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let _ = sender.send(apply_on_main(&target, theme, ui_style));
        })
        .map_err(|_| AppearanceError::Update.to_string())?;
    receiver
        .await
        .map_err(|_| AppearanceError::Update.to_string())?
}

pub fn apply_on_main(
    window: &WebviewWindow,
    theme: Theme,
    ui_style: UiStyle,
) -> Result<(), String> {
    require_settings_window(window).map_err(|error| error.to_string())?;
    crate::platform::appearance::apply_on_main(window, theme, ui_style)
        .map_err(|_| AppearanceError::Update.to_string())
}

fn require_settings_window(window: &WebviewWindow) -> Result<(), AppearanceError> {
    if window.label() == WINDOW {
        Ok(())
    } else {
        Err(AppearanceError::WrongWindow)
    }
}
