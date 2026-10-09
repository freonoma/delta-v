use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const WINDOW: &str = "settings";
const SECTION_EVENT: &str = "settings-section-requested";
const QUIT_EVENT: &str = "settings-quit-requested";

#[derive(Default)]
pub struct SettingsWindowState {
    pending_quit: AtomicBool,
    return_to_panel: AtomicBool,
    show_panel_on_close: AtomicBool,
    quitting: AtomicBool,
    last_section: Mutex<SettingsSection>,
}

impl SettingsWindowState {
    fn section_for_open(
        &self,
        requested: Option<SettingsSection>,
    ) -> Result<SettingsSection, SettingsSectionError> {
        let mut section = self
            .last_section
            .lock()
            .map_err(|_| SettingsSectionError::Update)?;
        if let Some(requested) = requested {
            *section = requested;
        }
        Ok(*section)
    }

    fn remember_panel(&self, should_return: bool) {
        if should_return {
            self.return_to_panel.store(true, Ordering::SeqCst);
        }
    }

    fn take_panel_return(&self) -> Option<bool> {
        let owned = self.return_to_panel.swap(false, Ordering::SeqCst);
        let explicit = self.show_panel_on_close.swap(false, Ordering::SeqCst);
        (!self.quitting.load(Ordering::SeqCst) && (owned || explicit)).then_some(explicit)
    }

    fn begin_quit(&self) {
        self.quitting.store(true, Ordering::SeqCst);
        self.return_to_panel.store(false, Ordering::SeqCst);
        self.show_panel_on_close.store(false, Ordering::SeqCst);
    }
}

pub fn request_quit(app: &AppHandle) -> tauri::Result<()> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(WINDOW) else {
            crate::runtime::request_quit(&handle);
            return;
        };
        // Keep the request until the listener consumes it, including during initial page loading.
        handle
            .state::<SettingsWindowState>()
            .pending_quit
            .store(true, Ordering::SeqCst);
        match crate::platform::suspend_for_settings(&handle, false) {
            Ok(should_return) => handle
                .state::<SettingsWindowState>()
                .remember_panel(should_return),
            Err(error) => eprintln!("Could not preserve the usage panel: {error}"),
        }
        if let Err(error) = focus_window(&window) {
            eprintln!("Could not focus Settings: {error}");
        }
        if let Err(error) = handle.emit_to(WINDOW, QUIT_EVENT, ()) {
            eprintln!("Could not request confirmation before quitting: {error}");
        }
    })
}

#[derive(Debug, thiserror::Error)]
#[error("Quit confirmation is only available in Settings.")]
pub struct QuitConfirmationError;

pub fn take_quit_request(window: &WebviewWindow) -> Result<bool, QuitConfirmationError> {
    require_settings_window(window)?;
    Ok(window
        .state::<SettingsWindowState>()
        .pending_quit
        .swap(false, Ordering::SeqCst))
}

pub fn confirm_quit(window: &WebviewWindow) -> Result<(), QuitConfirmationError> {
    require_settings_window(window)?;
    window.state::<SettingsWindowState>().begin_quit();
    crate::platform::cancel_settings_return(window.app_handle());
    crate::runtime::request_quit(window.app_handle());
    Ok(())
}

pub fn clear_quit_request(app: &AppHandle) {
    app.state::<SettingsWindowState>()
        .pending_quit
        .store(false, Ordering::SeqCst);
}

pub fn destroyed(app: &AppHandle) {
    clear_quit_request(app);
    if let Some(explicit) = app.state::<SettingsWindowState>().take_panel_return()
        && let Err(error) = crate::platform::return_from_settings(app, explicit)
    {
        eprintln!("Could not return to the usage panel: {error}");
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CloseSettingsError {
    #[error("This window cannot close Settings.")]
    WrongWindow,
    #[error("Could not close Settings. Try again.")]
    Close,
}

pub async fn close(window: WebviewWindow, show_panel: bool) -> Result<(), CloseSettingsError> {
    if window.label() != WINDOW {
        return Err(CloseSettingsError::WrongWindow);
    }
    let app = window.app_handle().clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        window
            .state::<SettingsWindowState>()
            .show_panel_on_close
            .store(show_panel, Ordering::SeqCst);
        // Restoring after Destroyed keeps the closing window's blur from hiding the panel again.
        let _ = sender.send(window.destroy());
    })
    .map_err(|_| CloseSettingsError::Close)?;
    receiver
        .await
        .map_err(|_| CloseSettingsError::Close)?
        .map_err(|_| CloseSettingsError::Close)
}

