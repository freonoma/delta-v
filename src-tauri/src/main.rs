mod credentials;
mod model;
mod panel_state;
mod platform;
mod providers;
mod runtime;
mod settings;
mod tray;

use tauri::Manager;

#[tauri::command]
fn get_state(runtime: tauri::State<'_, runtime::Runtime>) -> Result<runtime::AppState, String> {
    runtime.state()
}

#[tauri::command]
fn refresh_usage(app: tauri::AppHandle) {
    runtime::request_refresh(&app);
}

#[tauri::command]
fn reconnect_provider(
    app: tauri::AppHandle,
    provider: model::ProviderId,
    action: platform::auth::Action,
) -> Result<(), String> {
    runtime::request_reconnect(&app, provider, action)
}

#[tauri::command]
fn cancel_reconnect(app: tauri::AppHandle, provider: model::ProviderId) -> Result<(), String> {
    runtime::cancel_reconnect(&app, provider)
}

#[tauri::command]
fn set_provider_enabled(
    app: tauri::AppHandle,
    provider: model::ProviderId,
    enabled: bool,
) -> Result<(), String> {
    runtime::set_provider_enabled(&app, provider, enabled)
}

#[tauri::command]
async fn open_setup_instructions(provider: model::ProviderId) -> Result<(), String> {
    tokio::task::spawn_blocking(move || platform::open_setup_instructions(provider))
        .await
        .map_err(|_| "Could not open the setup instructions. Please try again.".to_owned())?
}

#[tauri::command]
async fn get_login_item_state() -> Result<platform::login_item::LoginItemState, String> {
    tokio::task::spawn_blocking(|| platform::login_item::state().map_err(|error| error.to_string()))
        .await
        .map_err(|_| "Could not read the launch-at-login setting. Please try again.".to_owned())?
}

#[tauri::command]
async fn set_launch_at_login(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<platform::login_item::LoginItemState, String> {
    tokio::task::spawn_blocking(move || {
        let state =
            platform::login_item::set_enabled(enabled).map_err(|error| error.to_string())?;
        runtime::dismiss_launch_at_login_prompt(&app).map_err(|_| {
            "The macOS setting changed, but Delta-V could not save your choice. Please try again."
                .to_owned()
        })?;
        Ok(state)
    })
    .await
    .map_err(|_| "Could not change launch at login. Please try again.".to_owned())?
}

#[tauri::command]
fn dismiss_launch_at_login_prompt(app: tauri::AppHandle) -> Result<(), String> {
    runtime::dismiss_launch_at_login_prompt(&app)
}

#[tauri::command]
async fn open_login_item_settings() -> Result<(), String> {
    tokio::task::spawn_blocking(|| {
        platform::login_item::open_settings().map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "Could not open Login Items. Open System Settings and try again.".to_owned())?
}

#[tauri::command]
fn save_settings(
    app: tauri::AppHandle,
    settings: settings::Settings,
) -> Result<runtime::AppState, String> {
    let view = app.state::<runtime::Runtime>().save(settings)?;
    runtime::publish(&app);
    Ok(view)
}

#[tauri::command]
fn hide_popover(app: tauri::AppHandle) -> Result<(), String> {
    platform::hide_popover(&app).map_err(|_| "Could not hide the usage panel.".to_owned())
}

#[tauri::command]
fn resize_popover(
    app: tauri::AppHandle,
    width: f64,
    height: f64,
    ready: bool,
) -> Result<(), String> {
    platform::resize_popover(&app, width, height, ready)
        .map_err(|_| "Could not resize the usage panel.".to_owned())
}

#[tauri::command]
fn get_panel_preferences(app: tauri::AppHandle) -> Result<platform::PanelPreferencesView, String> {
    platform::panel_preferences(&app)
}

#[tauri::command]
async fn save_panel_preferences(
    app: tauri::AppHandle,
    preferences: panel_state::PanelPreferences,
) -> Result<panel_state::PanelPreferences, String> {
    platform::save_panel_preferences(&app, preferences).await
}

#[tauri::command]
fn drag_popover(app: tauri::AppHandle) -> Result<(), String> {
    platform::drag_popover(&app).map_err(|_| "Could not move the usage panel.".to_owned())
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    runtime::request_quit(&app);
}

fn main() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_nspanel::init())
        .invoke_handler(tauri::generate_handler![
            get_state,
            refresh_usage,
            reconnect_provider,
            cancel_reconnect,
            set_provider_enabled,
            open_setup_instructions,
            get_login_item_state,
            set_launch_at_login,
            dismiss_launch_at_login_prompt,
            open_login_item_settings,
            save_settings,
            hide_popover,
            resize_popover,
            get_panel_preferences,
            save_panel_preferences,
            drag_popover,
            quit_app
        ])
        .setup(|app| {
            platform::configure(app)?;
            app.manage(runtime::Runtime::new()?);
            tray::install(app)?;
            runtime::start(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
}
