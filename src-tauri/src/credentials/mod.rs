pub mod claude_file;
pub mod codex_file;
pub mod deepseek_secret;
pub mod kimi_secret;
pub mod minimax_secret;
pub mod zai_secret;

use std::{fs, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CredentialMaterial {
    Oauth {
        access_token: String,
        id_token: Option<String>,
        refresh_token: Option<String>,
    },
    ApiKey(String),
}

impl CredentialMaterial {
    pub fn refresh_token(&self) -> Option<&str> {
        match self {
            Self::Oauth { refresh_token, .. } => refresh_token.as_deref(),
            Self::ApiKey(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCredential {
    pub provider: String,
    pub material: CredentialMaterial,
    pub expires_at: Option<i64>,
    pub account_id: Option<String>,
    pub email: Option<String>,
}

impl UsageCredential {
    pub fn is_expired_at(&self, now_ms: i64) -> bool {
        matches!(self.material, CredentialMaterial::Oauth { .. })
            && self
                .expires_at
                .is_some_and(|expires_at| expires_at <= now_ms)
    }

    pub fn needs_refresh_at(&self, now_ms: i64, skew_ms: i64) -> bool {
        matches!(self.material, CredentialMaterial::Oauth { .. })
            && self.material.refresh_token().is_some()
            && self
                .expires_at
                .is_some_and(|expires_at| expires_at <= now_ms.saturating_add(skew_ms))
    }
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("home directory is unavailable")]
    HomeDirectoryUnavailable,
    #[error("credential file not found: {path}")]
    MissingFile { path: PathBuf },
    #[error("failed to read credential file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse credential file {path}: {source}")]
    ParseFile {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("credential file {path} is missing required field {field}")]
    MissingField { path: PathBuf, field: &'static str },
    #[error("missing configured secret {provider}.{field}")]
    MissingSecret {
        provider: &'static str,
        field: &'static str,
    },
    #[error("failed to refresh {provider} credential (HTTP {status}: {body}); run claude login again if the refresh token is no longer valid")]
    RefreshHttp {
        provider: &'static str,
        status: u16,
        body: String,
    },
    #[error("failed to refresh {provider} credential over the network: {source}")]
    RefreshNetwork {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("failed to decode refreshed {provider} credential: {source}")]
    RefreshDecode {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("refreshed {provider} credential response has invalid field {field}")]
    RefreshInvalidResponse {
        provider: &'static str,
        field: &'static str,
    },
    #[error("failed to serialize credential file {path}: {source}")]
    SerializeFile {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to write credential file {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to replace credential file {path}: {source}")]
    ReplaceFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub trait SecretStore: Send + Sync {
    fn get_secret(&self, provider: &str, field: &str) -> Option<String>;
}

#[async_trait::async_trait]
pub trait CredentialSource: Send + Sync {
    fn provider(&self) -> &'static str;

    async fn load(&self, secrets: &dyn SecretStore) -> Result<UsageCredential, CredentialError>;

    async fn refresh(
        &self,
        _client: &reqwest::Client,
        _credential: &UsageCredential,
    ) -> Result<Option<UsageCredential>, CredentialError> {
        Ok(None)
    }
}

pub fn source_for_provider(provider: &str) -> Option<Box<dyn CredentialSource>> {
    crate::providers::find(provider).map(|descriptor| (descriptor.credentials)())
}

pub fn home_path(parts: &[&str]) -> Result<PathBuf, CredentialError> {
    let mut path = dirs::home_dir().ok_or(CredentialError::HomeDirectoryUnavailable)?;
    for part in parts {
        path.push(part);
    }
    Ok(path)
}

pub fn read_json_file<T>(path: PathBuf) -> Result<T, CredentialError>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.is_file() {
        return Err(CredentialError::MissingFile { path });
    }

    let text = fs::read_to_string(&path).map_err(|source| CredentialError::ReadFile {
        path: path.clone(),
        source,
    })?;

    serde_json::from_str(&text).map_err(|source| CredentialError::ParseFile { path, source })
}

pub(crate) fn required_string(
    path: &std::path::Path,
    value: Option<String>,
    field: &'static str,
) -> Result<String, CredentialError> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CredentialError::MissingField {
            path: path.to_path_buf(),
            field,
        })
}

pub(crate) fn parse_epoch_millis(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    match value {
        Value::Number(number) => number.as_i64().and_then(normalize_epoch_number),
        Value::String(text) => parse_epoch_string(text),
        _ => None,
    }
}

fn parse_epoch_string(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(value) = trimmed.parse::<i64>() {
        return normalize_epoch_number(value);
    }

    DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|value| value.with_timezone(&Utc).timestamp_millis())
}

fn normalize_epoch_number(value: i64) -> Option<i64> {
    if value <= 0 {
        None
    } else if value < 1_000_000_000_000 {
        value.checked_mul(1000)
    } else {
        Some(value)
    }
}

pub(crate) fn normalize_email(email: Option<String>) -> Option<String> {
    email
        .map(|email| email.trim().to_ascii_lowercase())
        .filter(|email| !email.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{parse_epoch_millis, CredentialMaterial, UsageCredential};
    use serde_json::json;

    #[test]
    fn parses_epoch_seconds_and_millis() {
        assert_eq!(
            parse_epoch_millis(Some(&json!(1_700_000_000))),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            parse_epoch_millis(Some(&json!(1_700_000_000_123_i64))),
            Some(1_700_000_000_123)
        );
    }

    #[test]
    fn parses_rfc3339_timestamp() {
        assert_eq!(
            parse_epoch_millis(Some(&json!("2026-05-17T00:00:00Z"))),
            Some(1_778_976_000_000),
        );
    }

    #[test]
    fn detects_oauth_credentials_that_need_refresh() {
        let now_ms = 1_000_000;
        let skew_ms = 300_000;

        assert!(oauth_credential(Some(now_ms - 1), Some("refresh")).needs_refresh_at(now_ms, 0));
        assert!(oauth_credential(Some(now_ms + skew_ms), Some("refresh"))
            .needs_refresh_at(now_ms, skew_ms));
        assert!(
            !oauth_credential(Some(now_ms + skew_ms + 1), Some("refresh"))
                .needs_refresh_at(now_ms, skew_ms)
        );
    }

    #[test]
    fn ignores_credentials_that_cannot_be_refreshed() {
        let now_ms = 1_000_000;
        let skew_ms = 300_000;

        assert!(!oauth_credential(Some(now_ms - 1), None).needs_refresh_at(now_ms, skew_ms));
        assert!(!oauth_credential(None, Some("refresh")).needs_refresh_at(now_ms, skew_ms));
        assert!(!UsageCredential {
            provider: "zai".to_owned(),
            material: CredentialMaterial::ApiKey("secret".to_owned()),
            expires_at: Some(now_ms - 1),
            account_id: None,
            email: None,
        }
        .needs_refresh_at(now_ms, skew_ms));
    }

    fn oauth_credential(expires_at: Option<i64>, refresh_token: Option<&str>) -> UsageCredential {
        UsageCredential {
            provider: "anthropic".to_owned(),
            material: CredentialMaterial::Oauth {
                access_token: "access".to_owned(),
                id_token: None,
                refresh_token: refresh_token.map(str::to_owned),
            },
            expires_at,
            account_id: None,
            email: None,
        }
    }
}
