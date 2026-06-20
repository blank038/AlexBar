use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

use crate::{
    credentials::SecretStore,
    providers,
    usage::{ProviderSnapshot, RateLimitGate},
};

pub const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 60;
pub const CACHE_TTL_MS: i64 = 30_000;
const SETTINGS_KEY: &str = "settings";
const SETTINGS_STORE: &str = "settings.json";
const SECRETS_STORE: &str = "secrets.json";

type Store = tauri_plugin_store::Store<tauri::Wry>;

fn default_locale() -> String {
    "zh-CN".to_owned()
}

fn default_visible_provider_limit() -> u32 {
    2
}

fn default_provider_order() -> Vec<String> {
    providers::ids().map(ToOwned::to_owned).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub enabled_providers: Vec<String>,
    #[serde(default = "default_provider_order")]
    pub provider_order: Vec<String>,
    pub refresh_interval_secs: u64,
    #[serde(default = "default_visible_provider_limit")]
    pub visible_provider_limit: u32,
    #[serde(default = "default_locale")]
    pub locale: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            enabled_providers: providers::ids().map(ToOwned::to_owned).collect(),
            provider_order: default_provider_order(),
            refresh_interval_secs: DEFAULT_REFRESH_INTERVAL_SECS,
            visible_provider_limit: default_visible_provider_limit(),
            locale: default_locale(),
        }
    }
}

impl AppSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        for provider in &self.enabled_providers {
            if providers::find(provider).is_none() {
                return Err(SettingsError::InvalidProvider {
                    provider: provider.clone(),
                });
            }
        }

        let mut seen_order = HashSet::new();
        for provider in &self.provider_order {
            if providers::find(provider).is_none() {
                return Err(SettingsError::InvalidProviderOrder {
                    provider: provider.clone(),
                });
            }
            if !seen_order.insert(provider.as_str()) {
                return Err(SettingsError::DuplicateProviderOrder {
                    provider: provider.clone(),
                });
            }
        }

        match self.refresh_interval_secs {
            30 | 60 | 120 | 300 => {}
            value => return Err(SettingsError::InvalidRefreshInterval { value }),
        }

        match self.visible_provider_limit {
            1..=8 => {}
            value => return Err(SettingsError::InvalidVisibleProviderLimit { value }),
        }

        match self.locale.as_str() {
            "zh-CN" | "en-US" => Ok(()),
            value => Err(SettingsError::InvalidLocale {
                value: value.to_owned(),
            }),
        }
    }

    pub fn is_enabled(&self, provider: &str) -> bool {
        self.enabled_providers
            .iter()
            .any(|enabled| enabled == provider)
    }

    pub fn ordered_enabled_providers(&self) -> impl Iterator<Item = &str> {
        self.provider_order
            .iter()
            .map(String::as_str)
            .filter(|provider| self.is_enabled(provider))
    }

    fn normalize_provider_order(&mut self) {
        let ordered = self.provider_order.iter().cloned().collect::<HashSet<_>>();
        for provider in providers::ids() {
            if !ordered.contains(provider) {
                self.provider_order.push(provider.to_owned());
            }
        }
    }
}

pub struct AppState {
    snapshots: RwLock<Vec<ProviderSnapshot>>,
    settings: RwLock<AppSettings>,
    last_refresh_at: RwLock<Option<i64>>,
    quota_gates: HashMap<&'static str, Arc<RateLimitGate>>,
    store: Arc<Store>,
    secrets: Arc<Store>,
    client: reqwest::Client,
}