fn require_settings_window(window: &WebviewWindow) -> Result<(), QuitConfirmationError> {
    if window.label() == WINDOW {
        Ok(())
    } else {
        Err(QuitConfirmationError)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsSection {
    #[default]
    Display,
    Accounts,
    History,
    App,
}

impl SettingsSection {
    fn title(self) -> &'static str {
        match self {
            Self::Display => "Display & usage - Delta-V Settings",
            Self::Accounts => "Accounts - Delta-V Settings",
            Self::History => "History - Delta-V Settings",
            Self::App => "App - Delta-V Settings",
        }
    }

    fn path(self) -> String {
        let section = match self {
            Self::Display => "display",
            Self::Accounts => "accounts",
            Self::History => "history",
            Self::App => "app",
        };
        format!("index.html?view=settings&section={section}")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsSectionError {
    #[error("This window cannot change the Settings section.")]
    WrongWindow,
    #[error("Could not update the Settings section. Try again.")]
    Update,
}

pub async fn set_section(
    window: WebviewWindow,
    section: SettingsSection,
) -> Result<(), SettingsSectionError> {
    if window.label() != WINDOW {
        return Err(SettingsSectionError::WrongWindow);
    }
    let app = window.app_handle().clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(set_section_on_main(&window, section));
    })
    .map_err(|_| SettingsSectionError::Update)?;
    receiver.await.map_err(|_| SettingsSectionError::Update)?
}

fn set_section_on_main(
    window: &WebviewWindow,
    section: SettingsSection,
) -> Result<(), SettingsSectionError> {
    window
        .state::<SettingsWindowState>()
        .section_for_open(Some(section))?;
    window
        .set_title(section.title())
        .map_err(|_| SettingsSectionError::Update)
}

#[derive(Debug, thiserror::Error)]
#[error("Could not open Settings. Try again.")]
pub struct OpenSettingsError;

pub async fn open(
    app: &AppHandle,
    section: Option<SettingsSection>,
    from_panel: bool,
) -> Result<(), OpenSettingsError> {
    let handle = app.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    // Lookup and creation must share the main-thread queue: two callers can open Settings at once.
    app.run_on_main_thread(move || {
        let _ = sender.send(open_on_main(&handle, section, from_panel));
    })
    .map_err(|_| OpenSettingsError)?;
    receiver
        .await
        .map_err(|_| OpenSettingsError)?
        .map_err(|_| OpenSettingsError)
}

fn open_on_main(
    app: &AppHandle,
    section: Option<SettingsSection>,
    from_panel: bool,
) -> tauri::Result<()> {
    let window = if let Some(window) = app.get_webview_window(WINDOW) {
        if let Some(section) = section {
            set_section_on_main(&window, section)
                .map_err(|error| tauri::Error::Io(std::io::Error::other(error)))?;
            app.emit_to(WINDOW, SECTION_EVENT, section)?;
        }
        window
    } else {
        clear_quit_request(app);
        let state = app.state::<SettingsWindowState>();
        state.return_to_panel.store(false, Ordering::SeqCst);
        state.show_panel_on_close.store(false, Ordering::SeqCst);
        state.quitting.store(false, Ordering::SeqCst);
        let section = state
            .section_for_open(section)
            .map_err(|error| tauri::Error::Io(std::io::Error::other(error)))?;
        // The first section travels in the URL because the webview cannot listen yet.
        let mut builder =
            WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App(section.path().into()))
                .title(section.title())
                .title_bar_style(tauri::TitleBarStyle::Transparent)
                .hidden_title(true)
                .decorations(true)
                .resizable(true)
                .minimizable(false)
                .maximizable(false)
                .inner_size(700.0, 600.0)
                .min_inner_size(620.0, 480.0)
                .visible(false)
                .focused(false);

        let monitor = app
            .get_webview_window("main")
            .and_then(|window| window.current_monitor().ok().flatten())
            .or_else(|| app.primary_monitor().ok().flatten());
        if let Some(monitor) = monitor {
            let scale = monitor.scale_factor();
            let area = monitor.work_area();
            let origin = area.position.to_logical::<f64>(scale);
            let available = area.size.to_logical::<f64>(scale);
            let size = fitted_size(available.width, available.height);
            builder = builder
                .inner_size(size.width, size.height)
                .min_inner_size(size.min_width, size.min_height)
                .position(
                    origin.x + (available.width - size.width) / 2.0,
                    origin.y + (available.height - size.height - 32.0) / 2.0,
                );
        } else {
            builder = builder.center();
        }
        let window = builder.build()?;
        if let Ok(state) = app.state::<crate::runtime::Runtime>().state()
            && let Err(error) =
                crate::settings_appearance::apply_on_main(&window, state.settings.theme)
        {
            eprintln!("{error}");
        }
        window
    };
    let should_return = crate::platform::suspend_for_settings(app, from_panel)?;
    app.state::<SettingsWindowState>()
        .remember_panel(should_return);
    if let Err(error) = focus_window(&window) {
        if let Some(explicit) = app.state::<SettingsWindowState>().take_panel_return() {
            let _ = crate::platform::return_from_settings(app, explicit);
        }
        return Err(error);
    }
    Ok(())
}

