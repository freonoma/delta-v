#[cfg(not(target_os = "macos"))]
compile_error!("Delta-V currently supports macOS only.");

pub mod auth;
mod claude_auth;
pub mod login_item;

use std::{
    ffi::c_void,
    io::Read,
    process::{Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use tauri::{AppHandle, Emitter, Manager, tray::TrayIconEvent};
use tauri_nspanel::{
    CollectionBehavior, ManagerExt, StyleMask, WebviewWindowExt,
    objc2_app_kit::{NSFont, NSScreen, NSWorkspace},
    objc2_foundation::{NSNumber, NSString, NSURL},
    tauri_panel,
};
use tauri_plugin_positioner::{Position, WindowExt};

use crate::{
    model::ProviderId,
    panel_state::{self, PanelPosition, PanelPreferences, PanelState},
};

const WINDOW: &str = "main";
const TRAY: &str = "main";
static DISPLAY_APP: OnceLock<AppHandle> = OnceLock::new();

tauri_panel! {
    panel!(UsagePanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
}

struct PopoverState {
    pinned: AtomicBool,
    restore_pending: AtomicBool,
    dragging: AtomicBool,
    screen_change_pending: AtomicBool,
    saved: Mutex<PanelRecord>,
    last_blur: Mutex<Option<Instant>>,
    preferred_size: Mutex<(f64, f64)>,
}

struct PanelRecord {
    state: PanelState,
    error: Option<String>,
}

#[derive(serde::Serialize)]
pub struct PanelPreferencesView {
    preferences: PanelPreferences,
    error: Option<String>,
}

impl PopoverState {
    fn load() -> Self {
        let (state, error) = match panel_state::load() {
            Ok(state) => (state, None),
            Err(error) => (PanelState::default(), Some(error.to_string())),
        };
        Self {
            pinned: AtomicBool::new(state.preferences.pinned),
            restore_pending: AtomicBool::new(state.preferences.pinned),
            dragging: AtomicBool::new(false),
            screen_change_pending: AtomicBool::new(false),
            saved: Mutex::new(PanelRecord { state, error }),
            last_blur: Mutex::new(None),
            preferred_size: Mutex::new((560.0, 360.0)),
        }
    }
}

pub struct Activity {
    pub idle_seconds: Option<f64>,
    pub locked: Option<bool>,
}

pub fn configure(app: &mut tauri::App) -> tauri::Result<()> {
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    app.manage(PopoverState::load());
    let window = app
        .get_webview_window(WINDOW)
        .ok_or(tauri::Error::WindowNotFound)?;
    let panel = window.to_panel::<UsagePanel>()?;
    panel.set_style_mask(StyleMask::empty().nonactivating_panel().value());
    panel.set_collection_behavior(
        CollectionBehavior::new()
            .can_join_all_spaces()
            .full_screen_auxiliary()
            .ignores_cycle()
            .value(),
    );
    panel.set_level(if popover_pinned(app.handle()) {
        tauri_nspanel::PanelLevel::Floating.value()
    } else {
        tauri_nspanel::PanelLevel::Status.value()
    });
    // The panel belongs to an inactive accessory app; blur handles dismissal.
    panel.set_hides_on_deactivate(false);
    panel.set_has_shadow(true);
    panel.set_corner_radius(16.0);
    panel.set_transparent(true);
    panel.hide();

    let _ = DISPLAY_APP.set(app.handle().clone());
    // CoreGraphics can notify once per affected screen. Query AppKit after the change settles.
    let result = unsafe {
        CGDisplayRegisterReconfigurationCallback(Some(displays_changed), std::ptr::null_mut())
    };
    if result != 0 {
        return Err(tauri::Error::Io(std::io::Error::other(
            "Could not observe changes to connected displays",
        )));
    }

    let handle = app.handle().clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Focused(false)) {
            if popover_pinned(&handle) {
                return;
            }
            if let Ok(mut last_blur) = handle.state::<PopoverState>().last_blur.lock() {
                *last_blur = Some(Instant::now());
            }
            if let Err(error) = hide_popover(&handle) {
                eprintln!("Could not hide the usage panel: {error}");
            }
        }
    });
    Ok(())
}

