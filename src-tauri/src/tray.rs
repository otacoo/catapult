// ── Notification area (system tray) ─────────────────────────────────────────
//
// Close-to-tray hides the window; the icon exists only while enabled.

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

pub const TRAY_ID: &str = "catapult-tray";

pub fn create_tray(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let show = MenuItem::with_id(app, "show", "Show Catapult", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Catapult")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Sync tray icon presence with `close_to_tray`.
pub fn sync_tray(app: &AppHandle) -> tauri::Result<()> {
    let enabled = app
        .state::<crate::AppState>()
        .config
        .lock()
        .unwrap()
        .close_to_tray;
    match (enabled, app.tray_by_id(TRAY_ID)) {
        (false, None) => Ok(()),
        (false, Some(tray)) => tray.set_visible(false),
        (true, Some(tray)) => tray.set_visible(true),
        (true, None) => {
            let tray = create_tray(app)?;
            tray.set_visible(true)
        }
    }
}
