use std::{sync::LazyLock, time::Duration};

use tauri::{AppHandle, Emitter, Manager};

use crate::{
    credentials::{claude_file::REFRESH_SKEW_MS, source_for_provider},
    providers,
    state::{AppSettings, AppState},
    tray_icon,
    usage::{failure_snapshot, now_millis, ProviderSnapshot, SourceError},
};

static REFRESH_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

pub fn spawn_refresh_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let _ = refresh_enabled_providers(&app).await;
        loop {
            let interval_secs = app
                .state::<AppState>()
                .settings()
                .await
                .refresh_interval_secs;
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            let _ = refresh_enabled_providers(&app).await;
        }
    });
}

pub async fn get_cached_or_refresh(app: &AppHandle) -> Vec<ProviderSnapshot> {
    let state = app.state::<AppState>();
    if let Some(snapshots) = state.cached_snapshots(now_millis()).await {
        return snapshots;
    }

    let _guard = REFRESH_LOCK.lock().await;
    if let Some(snapshots) = state.cached_snapshots(now_millis()).await {
        return snapshots;
    }
    refresh_enabled_providers_unlocked(app).await
}

pub async fn refresh_enabled_providers(app: &AppHandle) -> Vec<ProviderSnapshot> {
    let state = app.state::<AppState>();
    let _guard = REFRESH_LOCK.lock().await;
    if let Some(snapshots) = fresh_snapshots_for_current_settings(&state).await {
        return snapshots;
    }

    refresh_enabled_providers_unlocked(app).await
}

async fn refresh_enabled_providers_unlocked(app: &AppHandle) -> Vec<ProviderSnapshot> {
    let state = app.state::<AppState>();
    let settings = state.settings().await;
    let mut snapshots = Vec::with_capacity(settings.enabled_providers.len());

    for provider in settings.ordered_enabled_providers() {
        snapshots.push(refresh_provider_snapshot_unlocked(provider, app).await);
    }

    let refreshed_at = now_millis();
    state
        .replace_snapshots(snapshots.clone(), refreshed_at)
        .await;
    publish_snapshots(app, &snapshots);
    snapshots
}

pub async fn refresh_provider_report(provider: &str, app: &AppHandle) -> ProviderSnapshot {
    let state = app.state::<AppState>();
    let _guard = REFRESH_LOCK.lock().await;
    if let Some(snapshot) = fresh_provider_snapshot(&state, provider).await {
        return snapshot;
    }

    refresh_provider_snapshot_unlocked(provider, app).await
}

pub async fn refresh_provider_report_forced(provider: &str, app: &AppHandle) -> ProviderSnapshot {
    let _guard = REFRESH_LOCK.lock().await;
    refresh_provider_snapshot_unlocked(provider, app).await
}

async fn refresh_provider_snapshot_unlocked(provider: &str, app: &AppHandle) -> ProviderSnapshot {
    let state = app.state::<AppState>();
    let snapshot = match fetch_provider(provider, state.inner()).await {
        Ok(snapshot) => snapshot,
        Err(FetchError::Source(SourceError::RateLimited { .. })) => {
            if let Some(snapshot) = cached_successful_provider_snapshot(&state, provider).await {
                return snapshot;
            }
            transient_waiting_snapshot(provider)
        }
        Err(error) => failure_snapshot(provider, error.to_string()),
    };
    state.upsert_snapshot(snapshot.clone()).await;
    let snapshots = state.snapshots().await;
    publish_snapshots(app, &snapshots);
    snapshot
}

async fn fresh_snapshots_for_current_settings(state: &AppState) -> Option<Vec<ProviderSnapshot>> {
    let settings = state.settings().await;
    let snapshots = state.cached_snapshots(now_millis()).await?;
    if snapshots_match_current_settings(&settings, &snapshots) {
        Some(snapshots)
    } else {
        None
    }
}

fn snapshots_match_current_settings(
    settings: &AppSettings,
    snapshots: &[ProviderSnapshot],
) -> bool {
    let ordered_enabled = settings.ordered_enabled_providers().collect::<Vec<_>>();
    snapshots.len() == ordered_enabled.len()
        && ordered_enabled.iter().enumerate().all(|(index, provider)| {
            snapshots
                .get(index)
                .is_some_and(|snapshot| snapshot.provider == **provider)
        })
}

async fn fresh_provider_snapshot(state: &AppState, provider: &str) -> Option<ProviderSnapshot> {
    state
        .cached_snapshots(now_millis())
        .await?
        .into_iter()
        .find(|snapshot| snapshot.provider == provider)
}

async fn cached_successful_provider_snapshot(
    state: &AppState,
    provider: &str,
) -> Option<ProviderSnapshot> {
    state.snapshots().await.into_iter().find(|snapshot| {
        snapshot.provider == provider && snapshot.note.is_none() && !snapshot.metrics.is_empty()
    })
}

fn transient_waiting_snapshot(provider: &str) -> ProviderSnapshot {
    ProviderSnapshot {
        provider: provider.to_owned(),
        refreshed_at: now_millis(),
        account: None,
        metrics: Vec::new(),
        note: None,
    }
}

async fn fetch_provider(provider: &str, state: &AppState) -> Result<ProviderSnapshot, FetchError> {
    let descriptor = providers::find(provider)
        .ok_or_else(|| FetchError::Message(format!("unsupported provider {provider}")))?;
    let source = source_for_provider(provider)
        .ok_or_else(|| FetchError::Message(format!("unsupported provider {provider}")))?;
    debug_assert_eq!(source.provider(), provider);
    let mut credential = source
        .load(state)
        .await
        .map_err(|error| FetchError::Message(error.to_string()))?;
    if credential.needs_refresh_at(now_millis(), REFRESH_SKEW_MS) {
        match source.refresh(state.client(), &credential).await {
            Ok(Some(refreshed)) => credential = refreshed,
            Ok(None) => {}
            Err(error) => return Err(FetchError::Message(error.to_string())),
        }
    }
    let gate = state
        .quota_gate(provider)
        .ok_or_else(|| FetchError::Message(format!("missing quota gate for {provider}")))?;
    let report_source = (descriptor.report)(gate);
    debug_assert_eq!(report_source.provider(), provider);
    report_source
        .fetch(state.client(), &credential)
        .await
        .map_err(FetchError::Source)
}

enum FetchError {
    Source(SourceError),
    Message(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(error) => error.fmt(formatter),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

fn publish_snapshots(app: &AppHandle, snapshots: &[ProviderSnapshot]) {
    let _ = app.emit("usage://updated", snapshots);
    tray_icon::update_tray_icon(app, snapshots);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_snapshot_check_requires_saved_provider_order() {
        let settings = AppSettings {
            enabled_providers: vec!["openai-codex".to_owned(), "zai".to_owned()],
            provider_order: vec![
                "zai".to_owned(),
                "deepseek".to_owned(),
                "anthropic".to_owned(),
                "openai-codex".to_owned(),
            ],
            refresh_interval_secs: 60,
            visible_provider_limit: 2,
            locale: "zh-CN".to_owned(),
        };
        let ordered = vec![
            transient_waiting_snapshot("zai"),
            transient_waiting_snapshot("openai-codex"),
        ];
        let fixed = vec![
            transient_waiting_snapshot("openai-codex"),
            transient_waiting_snapshot("zai"),
        ];

        assert!(snapshots_match_current_settings(&settings, &ordered));
        assert!(!snapshots_match_current_settings(&settings, &fixed));
    }
}
