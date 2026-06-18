use tauri::{AppHandle, Emitter, State};

use crate::{
    refresh,
    state::{AppSettings, AppState},
    usage::ProviderSnapshot,
    window,
};

const API_KEY_FIELD: &str = "api_key";
#[tauri::command]
pub async fn get_reports(app: AppHandle) -> Result<Vec<ProviderSnapshot>, String> {
    Ok(refresh::get_cached_or_refresh(&app).await)
}

#[tauri::command]
pub async fn refresh_all(app: AppHandle) -> Result<Vec<ProviderSnapshot>, String> {
    Ok(refresh::refresh_enabled_providers(&app).await)
}

#[tauri::command]
pub async fn refresh_provider(
    provider: String,
    app: AppHandle,
) -> Result<ProviderSnapshot, String> {
    Ok(refresh::refresh_provider_report(&provider, &app).await)
}

#[tauri::command]
pub async fn set_popover_height(height: u32, app: AppHandle) -> Result<(), String> {
    window::set_popover_height(&app, height).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn open_settings_window(app: AppHandle) -> Result<(), String> {
    window::open_settings_window(&app);
    Ok(())
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    Ok(state.settings().await)
}

#[tauri::command]
pub async fn update_settings(
    settings: AppSettings,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    let saved = state
        .update_settings(settings)
        .await
        .map_err(|error| error.to_string())?;
    app.emit("settings://updated", &saved)
        .map_err(|error| error.to_string())?;
    Ok(saved)
}

#[tauri::command]
pub async fn set_provider_secret(
    provider: String,
    field: String,
    value: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .set_provider_secret(&provider, &field, value.as_deref())
        .map_err(|error| error.to_string())?;
    if state.settings().await.is_enabled(&provider) {
        refresh::refresh_provider_report_forced(&provider, &app).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_provider_secret_status(
    provider: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    state
        .provider_secret_status(&provider, API_KEY_FIELD)
        .map_err(|error| error.to_string())
}
