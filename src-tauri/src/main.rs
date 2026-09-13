mod credentials;
mod model;
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
fn save_settings(
    app: tauri::AppHandle,
    settings: settings::Settings,
) -> Result<runtime::AppState, String> {
    let view = app.state::<runtime::Runtime>().save(settings)?;
    runtime::publish(&app, &view);
    Ok(view)
}

#[tauri::command]
fn hide_popover(app: tauri::AppHandle) -> Result<(), String> {
    platform::hide_popover(&app).map_err(|_| "Could not hide the usage panel.".to_owned())
}

#[tauri::command]
fn resize_popover(app: tauri::AppHandle, width: f64, height: f64) -> Result<(), String> {
    platform::resize_popover(&app, width, height)
        .map_err(|_| "Could not resize the usage panel.".to_owned())
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

fn main() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_nspanel::init())
        .invoke_handler(tauri::generate_handler![
            get_state,
            refresh_usage,
            save_settings,
            hide_popover,
            resize_popover,
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