fn focus_window(window: &WebviewWindow) -> tauri::Result<()> {
    window.unminimize()?;
    window.show()?;
    window.set_focus()
}

#[derive(Debug, PartialEq)]
struct WindowSize {
    width: f64,
    height: f64,
    min_width: f64,
    min_height: f64,
}

fn fitted_size(work_width: f64, work_height: f64) -> WindowSize {
    // Leave a margin and room for the native title bar, including on small displays.
    let width = (work_width - 32.0).clamp(1.0, 700.0);
    let height = (work_height - 64.0).clamp(1.0, 600.0);
    WindowSize {
        width,
        height,
        min_width: width.min(620.0),
        min_height: height.min(480.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_use_only_known_routes() {
        for (name, section) in [
            ("display", SettingsSection::Display),
            ("accounts", SettingsSection::Accounts),
            ("history", SettingsSection::History),
            ("app", SettingsSection::App),
        ] {
            assert_eq!(
                serde_json::from_value::<SettingsSection>(serde_json::json!(name)).unwrap(),
                section
            );
            assert_eq!(serde_json::to_value(section).unwrap(), name);
            assert_eq!(
                section.path(),
                format!("index.html?view=settings&section={name}")
            );
        }
        for invalid in ["", "Display", "updates", "../index.html", "app&view=main"] {
            assert!(serde_json::from_value::<SettingsSection>(serde_json::json!(invalid)).is_err());
        }
    }

    #[test]
    fn implicit_reopen_keeps_last_section_for_this_session() {
        let state = SettingsWindowState::default();
        assert_eq!(
            state.section_for_open(None).unwrap(),
            SettingsSection::Display
        );
        state
            .section_for_open(Some(SettingsSection::History))
            .unwrap();
        state.take_panel_return();
        assert_eq!(
            state.section_for_open(None).unwrap(),
            SettingsSection::History
        );
        assert_eq!(
            SettingsWindowState::default()
                .section_for_open(None)
                .unwrap(),
            SettingsSection::Display
        );
    }

    #[test]
    fn explicit_section_overrides_the_previous_section() {
        let state = SettingsWindowState::default();
        state
            .section_for_open(Some(SettingsSection::History))
            .unwrap();
        assert_eq!(
            state
                .section_for_open(Some(SettingsSection::Accounts))
                .unwrap(),
            SettingsSection::Accounts
        );
        assert_eq!(
            state.section_for_open(None).unwrap(),
            SettingsSection::Accounts
        );
    }

    #[test]
    fn initial_size_and_minimum_fit_small_work_areas() {
        assert_eq!(
            fitted_size(1440.0, 875.0),
            WindowSize {
                width: 700.0,
                height: 600.0,
                min_width: 620.0,
                min_height: 480.0
            }
        );
        assert_eq!(
            fitted_size(640.0, 480.0),
            WindowSize {
                width: 608.0,
                height: 416.0,
                min_width: 608.0,
                min_height: 416.0
            }
        );
    }

    #[test]
    fn menu_reopening_does_not_create_or_erase_a_panel_return() {
        let state = SettingsWindowState::default();
        state.remember_panel(false);
        assert_eq!(state.take_panel_return(), None);

        state.remember_panel(true);
        state.remember_panel(false);
        assert_eq!(state.take_panel_return(), Some(false));
        assert_eq!(state.take_panel_return(), None);
    }

    #[test]
    fn back_to_panel_explicitly_returns_without_an_origin() {
        let state = SettingsWindowState::default();
        state.remember_panel(false);
        state.show_panel_on_close.store(true, Ordering::SeqCst);
        assert_eq!(state.take_panel_return(), Some(true));
        assert_eq!(state.take_panel_return(), None);
    }

    #[test]
    fn confirmed_quit_cancels_both_owned_and_explicit_returns() {
        let state = SettingsWindowState::default();
        state.remember_panel(true);
        state.show_panel_on_close.store(true, Ordering::SeqCst);
        state.begin_quit();
        assert_eq!(state.take_panel_return(), None);
        // A late focus request during shutdown must not restore a panel either.
        state.remember_panel(true);
        assert_eq!(state.take_panel_return(), None);
    }
}