pub fn on_tray_event(app: &AppHandle, event: &TrayIconEvent) {
    tauri_plugin_positioner::on_tray_event(app, event);
}

pub fn toggle_popover(app: &AppHandle) -> tauri::Result<()> {
    app.state::<PopoverState>()
        .restore_pending
        .store(false, Ordering::Relaxed);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let Ok(panel) = handle.get_webview_panel(WINDOW) else {
            eprintln!("Could not find the usage panel");
            return;
        };
        if panel.is_visible() {
            if let Err(error) = hide_popover(&handle) {
                eprintln!("Could not hide the usage panel: {error}");
            }
            return;
        }

        // A tray click can resign the panel before its mouse-up event arrives.
        let just_blurred = handle
            .state::<PopoverState>()
            .last_blur
            .lock()
            .ok()
            .and_then(|blur| *blur)
            .is_some_and(|blur| blur.elapsed() < Duration::from_millis(200));
        if !popover_pinned(&handle) && just_blurred {
            return;
        }
        if let Err(error) = position_popover(&handle, popover_pinned(&handle)) {
            eprintln!("Could not position the usage panel: {error}");
        }
        if let Err(error) = handle.emit("popover-reset", ()) {
            eprintln!("Could not prepare the usage panel: {error}");
        }
        panel.show_and_make_key();
    })
}

pub fn hide_popover(app: &AppHandle) -> tauri::Result<()> {
    app.state::<PopoverState>()
        .restore_pending
        .store(false, Ordering::Relaxed);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if let Ok(panel) = handle.get_webview_panel(WINDOW) {
            panel.hide();
        }
        if let Err(error) = handle.emit("popover-reset", ()) {
            eprintln!("Could not prepare the usage panel: {error}");
        }
    })
}

pub fn popover_pinned(app: &AppHandle) -> bool {
    app.state::<PopoverState>().pinned.load(Ordering::Relaxed)
}

pub fn panel_preferences(app: &AppHandle) -> Result<PanelPreferencesView, String> {
    let state = app.state::<PopoverState>();
    let saved = state
        .saved
        .lock()
        .map_err(|_| "Could not read panel preferences.")?;
    Ok(PanelPreferencesView {
        preferences: saved.state.preferences,
        error: saved.error.clone(),
    })
}

pub async fn save_panel_preferences(
    app: &AppHandle,
    mut preferences: PanelPreferences,
) -> Result<PanelPreferences, String> {
    if !preferences.pinned {
        preferences.mini = false;
        preferences.expanded = false;
    }
    let handle = app.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let result = (|| {
            let panel = handle
                .get_webview_panel(WINDOW)
                .map_err(|_| "Could not find the usage panel.")?;
            let state = handle.state::<PopoverState>();
            let was_pinned = popover_pinned(&handle);
            let mut candidate = state
                .saved
                .lock()
                .map_err(|_| "Could not save panel preferences.")?
                .state
                .clone();
            candidate.preferences = preferences;
            if preferences.pinned && !was_pinned {
                candidate.position = current_panel_position(&handle);
            }
            if !preferences.pinned && was_pinned {
                position_popover(&handle, false)
                    .map_err(|_| "Could not return the panel to the menu bar.")?;
            }
            if let Err(error) = panel_state::save(&candidate) {
                if was_pinned {
                    let _ = position_popover(&handle, true);
                }
                return Err(error.to_string());
            }
            {
                let mut saved = state
                    .saved
                    .lock()
                    .map_err(|_| "Could not save panel preferences.")?;
                saved.state = candidate;
                saved.error = None;
            }
            panel.set_level(if preferences.pinned {
                tauri_nspanel::PanelLevel::Floating.value()
            } else {
                tauri_nspanel::PanelLevel::Status.value()
            });
            state.pinned.store(preferences.pinned, Ordering::Relaxed);
            state.restore_pending.store(false, Ordering::Relaxed);
            if let Ok(mut last_blur) = state.last_blur.lock() {
                *last_blur = None;
            }
            Ok(preferences)
        })();
        let _ = sender.send(result);
    })
    .map_err(|_| "Could not save panel preferences.")?;
    receiver
        .await
        .map_err(|_| "Could not save panel preferences.".to_owned())?
}

