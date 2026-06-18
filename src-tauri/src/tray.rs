use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App,
};

use crate::{refresh, tray_icon, window};

pub fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let refresh_item = MenuItem::with_id(app, "refresh", "Refresh", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&refresh_item, &settings_item, &quit_item])?;
    let icon = tray_icon::build_meter_icon(None, None);

    TrayIconBuilder::with_id(tray_icon::tray_id())
        .icon(icon)
        .tooltip("AlexBar")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "refresh" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = refresh::refresh_enabled_providers(&app).await;
                });
            }
            "settings" => window::open_settings_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                window::toggle_popover(tray.app_handle(), Some(position));
            }
        })
        .build(app)?;

    Ok(())
}
