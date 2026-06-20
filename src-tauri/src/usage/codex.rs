use std::sync::Arc;

use base64::{engine::general_purpose, Engine as _};
use serde_json::Value;

use super::{
    bool_from_value, now_millis, number_from_keys, reset_millis_from_keys, urgency_from_percent,
    AccountInfo, Bucket, CountUnit, Progress, ProviderMetric, ProviderSnapshot, Quota,
    RateLimitGate, ReportSource, SourceError, ABSOLUTE_RESET_KEYS,
    RELATIVE_RESET_AFTER_SECONDS_KEYS,
};
use crate::{
    credentials::{
        codex_file::CodexFileCredentialSource, CredentialMaterial, CredentialSource,
        UsageCredential,
    },
    providers::ProviderDescriptor,
};

const PROVIDER_ID: &str = "openai-codex";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const JWT_AUTH_CLAIM: &str = "https://api.openai.com/auth";
const JWT_PROFILE_CLAIM: &str = "https://api.openai.com/profile";
const SHORT_KEY: &str = "codex.short";
const LONG_KEY: &str = "codex.long";

#[derive(Debug, Default)]
pub struct CodexReportSource;

fn report_source(_gate: Arc<RateLimitGate>) -> Box<dyn ReportSource> {
    Box::<CodexReportSource>::default()
}

fn credential_source() -> Box<dyn CredentialSource> {
    Box::<CodexFileCredentialSource>::default()
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: PROVIDER_ID,
    label: "Codex",
    report: report_source,
    credentials: credential_source,
    short_quota_key: SHORT_KEY,
    long_quota_key: LONG_KEY,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatgptClaims {
    pub account_id: Option<String>,
    pub email: Option<String>,
}

#[async_trait::async_trait]
impl ReportSource for CodexReportSource {
    fn provider(&self) -> &'static str {
        PROVIDER_ID
    }

    async fn fetch(
        &self,
        client: &reqwest::Client,
        credential: &UsageCredential,
    ) -> Result<ProviderSnapshot, SourceError> {
        if credential.is_expired_at(now_millis()) {
            return Err(SourceError::Expired {
                provider: PROVIDER_ID,
            });
        }

        let CredentialMaterial::Oauth {
            access_token,
            id_token,
            ..
        } = &credential.material
        else {
            return Err(SourceError::UnsupportedCredential {
                provider: PROVIDER_ID,
                expected: "OAuth",
            });
        };

        let id_claims = id_token.as_deref().and_then(read_chatgpt_claims);
        let access_claims = read_chatgpt_claims(access_token);
        let account_id = credential
            .account_id
            .clone()
            .or_else(|| {
                id_claims
                    .as_ref()
                    .and_then(|claims| claims.account_id.clone())
            })
            .or_else(|| {
                access_claims
                    .as_ref()
                    .and_then(|claims| claims.account_id.clone())
            });
        let email = credential
            .email
            .clone()
            .or_else(|| id_claims.as_ref().and_then(|claims| claims.email.clone()))
            .or_else(|| {
                access_claims
                    .as_ref()
                    .and_then(|claims| claims.email.clone())
            });

        let mut request = client
            .get(CODEX_USAGE_URL)
            .bearer_auth(access_token)
            .header(reqwest::header::USER_AGENT, "AlexBar/0.1.0");
        if let Some(identifier) = account_id.as_deref() {
            request = request.header("ChatGPT-Account-Id", identifier);
        }

        let response = request
            .send()
            .await
            .map_err(|source| SourceError::Network {
                provider: PROVIDER_ID,
                source,
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(SourceError::Http {
                provider: PROVIDER_ID,
                status: status.as_u16(),
            });
        }

        let payload = response
            .json::<Value>()
            .await
            .map_err(|source| SourceError::Decode {
                provider: PROVIDER_ID,
                source,
            })?;

        codex_snapshot_from_payload(&payload, account_id, email)
    }
}

pub fn read_chatgpt_claims(token: &str) -> Option<ChatgptClaims> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if parts.next().is_some() || signature.is_empty() {
        return None;
    }

    let bytes = general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let payload = serde_json::from_slice::<Value>(&bytes).ok()?;
    let account_id = payload
        .get(JWT_AUTH_CLAIM)
        .and_then(Value::as_object)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let email = payload
        .get(JWT_PROFILE_CLAIM)
        .and_then(Value::as_object)
        .and_then(|profile| profile.get("email"))
        .and_then(Value::as_str)
        .map(|email| email.trim().to_ascii_lowercase())
        .filter(|email| !email.is_empty());

    Some(ChatgptClaims { account_id, email })
}