pub fn drag_popover(app: &AppHandle) -> tauri::Result<()> {
    if !popover_pinned(app) {
        return Ok(());
    }
    let state = app.state::<PopoverState>();
    if state.dragging.swap(true, Ordering::Relaxed) {
        return Ok(());
    }
    let result = app
        .get_webview_window(WINDOW)
        .ok_or(tauri::Error::WindowNotFound)
        .and_then(|window| window.start_dragging());
    if result.is_err() {
        state.dragging.store(false, Ordering::Relaxed);
        return result;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let app = handle.clone();
            if handle
                .run_on_main_thread(move || {
                    // AppKit's drag call returns before release and may consume mouse-up.
                    let released = NSEvent::pressedMouseButtons() & 1 == 0;
                    if released {
                        app.state::<PopoverState>()
                            .dragging
                            .store(false, Ordering::Relaxed);
                        if let Err(error) = remember_panel_position(&app) {
                            report_panel_error(&app, error);
                        }
                        if let Err(error) = position_popover(&app, popover_pinned(&app)) {
                            report_panel_error(&app, error.to_string());
                        }
                    }
                    let _ = sender.send(released);
                })
                .is_err()
            {
                break;
            }
            if !matches!(receiver.await, Ok(false)) {
                break;
            }
        }
    });
    Ok(())
}

pub fn resize_popover(app: &AppHandle, width: f64, height: f64, ready: bool) -> tauri::Result<()> {
    if !width.is_finite() || !height.is_finite() {
        return Err(tauri::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Panel dimensions must be finite",
        )));
    }
    {
        let state = app.state::<PopoverState>();
        let mut size = state.preferred_size.lock().map_err(|_| {
            tauri::Error::Io(std::io::Error::other("Could not resize the usage panel"))
        })?;
        *size = (width.clamp(220.0, 800.0), height.clamp(100.0, 620.0));
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if let Err(error) = position_popover(&handle, popover_pinned(&handle)) {
            eprintln!("Could not position the usage panel: {error}");
            return;
        }
        if ready
            && handle
                .state::<PopoverState>()
                .restore_pending
                .swap(false, Ordering::Relaxed)
            && popover_pinned(&handle)
            && let Ok(panel) = handle.get_webview_panel(WINDOW)
        {
            panel.show();
        }
    })
}

fn report_panel_error(app: &AppHandle, error: String) {
    if let Ok(mut saved) = app.state::<PopoverState>().saved.lock() {
        saved.error = Some(error.clone());
    }
    eprintln!("{error}");
    let _ = app.emit("panel-state-error", error);
}

fn display_id(screen: &NSScreen) -> Option<u32> {
    let description = screen.deviceDescription();
    let value = description.objectForKey(&NSString::from_str("NSScreenNumber"))?;
    Some(value.downcast_ref::<NSNumber>()?.unsignedIntValue())
}

fn position_on_display(frame: NSRect, available: NSRect, display_id: u32) -> PanelPosition {
    PanelPosition {
        display_id,
        x: (frame.origin.x - available.origin.x).max(0.0),
        top: (available.origin.y + available.size.height - frame.origin.y - frame.size.height)
            .max(0.0),
    }
}

fn current_panel_position(app: &AppHandle) -> Option<PanelPosition> {
    let panel = app.get_webview_panel(WINDOW).ok()?;
    let screen = panel.as_panel().screen()?;
    Some(position_on_display(
        panel.as_panel().frame(),
        screen.visibleFrame(),
        display_id(&screen)?,
    ))
}

