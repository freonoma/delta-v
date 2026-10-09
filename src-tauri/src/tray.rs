use std::sync::OnceLock;

use tauri::{
    AppHandle,
    image::Image,
    menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

const QUIT_ID: &str = "quit";
const SETTINGS_ID: &str = "settings";

pub fn install(app: &tauri::App) -> tauri::Result<()> {
    install_app_menu(app)?;
    app.on_menu_event(|app, event| match event.id().as_ref() {
        QUIT_ID => {
            if let Err(error) = crate::settings_window::request_quit(app) {
                eprintln!("Could not quit Delta-V: {error}");
            }
        }
        SETTINGS_ID => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = crate::settings_window::open(&app, None, false).await {
                    eprintln!("{error}");
                }
            });
        }
        _ => {}
    });
    let settings = MenuItem::with_id(app, SETTINGS_ID, "Settings...", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Quit Delta-V", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&settings, &PredefinedMenuItem::separator(app)?, &quit],
    )?;

    TrayIconBuilder::with_id("main")
        .icon(mark())
        .icon_as_template(true)
        .title("?")
        .tooltip("Delta-V: waiting for usage data")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();
            crate::platform::on_tray_event(app, &event);
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) && let Err(error) = crate::platform::toggle_popover(app)
            {
                eprintln!("Could not open the usage panel: {error}");
            }
        })
        .build(app)?;
    crate::platform::style_tray(app.handle())
}

fn install_app_menu(app: &tauri::App) -> tauri::Result<()> {
    let app_menu = Submenu::with_items(
        app,
        "Delta-V",
        true,
        &[
            &PredefinedMenuItem::about(
                app,
                Some("About Delta-V"),
                Some(AboutMetadata {
                    name: Some("Delta-V".to_owned()),
                    version: Some(app.package_info().version.to_string()),
                    ..Default::default()
                }),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, SETTINGS_ID, "Settings...", true, Some("CmdOrCtrl+,"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, QUIT_ID, "Quit Delta-V", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    let file_menu = Submenu::with_items(
        app,
        "File",
        true,
        &[&PredefinedMenuItem::close_window(app, None)?],
    )?;
    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window_menu = Submenu::with_id_and_items(
        app,
        WINDOW_SUBMENU_ID,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
        ],
    )?;
    app.set_menu(Menu::with_items(
        app,
        &[&app_menu, &file_menu, &edit_menu, &window_menu],
    )?)?;
    Ok(())
}

pub fn update(
    app: &AppHandle,
    percentage: Option<f64>,
    stale: bool,
    tooltip: &str,
) -> tauri::Result<()> {
    let Some(tray) = app.tray_by_id("main") else {
        return Ok(());
    };
    let title = match percentage.filter(|value| value.is_finite()) {
        Some(value) if stale => format!("{:.0}%?", value.clamp(0.0, 100.0)),
        Some(value) => format!("{:.0}%", value.clamp(0.0, 100.0)),
        None => "?".to_owned(),
    };
    tray.set_title(Some(title))?;
    tray.set_tooltip(Some(tooltip))
}

fn mark() -> Image<'static> {
    static MARK: OnceLock<Image<'static>> = OnceLock::new();
    MARK.get_or_init(render_mark).clone()
}

fn render_mark() -> Image<'static> {
    const WIDTH: u32 = 120;
    const HEIGHT: u32 = 72;
    const SAMPLE_GRID: u32 = 4;
    let mut pixels = vec![0; (WIDTH * HEIGHT * 4) as usize];
    let delta = [(0.8, 15.8), (7.8, 2.2), (14.8, 15.8)];
    let cutout = [(5.4, 12.8), (7.8, 7.7), (10.2, 12.8)];
    let velocity = [
        (13.8, 2.2),
        (17.2, 2.2),
        (21.4, 11.4),
        (25.6, 2.2),
        (29.0, 2.2),
        (22.9, 15.8),
        (19.9, 15.8),
    ];

    // Supersampling keeps the angular mark crisp at Retina and 1x menu bar sizes.
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let mut coverage = 0;
            for sy in 0..SAMPLE_GRID {
                for sx in 0..SAMPLE_GRID {
                    let px = (f64::from(x) + (f64::from(sx) + 0.5) / 4.0) / 4.0;
                    let py = (f64::from(y) + (f64::from(sy) + 0.5) / 4.0) / 4.0;
                    if (inside(px, py, &delta) && !inside(px, py, &cutout))
                        || inside(px, py, &velocity)
                    {
                        coverage += 1;
                    }
                }
            }
            let offset = ((y * WIDTH + x) * 4) as usize;
            pixels[offset + 3] = (coverage * 255 / (SAMPLE_GRID * SAMPLE_GRID)) as u8;
        }
    }
    Image::new_owned(pixels, WIDTH, HEIGHT)
}

fn inside(x: f64, y: f64, polygon: &[(f64, f64)]) -> bool {
    let mut included = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        if (current.1 > y) != (previous.1 > y)
            && x < (previous.0 - current.0) * (y - current.1) / (previous.1 - current.1) + current.0
        {
            included = !included;
        }
        previous = current;
    }
    included
}