fn codex_snapshot_from_payload(
    payload: &Value,
    account_id: Option<String>,
    email: Option<String>,
) -> Result<ProviderSnapshot, SourceError> {
    let now_ms = now_millis();
    let object = payload.as_object().ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message: "missing usage object",
    })?;
    let rate =
        object
            .get("rate_limit")
            .and_then(Value::as_object)
            .ok_or(SourceError::BadPayload {
                provider: PROVIDER_ID,
                message: "missing rate_limit object",
            })?;

    let plan = object
        .get("plan_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let allowed = bool_from_value(rate.get("allowed"));
    let reached = bool_from_value(rate.get("limit_reached")).unwrap_or(false);

    let mut quotas = Vec::with_capacity(2);
    if let Some(quota) = codex_quota_from_window(
        rate.get("primary_window"),
        SHORT_KEY,
        "短周期",
        reached,
        now_ms,
    ) {
        quotas.push(quota);
    }
    if let Some(quota) = codex_quota_from_window(
        rate.get("secondary_window"),
        LONG_KEY,
        "长周期",
        reached,
        now_ms,
    ) {
        quotas.push(quota);
    }

    if quotas.is_empty() && allowed.is_none() && !reached {
        return Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "missing rate_limit windows",
        });
    }

    Ok(ProviderSnapshot {
        provider: PROVIDER_ID.to_owned(),
        refreshed_at: now_ms,
        account: account_info(account_id, email, plan),
        metrics: quotas.into_iter().map(ProviderMetric::from).collect(),
        note: None,
    })
}

fn codex_quota_from_window(
    value: Option<&Value>,
    key: &str,
    fallback_display_name: &str,
    reached: bool,
    now_ms: i64,
) -> Option<Quota> {
    let object = value?.as_object()?;
    let used_percent = number_from_keys(
        object,
        &[
            "used_percent",
            "usedPercent",
            "usage_percent",
            "usagePercentage",
            "percentage",
        ],
    );
    let duration_secs = number_from_keys(
        object,
        &[
            "limit_window_seconds",
            "limitWindowSeconds",
            "window_seconds",
            "windowSeconds",
        ],
    );
    let resets_at = reset_millis_from_keys(
        object,
        ABSOLUTE_RESET_KEYS,
        RELATIVE_RESET_AFTER_SECONDS_KEYS,
        now_ms,
    );
    if used_percent.is_none() && duration_secs.is_none() && resets_at.is_none() {
        return None;
    }
    let (bucket, label) = match duration_secs {
        Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
            let label = format_rolling_window(seconds);
            let bucket = Bucket::Rolling {
                duration_ms: (seconds * 1000.0).round() as i64,
                label: label.clone(),
                resets_at,
            };
            (bucket, label)
        }
        _ => {
            let label = fallback_display_name.to_owned();
            let bucket = Bucket::OpenEnded {
                label: label.clone(),
                resets_at,
            };
            (bucket, label)
        }
    };
    let progress = used_percent
        .map(Progress::ratio)
        .unwrap_or_else(|| Progress::counted(None, None, None, None, CountUnit::Requests));
    let urgency = urgency_from_percent(progress.used_percent(), reached);

    Some(Quota {
        key: key.to_owned(),
        display_name: label,
        bucket,
        progress,
        urgency,
    })
}

fn account_info(
    identifier: Option<String>,
    email: Option<String>,
    plan: Option<String>,
) -> Option<AccountInfo> {
    if identifier.is_none() && email.is_none() && plan.is_none() {
        None
    } else {
        Some(AccountInfo {
            identifier,
            email,
            plan,
        })
    }
}