impl AppState {
    pub fn load(app: &tauri::App) -> Result<Self, StateError> {
        use tauri_plugin_store::StoreExt;

        let store = app.store(SETTINGS_STORE).map_err(SettingsError::Store)?;
        let settings = load_settings(&store)?;
        let secrets = app.store(SECRETS_STORE).map_err(SettingsError::Store)?;
        let client = reqwest::Client::builder()
            .https_only(true)
            .user_agent("AlexBar/0.1.0")
            .build()
            .map_err(StateError::HttpClient)?;
        let quota_gates = providers::ids()
            .map(|provider| (provider, Arc::new(RateLimitGate::default())))
            .collect();

        Ok(Self {
            snapshots: RwLock::new(Vec::new()),
            settings: RwLock::new(settings),
            last_refresh_at: RwLock::new(None),
            quota_gates,
            store,
            secrets,
            client,
        })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn quota_gate(&self, provider: &str) -> Option<Arc<RateLimitGate>> {
        self.quota_gates.get(provider).cloned()
    }

    pub async fn snapshots(&self) -> Vec<ProviderSnapshot> {
        self.snapshots.read().await.clone()
    }

    pub async fn replace_snapshots(&self, snapshots: Vec<ProviderSnapshot>, refreshed_at: i64) {
        *self.snapshots.write().await = snapshots;
        *self.last_refresh_at.write().await = Some(refreshed_at);
    }

    pub async fn upsert_snapshot(&self, snapshot: ProviderSnapshot) {
        let refreshed_at = snapshot.refreshed_at;
        let mut snapshots = self.snapshots.write().await;
        if let Some(existing) = snapshots
            .iter_mut()
            .find(|existing| existing.provider == snapshot.provider)
        {
            *existing = snapshot;
        } else {
            snapshots.push(snapshot);
        }
        *self.last_refresh_at.write().await = Some(refreshed_at);
    }

    pub async fn cached_snapshots(&self, now_ms: i64) -> Option<Vec<ProviderSnapshot>> {
        let last_refresh_at = *self.last_refresh_at.read().await;
        match last_refresh_at {
            Some(last_refresh_at) if now_ms - last_refresh_at <= CACHE_TTL_MS => {
                Some(self.snapshots().await)
            }
            _ => None,
        }
    }

    pub async fn settings(&self) -> AppSettings {
        self.settings.read().await.clone()
    }

    pub async fn update_settings(
        &self,
        mut settings: AppSettings,
    ) -> Result<AppSettings, SettingsError> {
        settings.normalize_provider_order();
        settings.validate()?;
        save_settings(&self.store, &settings)?;
        *self.settings.write().await = settings.clone();
        Ok(settings)
    }

    pub fn set_provider_secret(
        &self,
        provider: &str,
        field: &str,
        value: Option<&str>,
    ) -> Result<(), SettingsError> {
        validate_provider(provider)?;
        let key = secret_key(provider, field);
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => self.secrets.set(key, value.to_owned()),
            None => {
                self.secrets.delete(key);
            }
        }
        self.secrets.save().map_err(SettingsError::Store)
    }

    pub fn provider_secret_status(
        &self,
        provider: &str,
        field: &str,
    ) -> Result<bool, SettingsError> {
        validate_provider(provider)?;
        Ok(read_secret(&self.secrets, provider, field).is_some())
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error("failed to build HTTP client: {0}")]
    HttpClient(reqwest::Error),
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("settings contain unsupported provider {provider}")]
    InvalidProvider { provider: String },
    #[error("settings contain unsupported provider order entry {provider}")]
    InvalidProviderOrder { provider: String },
    #[error("settings contain duplicate provider order entry {provider}")]
    DuplicateProviderOrder { provider: String },
    #[error("settings contain unsupported refresh interval {value}")]
    InvalidRefreshInterval { value: u64 },
    #[error("settings contain unsupported visible provider limit {value}")]
    InvalidVisibleProviderLimit { value: u32 },
    #[error("settings contain unsupported locale {value}")]
    InvalidLocale { value: String },
    #[error("failed to use settings store: {0}")]
    Store(tauri_plugin_store::Error),
    #[error("failed to decode settings from store: {0}")]
    Decode(serde_json::Error),
    #[error("failed to encode settings for store: {0}")]
    Encode(serde_json::Error),
}

fn load_settings(store: &Store) -> Result<AppSettings, SettingsError> {
    let settings = match store.get(SETTINGS_KEY) {
        Some(value) => {
            let should_save_legacy_order = value.get("providerOrder").is_none();
            let mut settings =
                serde_json::from_value::<AppSettings>(value).map_err(SettingsError::Decode)?;
            let before_normalize = settings.clone();
            settings.normalize_provider_order();
            settings.validate()?;
            if should_save_legacy_order || settings != before_normalize {
                save_settings(store, &settings)?;
            }
            settings
        }
        None => {
            let settings = AppSettings::default();
            save_settings(store, &settings)?;
            settings
        }
    };
    Ok(settings)
}

fn save_settings(store: &Store, settings: &AppSettings) -> Result<(), SettingsError> {
    let value = serde_json::to_value(settings).map_err(SettingsError::Encode)?;
    store.set(SETTINGS_KEY, value);
    store.save().map_err(SettingsError::Store)
}