fn remember_panel_position(app: &AppHandle) -> Result<(), String> {
    if !popover_pinned(app) {
        return Ok(());
    }
    let position = current_panel_position(app).ok_or("Could not read the panel position.")?;
    let state = app.state::<PopoverState>();
    let mut saved = state
        .saved
        .lock()
        .map_err(|_| "Could not save the panel position.")?;
    if saved.state.position == Some(position) {
        return Ok(());
    }
    let mut candidate = saved.state.clone();
    candidate.position = Some(position);
    saved.state = candidate;
    panel_state::save(&saved.state).map_err(|error| error.to_string())?;
    saved.error = None;
    Ok(())
}

pub async fn finish_panel_drag(app: &AppHandle) {
    let handle = app.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    if app
        .run_on_main_thread(move || {
            if handle
                .state::<PopoverState>()
                .dragging
                .swap(false, Ordering::Relaxed)
                && let Err(error) = remember_panel_position(&handle)
            {
                report_panel_error(&handle, error);
            }
            let _ = sender.send(());
        })
        .is_ok()
    {
        let _ = receiver.await;
    }
}

unsafe extern "C" fn displays_changed(_: u32, flags: u32, _: *mut c_void) {
    if flags & 1 != 0 {
        return;
    }
    let Some(app) = DISPLAY_APP.get() else {
        return;
    };
    let state = app.state::<PopoverState>();
    if state.screen_change_pending.swap(true, Ordering::Relaxed) {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let app = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            app.state::<PopoverState>()
                .screen_change_pending
                .store(false, Ordering::Relaxed);
            if popover_pinned(&app)
                && let Err(error) = position_popover(&app, true)
            {
                report_panel_error(&app, error.to_string());
            }
        });
    });
}

fn restored_screen(ids: &[Option<u32>], preferred: u32) -> Option<usize> {
    ids.iter()
        .position(|id| *id == Some(preferred))
        .or_else(|| (!ids.is_empty()).then_some(0))
}

fn restored_frame(position: PanelPosition, available: NSRect, requested: NSSize) -> NSRect {
    let desired = NSRect::new(
        NSPoint::new(
            available.origin.x + position.x,
            available.origin.y + available.size.height - position.top - requested.height,
        ),
        requested,
    );
    floating_frame(desired, available, requested)
}

fn popover_frame(anchor: NSRect, available: NSRect, requested: NSSize) -> NSRect {
    let margin = 8.0;
    let bottom = available.origin.y + margin;
    let top = (anchor
        .origin
        .y
        .min(available.origin.y + available.size.height)
        - margin)
        .max(bottom + 1.0);
    let height = requested.height.min(top - bottom);
    let width = requested
        .width
        .min((available.size.width - 2.0 * margin).max(1.0));
    let min_x = available.origin.x + margin;
    let max_x = (available.origin.x + available.size.width - width - margin).max(min_x);
    let x = (anchor.origin.x + anchor.size.width / 2.0 - width / 2.0).clamp(min_x, max_x);
    NSRect::new(NSPoint::new(x, top - height), NSSize::new(width, height))
}

fn floating_frame(current: NSRect, available: NSRect, requested: NSSize) -> NSRect {
    let margin = 8.0;
    let width = requested
        .width
        .min((available.size.width - 2.0 * margin).max(1.0));
    let height = requested
        .height
        .min((available.size.height - 2.0 * margin).max(1.0));
    let left = available.origin.x + margin;
    let bottom = available.origin.y + margin;
    let right = (available.origin.x + available.size.width - width - margin).max(left);
    let top = (available.origin.y + available.size.height - margin).max(bottom + height);
    // AppKit's origin is bottom-left. Keep the top edge still when content grows.
    let y = (current.origin.y + current.size.height).clamp(bottom + height, top) - height;
    NSRect::new(
        NSPoint::new(current.origin.x.clamp(left, right), y),
        NSSize::new(width, height),
    )
}

