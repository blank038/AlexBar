pub mod claude;
pub mod codex;
pub mod zai;

use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::credentials::UsageCredential;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSnapshot {
    pub provider: String,
    pub refreshed_at: i64,
    pub account: Option<AccountInfo>,
    pub quotas: Vec<Quota>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub identifier: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quota {
    pub key: String,
    pub display_name: String,
    pub bucket: Bucket,
    pub progress: Progress,
    pub urgency: Urgency,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Bucket {
    Rolling {
        duration_ms: i64,
        label: String,
        resets_at: Option<i64>,
    },
    OpenEnded {
        label: String,
        resets_at: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Progress {
    Ratio {
        used_percent: f32,
    },
    Counted {
        used: Option<f64>,
        total: Option<f64>,
        remaining: Option<f64>,
        used_percent: Option<f32>,
        unit: CountUnit,
    },
}

impl Progress {
    pub fn ratio(used_percent: f64) -> Self {
        Self::Ratio {
            used_percent: normalize_external_percent(used_percent),
        }
    }

    pub fn counted(
        used: Option<f64>,
        total: Option<f64>,
        remaining: Option<f64>,
        explicit_percent: Option<f64>,
        unit: CountUnit,
    ) -> Self {
        let used_percent = explicit_percent
            .map(normalize_external_percent)
            .or_else(|| percent_from_counts(used, total, remaining));
        Self::Counted {
            used,
            total,
            remaining,
            used_percent,
            unit,
        }
    }

    pub fn used_percent(&self) -> Option<f32> {
        match self {
            Self::Ratio { used_percent } => Some(*used_percent),
            Self::Counted {
                used_percent,
                used,
                total,
                remaining,
                ..
            } => used_percent.or_else(|| percent_from_counts(*used, *total, *remaining)),
        }
    }

    pub fn used_fraction(&self) -> Option<f64> {
        self.used_percent().map(|value| f64::from(value) / 100.0)
    }

    pub fn remaining_fraction(&self) -> Option<f64> {
        self.used_fraction().map(|value| (1.0 - value).max(0.0))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CountUnit {
    Tokens,
    Requests,
    Dollars,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Urgency {
    Calm,
    Tense,
    Capped,
    Unknown,
}

#[derive(Debug, Default)]
pub struct RateLimitGate {
    blocked_until_ms: AtomicI64,
}

impl RateLimitGate {
    pub fn is_blocked(&self, now_ms: i64) -> bool {
        now_ms < self.blocked_until_ms.load(Ordering::Relaxed)
    }

    pub fn block_for(&self, now_ms: i64, delay_ms: i64) {
        if delay_ms > 0 {
            self.blocked_until_ms
                .store(now_ms.saturating_add(delay_ms), Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub fn clear(&self) {
        self.blocked_until_ms.store(0, Ordering::Relaxed);
    }
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("credential expired for {provider}")]
    Expired { provider: &'static str },
    #[error("credential material for {provider} must be {expected}")]
    UnsupportedCredential {
        provider: &'static str,
        expected: &'static str,
    },
    #[error("quota endpoint returned HTTP {status} for {provider}")]
    Http { provider: &'static str, status: u16 },
    #[error(
        "{provider_label} quota endpoint is rate limited; AlexBar will retry on the next refresh"
    )]
    RateLimited {
        provider: &'static str,
        provider_label: &'static str,
    },
    #[error("invalid quota payload for {provider}: {message}")]
    BadPayload {
        provider: &'static str,
        message: &'static str,
    },
    #[error("network request failed for {provider}: {source}")]
    Network {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("failed to decode JSON for {provider}: {source}")]
    Decode {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },
}

#[async_trait]
pub trait QuotaSource: Send + Sync {
    fn provider(&self) -> &'static str;

    async fn fetch(
        &self,
        client: &reqwest::Client,
        credential: &UsageCredential,
    ) -> Result<ProviderSnapshot, SourceError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochScale {
    Seconds,
    Milliseconds,
}

pub(crate) const ABSOLUTE_RESET_KEYS: &[&str] = &[
    "reset_at",
    "resetAt",
    "resets_at",
    "resetsAt",
    "reset_time",
    "resetTime",
    "nextResetTime",
    "next_reset_time",
];

pub(crate) const RELATIVE_RESET_AFTER_SECONDS_KEYS: &[&str] = &[
    "reset_after_seconds",
    "resetAfterSeconds",
    "reset_after",
    "resetAfter",
    "resets_after_seconds",
    "resetsAfterSeconds",
];

pub fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn urgency_from_percent(used_percent: Option<f32>, forced_cap: bool) -> Urgency {
    if forced_cap {
        return Urgency::Capped;
    }

    match used_percent {
        None => Urgency::Unknown,
        Some(value) if value >= 100.0 => Urgency::Capped,
        Some(value) if value >= 90.0 => Urgency::Tense,
        Some(_) => Urgency::Calm,
    }
}

pub fn failure_snapshot(provider: &str, message: impl Into<String>) -> ProviderSnapshot {
    ProviderSnapshot {
        provider: provider.to_owned(),
        refreshed_at: now_millis(),
        account: None,
        quotas: Vec::new(),
        note: Some(message.into()),
    }
}

pub fn quota_used_fraction(quota: &Quota) -> Option<f64> {
    if quota.urgency == Urgency::Unknown {
        None
    } else {
        quota.progress.used_fraction()
    }
}

pub fn quota_remaining_fraction(quota: &Quota) -> Option<f64> {
    if quota.urgency == Urgency::Unknown {
        None
    } else {
        quota.progress.remaining_fraction()
    }
}

pub(crate) fn clamp_percent(value: f64) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 100.0) as f32
    } else {
        0.0
    }
}

pub(crate) fn number_from_value(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64().filter(|value| value.is_finite()),
        Some(Value::String(text)) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite()),
        _ => None,
    }
}

pub(crate) fn number_from_keys(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<f64> {
    keys.iter()
        .find_map(|key| number_from_value(object.get(*key)))
}

pub(crate) fn epoch_millis_from_value(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .and_then(normalize_epoch_auto)
                .or_else(|| {
                    chrono::DateTime::parse_from_rfc3339(trimmed)
                        .ok()
                        .map(|value| value.timestamp_millis())
                })
        }
        _ => number_from_value(value).and_then(normalize_epoch_auto),
    }
}

pub(crate) fn epoch_millis_from_keys(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<i64> {
    keys.iter()
        .find_map(|key| epoch_millis_from_value(object.get(*key)))
}

pub(crate) fn reset_millis_from_keys(
    object: &serde_json::Map<String, Value>,
    absolute_keys: &[&str],
    relative_second_keys: &[&str],
    now_ms: i64,
) -> Option<i64> {
    epoch_millis_from_keys(object, absolute_keys).or_else(|| {
        number_from_keys(object, relative_second_keys)
            .and_then(|seconds| millis_after_seconds(now_ms, seconds))
    })
}

pub(crate) fn millis_after_seconds(now_ms: i64, seconds: f64) -> Option<i64> {
    if seconds.is_finite() && seconds >= 0.0 {
        Some(now_ms.saturating_add((seconds * 1000.0).round() as i64))
    } else {
        None
    }
}

pub(crate) fn bool_from_value(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(value)) => Some(*value),
        _ => None,
    }
}

pub(crate) fn normalize_epoch(value: f64, scale: EpochScale) -> Option<i64> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }

    let millis = match scale {
        EpochScale::Seconds => value * 1000.0,
        EpochScale::Milliseconds => value,
    };
    if millis.is_finite() && millis > 0.0 {
        Some(millis.round() as i64)
    } else {
        None
    }
}

pub(crate) fn normalize_epoch_auto(value: f64) -> Option<i64> {
    const MODERN_EPOCH_MS_FLOOR: f64 = 946_684_800_000.0;

    if value >= MODERN_EPOCH_MS_FLOOR {
        normalize_epoch(value, EpochScale::Milliseconds)
    } else {
        normalize_epoch(value, EpochScale::Seconds)
    }
}

fn normalize_external_percent(value: f64) -> f32 {
    if value.is_finite() && value > 0.0 && value < 1.0 {
        clamp_percent(value * 100.0)
    } else {
        clamp_percent(value)
    }
}

fn percent_from_counts(
    used: Option<f64>,
    total: Option<f64>,
    remaining: Option<f64>,
) -> Option<f32> {
    if let (Some(used), Some(total)) = (used, total) {
        if used.is_finite() && total.is_finite() && total > 0.0 {
            return Some(clamp_percent((used / total) * 100.0));
        }
    }

    if let (Some(total), Some(remaining)) = (total, remaining) {
        if total.is_finite() && remaining.is_finite() && total > 0.0 {
            return Some(clamp_percent(((total - remaining) / total) * 100.0));
        }
    }

    if let (Some(used), Some(remaining)) = (used, remaining) {
        if used.is_finite() && remaining.is_finite() {
            let total = used + remaining;
            if total > 0.0 {
                return Some(clamp_percent((used / total) * 100.0));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_fractional_external_percent_values() {
        assert_eq!(Progress::ratio(0.42).used_percent(), Some(42.0));
        assert_eq!(
            Progress::counted(None, None, None, Some(0.125), CountUnit::Requests).used_percent(),
            Some(12.5)
        );
        assert_eq!(
            Progress::counted(None, None, None, Some(1.0), CountUnit::Requests).used_percent(),
            Some(1.0)
        );
    }

    #[test]
    fn derives_counted_percent_from_remaining_values() {
        assert_eq!(
            Progress::counted(None, Some(100.0), Some(25.0), None, CountUnit::Requests)
                .used_percent(),
            Some(75.0)
        );
        assert_eq!(
            Progress::counted(Some(25.0), None, Some(75.0), None, CountUnit::Requests)
                .used_percent(),
            Some(25.0)
        );
    }

    #[test]
    fn serializes_enum_fields_as_frontend_camel_case() {
        let quota = Quota {
            key: "codex.short".to_owned(),
            display_name: "5 小时".to_owned(),
            bucket: Bucket::Rolling {
                duration_ms: 18_000_000,
                label: "5 小时".to_owned(),
                resets_at: Some(1_779_444_485_000),
            },
            progress: Progress::ratio(56.0),
            urgency: Urgency::Calm,
        };

        let value = serde_json::to_value(quota).unwrap();
        assert_eq!(value["displayName"], "5 小时");
        assert_eq!(value["bucket"]["durationMs"], 18_000_000);
        assert_eq!(value["bucket"]["resetsAt"], 1_779_444_485_000_i64);
        assert_eq!(value["progress"]["usedPercent"], 56.0);
        assert!(value["bucket"].get("duration_ms").is_none());
        assert!(value["bucket"].get("resets_at").is_none());
        assert!(value["progress"].get("used_percent").is_none());
    }
}