impl SecretStore for AppState {
    fn get_secret(&self, provider: &str, field: &str) -> Option<String> {
        read_secret(&self.secrets, provider, field)
    }
}

fn validate_provider(provider: &str) -> Result<(), SettingsError> {
    if providers::find(provider).is_some() {
        Ok(())
    } else {
        Err(SettingsError::InvalidProvider {
            provider: provider.to_owned(),
        })
    }
}

fn read_secret(store: &Store, provider: &str, field: &str) -> Option<String> {
    store
        .get(secret_key(provider, field))
        .and_then(|value| value.as_str().map(str::trim).map(ToOwned::to_owned))
        .filter(|value| !value.is_empty())
}

fn secret_key(provider: &str, field: &str) -> String {
    format!("{provider}.{field}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_supported_settings() {
        let settings = AppSettings {
            enabled_providers: vec![
                "openai-codex".to_owned(),
                "anthropic".to_owned(),
                "deepseek".to_owned(),
                "zai".to_owned(),
            ],
            provider_order: vec![
                "openai-codex".to_owned(),
                "anthropic".to_owned(),
                "deepseek".to_owned(),
                "zai".to_owned(),
            ],
            refresh_interval_secs: 30,
            visible_provider_limit: 2,
            locale: "zh-CN".to_owned(),
        };
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn rejects_unsupported_visible_provider_limit() {
        let settings = AppSettings {
            enabled_providers: vec!["openai-codex".to_owned()],
            provider_order: vec![
                "openai-codex".to_owned(),
                "anthropic".to_owned(),
                "deepseek".to_owned(),
                "zai".to_owned(),
            ],
            refresh_interval_secs: 60,
            visible_provider_limit: 0,
            locale: "zh-CN".to_owned(),
        };
        assert!(matches!(
            settings.validate(),
            Err(SettingsError::InvalidVisibleProviderLimit { value: 0 })
        ));
    }

    #[test]
    fn rejects_unsupported_provider() {
        let settings = AppSettings {
            enabled_providers: vec!["gemini".to_owned()],
            provider_order: vec![
                "openai-codex".to_owned(),
                "anthropic".to_owned(),
                "deepseek".to_owned(),
                "zai".to_owned(),
            ],
            refresh_interval_secs: 60,
            visible_provider_limit: 2,
            locale: "zh-CN".to_owned(),
        };
        assert!(matches!(
            settings.validate(),
            Err(SettingsError::InvalidProvider { .. })
        ));
    }

    #[test]
    fn defaults_provider_order_when_settings_are_missing_it() {
        let settings: AppSettings = serde_json::from_value(serde_json::json!({
            "enabledProviders": ["openai-codex", "anthropic"],
            "refreshIntervalSecs": 60,
            "visibleProviderLimit": 2,
            "locale": "zh-CN"
        }))
        .expect("legacy settings should decode");

        assert_eq!(
            settings.provider_order,
            vec![
                "openai-codex".to_owned(),
                "anthropic".to_owned(),
                "deepseek".to_owned(),
                "zai".to_owned(),
            ]
        );
    }

    #[test]
    fn rejects_duplicate_provider_order_entries() {
        let settings = AppSettings {
            enabled_providers: vec!["openai-codex".to_owned()],
            provider_order: vec![
                "openai-codex".to_owned(),
                "openai-codex".to_owned(),
                "deepseek".to_owned(),
                "zai".to_owned(),
            ],
            refresh_interval_secs: 60,
            visible_provider_limit: 2,
            locale: "zh-CN".to_owned(),
        };

        assert!(matches!(
            settings.validate(),
            Err(SettingsError::DuplicateProviderOrder { .. })
        ));
    }

    #[test]
    fn rejects_unsupported_provider_order_entries() {
        let settings = AppSettings {
            enabled_providers: vec!["openai-codex".to_owned()],
            provider_order: vec![
                "openai-codex".to_owned(),
                "anthropic".to_owned(),
                "gemini".to_owned(),
                "zai".to_owned(),
            ],
            refresh_interval_secs: 60,
            visible_provider_limit: 2,
            locale: "zh-CN".to_owned(),
        };

        assert!(matches!(
            settings.validate(),
            Err(SettingsError::InvalidProviderOrder { .. })
        ));
    }

    #[test]
    fn ordered_enabled_providers_follow_saved_order() {
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

        assert_eq!(
            settings.ordered_enabled_providers().collect::<Vec<_>>(),
            vec!["zai", "openai-codex"]
        );
    }
}