fn position_popover(app: &AppHandle, pinned: bool) -> tauri::Result<()> {
    if app.state::<PopoverState>().dragging.load(Ordering::Relaxed) {
        return Ok(());
    }
    let window = app
        .get_webview_window(WINDOW)
        .ok_or(tauri::Error::WindowNotFound)?;
    let panel = app
        .get_webview_panel(WINDOW)
        .map_err(|_| tauri::Error::WindowNotFound)?;
    let requested = {
        let state = app.state::<PopoverState>();
        let size = state.preferred_size.lock().map_err(|_| {
            tauri::Error::Io(std::io::Error::other("Could not read the panel size"))
        })?;
        NSSize::new(size.0, size.1)
    };
    if pinned {
        let position = app
            .state::<PopoverState>()
            .saved
            .lock()
            .map_err(|_| {
                tauri::Error::Io(std::io::Error::other("Could not read the panel position"))
            })?
            .state
            .position;
        if let Some(position) = position
            && let Some(mtm) = MainThreadMarker::new()
        {
            let screens = NSScreen::screens(mtm);
            let ids: Vec<_> = screens.iter().map(|screen| display_id(&screen)).collect();
            if let Some(index) = restored_screen(&ids, position.display_id) {
                let screen = screens.objectAtIndex(index);
                panel.as_panel().setFrame_display(
                    restored_frame(position, screen.visibleFrame(), requested),
                    true,
                );
                return Ok(());
            }
        }
    }
    let positioned = if let Some(tray) = app.tray_by_id(TRAY) {
        let panel = panel.clone();
        tray.with_inner_tray_icon(move |tray| {
            let Some(mtm) = tauri_nspanel::objc2_foundation::MainThreadMarker::new() else {
                return false;
            };
            let Some(item) = tray.ns_status_item() else {
                return false;
            };
            let Some(button) = item.button(mtm) else {
                return false;
            };
            let Some(tray_window) = button.window() else {
                return false;
            };
            let Some(screen) = tray_window.screen() else {
                return false;
            };

            // A status item's parent window can extend beyond the icon itself.
            let in_window = button.convertRect_toView(button.bounds(), None);
            let anchor = tray_window.convertRectToScreen(in_window);
            let frame = popover_frame(anchor, screen.visibleFrame(), requested);
            panel.as_panel().setFrame_display(frame, true);
            true
        })?
    } else {
        false
    };
    if positioned {
        return Ok(());
    }

    let fallback = window.move_window_constrained(Position::TrayCenter);
    if let Some(screen) = panel.as_panel().screen() {
        let available = screen.visibleFrame();
        let current = panel.as_panel().frame();
        let anchor = NSRect::new(
            NSPoint::new(
                current.origin.x + current.size.width / 2.0,
                available.origin.y + available.size.height,
            ),
            NSSize::new(0.0, 0.0),
        );
        panel
            .as_panel()
            .setFrame_display(popover_frame(anchor, available, requested), true);
        fallback
    } else {
        fallback
    }
}

pub fn style_tray(app: &AppHandle) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id(TRAY) {
        tray.with_inner_tray_icon(|tray| {
            if let Some(mtm) = tauri_nspanel::objc2_foundation::MainThreadMarker::new()
                && let Some(item) = tray.ns_status_item()
                && let Some(button) = item.button(mtm)
            {
                button.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
                    12.0, 0.23,
                )));
            }
        })?;
    }
    Ok(())
}

pub fn normalize_nfc(value: &str) -> String {
    tauri_nspanel::objc2::rc::autoreleasepool(|_| {
        NSString::from_str(value)
            .precomposedStringWithCanonicalMapping()
            .to_string()
    })
}

pub fn open_setup_instructions(provider: ProviderId) -> Result<(), String> {
    let address = match provider {
        ProviderId::Claude => "https://code.claude.com/docs/en/quickstart",
        ProviderId::Codex => "https://learn.chatgpt.com/docs/codex/cli",
    };
    tauri_nspanel::objc2::rc::autoreleasepool(|_| {
        let url = NSURL::URLWithString(&NSString::from_str(address))
            .ok_or_else(|| "Could not open the setup instructions.".to_owned())?;
        if NSWorkspace::sharedWorkspace().openURL(&url) {
            Ok(())
        } else {
            Err("Could not open your browser. Please try again.".to_owned())
        }
    })
}

