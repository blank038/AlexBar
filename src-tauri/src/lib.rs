mod commands;
mod credentials;
mod providers;
mod refresh;
mod state;
mod tray;
mod tray_icon;
mod usage;
mod window;

use state::AppState;
use tauri::Manager;

fn autostart_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let builder = tauri_plugin_autostart::Builder::new();
    #[cfg(target_os = "macos")]
    let builder = builder.macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent);
    builder.build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(autostart_plugin())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let state = AppState::load(app)?;
            app.manage(state);
            if let Some(main_window) = app.get_webview_window("main") {
                window::apply_platform_chrome(&main_window);
            }
            tray::setup_tray(app)?;
            refresh::spawn_refresh_loop(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| match window.label() {
            "settings" => {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            "main" if matches!(event, tauri::WindowEvent::Focused(false)) => {
                crate::window::note_popover_hidden_by_focus_loss();
                let _ = window.hide();
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_reports,
            commands::refresh_all,
            commands::refresh_provider,
            commands::get_settings,
            commands::update_settings,
            commands::set_provider_secret,
            commands::get_provider_secret_status,
            commands::set_popover_height,
            commands::open_settings_window,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run AlexBar");
}