fn format_rolling_window(seconds: f64) -> String {
    const HOUR_SECONDS: f64 = 60.0 * 60.0;
    const DAY_SECONDS: f64 = 24.0 * HOUR_SECONDS;

    let hours = (seconds / HOUR_SECONDS).round().max(1.0) as i64;
    if hours < 24 {
        return format!("{hours} 小时");
    }

    let days = (seconds / DAY_SECONDS).round().max(1.0) as i64;
    format!("{days} 天")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct EmptySecretStore;

    impl crate::credentials::SecretStore for EmptySecretStore {
        fn get_secret(&self, _provider: &str, _field: &str) -> Option<String> {
            None
        }
    }

    fn jwt_with_payload(payload: Value) -> String {
        let header = general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        format!("{header}.{payload}.signature")
    }

    fn quota_at(snapshot: &ProviderSnapshot, index: usize) -> &Quota {
        match &snapshot.metrics[index] {
            ProviderMetric::Quota(quota) => quota,
            ProviderMetric::Balance(_) => panic!("expected quota metric"),
        }
    }

    #[test]
    fn reads_chatgpt_claims_once() {
        let token = jwt_with_payload(json!({
            JWT_AUTH_CLAIM: { "chatgpt_account_id": "acct_123" },
            JWT_PROFILE_CLAIM: { "email": "USER@Example.COM" }
        }));

        let claims = read_chatgpt_claims(&token).unwrap();
        assert_eq!(claims.account_id.as_deref(), Some("acct_123"));
        assert_eq!(claims.email.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn turns_codex_payload_into_quotas() {
        let payload = json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 42,
                    "limit_window_seconds": 18_000,
                    "reset_after_seconds": 60
                },
                "secondary_window": {
                    "used_percent": "91.5",
                    "limit_window_seconds": 604_800,
                    "reset_at": 1_778_976_000
                }
            }
        });

        let snapshot =
            codex_snapshot_from_payload(&payload, Some("acct".to_owned()), None).unwrap();
        assert_eq!(snapshot.provider, PROVIDER_ID);
        assert_eq!(
            snapshot.account.as_ref().unwrap().plan.as_deref(),
            Some("plus")
        );
        assert_eq!(snapshot.metrics.len(), 2);
        assert_eq!(quota_at(&snapshot, 0).display_name, "5 小时");
        assert_eq!(quota_at(&snapshot, 0).key, SHORT_KEY);
        assert_eq!(quota_at(&snapshot, 0).progress.used_fraction(), Some(0.42));
        assert_eq!(quota_at(&snapshot, 0).urgency, super::super::Urgency::Calm);
        assert!(matches!(
            quota_at(&snapshot, 0).bucket,
            Bucket::Rolling {
                duration_ms: 18_000_000,
                ..
            }
        ));
        assert_eq!(quota_at(&snapshot, 1).key, LONG_KEY);
        assert_eq!(quota_at(&snapshot, 1).display_name, "7 天");
        assert_eq!(quota_at(&snapshot, 1).urgency, super::super::Urgency::Tense);
        assert!(matches!(
            quota_at(&snapshot, 1).bucket,
            Bucket::Rolling {
                duration_ms: 604_800_000,
                resets_at: Some(1_778_976_000_000),
                ..
            }
        ));
    }

    #[test]
    fn reads_codex_window_aliases_without_plan_suffix() {
        let payload = json!({
            "plan_type": "prolite",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "usedPercent": "12.5",
                    "limitWindowSeconds": 18_000,
                    "resetAt": "2026-05-17T00:00:00Z"
                },
                "secondary_window": {
                    "usagePercentage": 80,
                    "windowSeconds": 604_800,
                    "resetAfterSeconds": 30
                }
            }
        });

        let snapshot =
            codex_snapshot_from_payload(&payload, Some("acct".to_owned()), None).unwrap();
        assert_eq!(
            snapshot.account.as_ref().unwrap().plan.as_deref(),
            Some("prolite")
        );
        assert_eq!(quota_at(&snapshot, 0).display_name, "5 小时");
        assert_eq!(quota_at(&snapshot, 0).progress.used_fraction(), Some(0.125));
        assert!(matches!(
            quota_at(&snapshot, 0).bucket,
            Bucket::Rolling {
                resets_at: Some(1_778_976_000_000),
                ..
            }
        ));
        assert_eq!(quota_at(&snapshot, 1).display_name, "7 天");
        assert!(snapshot.metrics.iter().all(|metric| match metric {
            ProviderMetric::Quota(quota) => !quota.display_name.contains("prolite"),
            ProviderMetric::Balance(_) => true,
        }));
    }

    #[test]
    fn codex_window_without_usage_percent_stays_unknown() {
        let payload = json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "limitWindowSeconds": 18_000,
                    "resetsAt": 1_778_976_000_000_i64
                }
            }
        });

        let snapshot = codex_snapshot_from_payload(&payload, None, None).unwrap();
        assert_eq!(snapshot.metrics.len(), 1);
        assert_eq!(quota_at(&snapshot, 0).display_name, "5 小时");
        assert_eq!(quota_at(&snapshot, 0).progress.used_percent(), None);
        assert_eq!(
            quota_at(&snapshot, 0).urgency,
            super::super::Urgency::Unknown
        );
        assert!(matches!(
            quota_at(&snapshot, 0).progress,
            Progress::Counted {
                used: None,
                total: None,
                remaining: None,
                used_percent: None,
                ..
            }
        ));
    }

    #[test]
    fn normalizes_codex_reset_sources_as_seconds_or_milliseconds() {
        assert_eq!(
            super::super::normalize_epoch_auto(1_778_976_000.0),
            Some(1_778_976_000_000)
        );
        assert_eq!(
            super::super::normalize_epoch_auto(1_778_976_000_000.0),
            Some(1_778_976_000_000)
        );
        assert_eq!(super::super::millis_after_seconds(1_000, 2.5), Some(3_500));
    }

    #[tokio::test]
    #[ignore = "requires local Codex CLI credentials and network access"]
    async fn live_codex_quota_returns_window_values() {
        use crate::credentials::{codex_file::CodexFileCredentialSource, CredentialSource};

        let credential = CodexFileCredentialSource
            .load(&EmptySecretStore)
            .await
            .expect("local Codex credential file should load");
        let client = reqwest::Client::builder()
            .https_only(true)
            .user_agent("AlexBar/0.1.0")
            .build()
            .expect("HTTP client should build");
        let snapshot = CodexReportSource
            .fetch(&client, &credential)
            .await
            .expect("Codex report endpoint should return a snapshot");

        assert!(snapshot.note.is_none());
        assert!(snapshot.metrics.iter().any(|metric| matches!(
            metric,
            ProviderMetric::Quota(quota) if quota.progress.used_percent().is_some()
        )));
    }
}