pub fn read_keychain(service: &str, account: &str) -> Result<String, String> {
    let mut child = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Could not open macOS Keychain".to_owned())?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Could not read the Keychain response".to_owned());
    };
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(65_537).read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(30) => {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Keychain access timed out. Allow access and try again.".to_owned());
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Could not finish reading macOS Keychain".to_owned());
            }
        }
    };
    if status.code() == Some(44) {
        return Err("No CLI sign-in was found in macOS Keychain".to_owned());
    }
    if !status.success() {
        return Err("The CLI sign-in in macOS Keychain is not accessible".to_owned());
    }
    let bytes = reader
        .join()
        .map_err(|_| "Could not read the Keychain response".to_owned())?
        .map_err(|_| "Could not read the Keychain response".to_owned())?;
    if bytes.len() > 65_536 {
        return Err("The Keychain response was too large".to_owned());
    }
    let value =
        String::from_utf8(bytes).map_err(|_| "The Keychain response was not text".to_owned())?;
    Ok(value.trim_end_matches(['\r', '\n']).to_owned())
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayRegisterReconfigurationCallback(
        callback: Option<unsafe extern "C" fn(u32, u32, *mut c_void)>,
        user_info: *mut c_void,
    ) -> i32;
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    fn CGSessionCopyCurrentDictionary() -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: *const c_void) -> bool;
    fn CFRelease(value: *const c_void);
}

pub fn activity() -> Activity {
    // Only the elapsed time is read, never the content of input events.
    let idle = unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) };
    let idle_seconds = (idle.is_finite() && idle >= 0.0).then_some(idle);
    let session = unsafe { CGSessionCopyCurrentDictionary() };
    let locked = if session.is_null() {
        None
    } else {
        let screen_locked = session_flag(session, "CGSSessionScreenIsLocked");
        let on_console = session_flag(session, "kCGSSessionOnConsoleKey");
        let logged_in = session_flag(session, "kCGSessionLoginDoneKey");
        // The lock key is absent in an unlocked session on current macOS.
        let locked = match (screen_locked, on_console, logged_in) {
            (Some(true), _, _) | (_, Some(false), _) | (_, _, Some(false)) => Some(true),
            (_, Some(true), Some(true)) => Some(false),
            _ => None,
        };
        unsafe { CFRelease(session) };
        locked
    };
    Activity {
        idle_seconds,
        locked,
    }
}

fn session_flag(session: *const c_void, name: &str) -> Option<bool> {
    let key = NSString::from_str(name);
    // NSString and CFString are toll-free bridged; the dictionary owns its value.
    unsafe {
        let value = CFDictionaryGetValue(session, (&*key as *const NSString).cast());
        if value.is_null() || CFGetTypeID(value) != CFBooleanGetTypeID() {
            None
        } else {
            Some(CFBooleanGetValue(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    #[test]
    fn panel_stays_below_the_icon_and_menu_bar() {
        let available = rect(0.0, 48.0, 1440.0, 826.0);
        let frame = popover_frame(
            rect(1100.0, 874.0, 52.0, 26.0),
            available,
            NSSize::new(560.0, 360.0),
        );
        assert_eq!(frame, rect(846.0, 506.0, 560.0, 360.0));
        let oversized_anchor =
            popover_frame(rect(1100.0, 930.0, 52.0, 26.0), available, frame.size);
        assert_eq!(oversized_anchor, frame);
    }

    #[test]
    fn panel_respects_a_display_left_of_the_primary_screen() {
        let frame = popover_frame(
            rect(-180.0, 796.0, 52.0, 24.0),
            rect(-1280.0, -180.0, 1280.0, 976.0),
            NSSize::new(560.0, 620.0),
        );
        assert_eq!(frame, rect(-568.0, 168.0, 560.0, 620.0));
    }

    #[test]
    fn expansion_shrinks_to_the_available_height_instead_of_covering_the_menu() {
        let anchor = rect(900.0, 540.0, 52.0, 24.0);
        let available = rect(0.0, 24.0, 1024.0, 516.0);
        let compact = popover_frame(anchor, available, NSSize::new(560.0, 280.0));
        let expanded = popover_frame(anchor, available, NSSize::new(560.0, 620.0));
        assert_eq!(expanded, rect(456.0, 32.0, 560.0, 500.0));
        assert_eq!(
            compact.origin.y + compact.size.height,
            expanded.origin.y + expanded.size.height
        );
    }

    #[test]
    fn pinned_panel_keeps_its_top_left_when_content_changes() {
        let available = rect(0.0, 48.0, 1440.0, 826.0);
        let current = rect(420.0, 500.0, 340.0, 280.0);
        let expanded = floating_frame(current, available, NSSize::new(560.0, 620.0));
        assert_eq!(expanded, rect(420.0, 160.0, 560.0, 620.0));
        assert_eq!(floating_frame(expanded, available, current.size), current);
    }

    #[test]
    fn pinned_panel_moves_only_as_far_as_needed_to_fit() {
        let frame = floating_frame(
            rect(1000.0, 60.0, 340.0, 280.0),
            rect(0.0, 48.0, 1440.0, 826.0),
            NSSize::new(560.0, 620.0),
        );
        assert_eq!(frame, rect(872.0, 56.0, 560.0, 620.0));
    }

    #[test]
    fn pinned_panel_uses_the_current_display_with_negative_coordinates() {
        let frame = floating_frame(
            rect(-950.0, -80.0, 340.0, 280.0),
            rect(-1280.0, -300.0, 1280.0, 1000.0),
            NSSize::new(560.0, 420.0),
        );
        assert_eq!(frame, rect(-950.0, -220.0, 560.0, 420.0));
    }

    #[test]
    fn pinned_panel_fits_a_smaller_screen() {
        let frame = floating_frame(
            rect(900.0, 500.0, 560.0, 360.0),
            rect(0.0, 24.0, 500.0, 516.0),
            NSSize::new(560.0, 620.0),
        );
        assert_eq!(frame, rect(8.0, 32.0, 484.0, 500.0));
    }

    #[test]
    fn restores_the_saved_display_or_a_visible_fallback() {
        assert_eq!(restored_screen(&[Some(10), Some(20)], 20), Some(1));
        assert_eq!(restored_screen(&[Some(10)], 20), Some(0));
        assert_eq!(restored_screen(&[], 20), None);
        assert_eq!(restored_screen(&[None, Some(20)], 20), Some(1));
    }

    #[test]
    fn saved_position_follows_a_display_when_its_arrangement_changes() {
        let original = rect(-1280.0, -300.0, 1280.0, 1000.0);
        let frame = rect(-950.0, -80.0, 336.0, 150.0);
        let saved = position_on_display(frame, original, 20);
        assert_eq!(restored_frame(saved, original, frame.size), frame);
        let rearranged = rect(1440.0, 40.0, 1280.0, 1000.0);
        assert_eq!(
            restored_frame(saved, rearranged, frame.size),
            rect(1770.0, 260.0, 336.0, 150.0)
        );
    }

    #[test]
    fn expanding_at_an_edge_does_not_replace_the_saved_anchor() {
        let available = rect(0.0, 48.0, 1440.0, 826.0);
        let mini = rect(1080.0, 60.0, 336.0, 150.0);
        let saved = position_on_display(mini, available, 10);
        assert_eq!(
            restored_frame(saved, available, NSSize::new(560.0, 620.0)),
            rect(872.0, 56.0, 560.0, 620.0)
        );
        assert_eq!(restored_frame(saved, available, mini.size), mini);
    }

    #[test]
    fn restore_clamps_to_a_smaller_replacement_display() {
        let saved = PanelPosition {
            display_id: 20,
            x: 1800.0,
            top: 900.0,
        };
        assert_eq!(
            restored_frame(
                saved,
                rect(0.0, 24.0, 1024.0, 700.0),
                NSSize::new(560.0, 620.0)
            ),
            rect(456.0, 32.0, 560.0, 620.0)
        );
    }
}
