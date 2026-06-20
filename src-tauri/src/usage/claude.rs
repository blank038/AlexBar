use std::{sync::Arc, time::Duration};

use serde_json::Value;

use super::{
    now_millis, number_from_keys, reset_millis_from_keys, urgency_from_percent, AccountInfo,
    Bucket, Progress, ProviderMetric, ProviderSnapshot, Quota, RateLimitGate, ReportSource,
    SourceError, ABSOLUTE_RESET_KEYS, RELATIVE_RESET_AFTER_SECONDS_KEYS,
};
use crate::{
    credentials::{
        claude_file::ClaudeFileCredentialSource, CredentialMaterial, CredentialSource,
        UsageCredential,
    },
    providers::ProviderDescriptor,
};

const PROVIDER_ID: &str = "anthropic";
const PROVIDER_LABEL: &str = "Claude";
const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/api/oauth";
const FIVE_HOURS_MS: i64 = 5 * 60 * 60 * 1000;
const SEVEN_DAYS_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const DEFAULT_RATE_LIMIT_BACKOFF_MS: i64 = 60_000;
const ANTHROPIC_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05";
const CLAUDE_USER_AGENT: &str = "claude-cli/2.1.63 (external, cli)";
const SHORT_KEY: &str = "claude.5h";
const LONG_KEY: &str = "claude.7d";

#[derive(Debug)]
pub struct ClaudeReportSource {
    gate: Arc<RateLimitGate>,
}

impl ClaudeReportSource {
    fn new(gate: Arc<RateLimitGate>) -> Self {
        Self { gate }
    }
}

fn report_source(gate: Arc<RateLimitGate>) -> Box<dyn ReportSource> {
    Box::new(ClaudeReportSource::new(gate))
}

fn credential_source() -> Box<dyn CredentialSource> {
    Box::<ClaudeFileCredentialSource>::default()
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: PROVIDER_ID,
    label: PROVIDER_LABEL,
    report: report_source,
    credentials: credential_source,
    short_quota_key: SHORT_KEY,
    long_quota_key: LONG_KEY,
};

#[derive(Debug, Clone, Copy, PartialEq)]
struct WindowUtilization {
    used_percent: f64,
    resets_at: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct Retry {
    max_attempts: usize,
    base_delay: Duration,
    max_delay: Duration,
}

impl Retry {
    fn new(max_attempts: usize) -> Self {
        Self {
            max_attempts,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_millis(5_000),
        }
    }

    fn with_backoff(mut self, base_delay: Duration, max_delay: Duration) -> Self {
        self.base_delay = base_delay;
        self.max_delay = max_delay;
        self
    }

    fn pause_for(
        self,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
        attempt: usize,
    ) -> Option<Duration> {
        if !should_retry_status(status) {
            return None;
        }

        let delay = retry_after(headers).unwrap_or_else(|| self.exponential_pause(attempt));
        if delay <= self.max_delay {
            Some(delay)
        } else {
            None
        }
    }

    fn quiet_payload_pause(self, attempt: usize) -> Duration {
        self.exponential_pause(attempt)
    }

    fn exponential_pause(self, attempt: usize) -> Duration {
        let factor = 2_u64.saturating_pow(attempt as u32);
        Duration::from_millis(self.base_delay.as_millis().saturating_mul(factor.into()) as u64)
    }
}

#[async_trait::async_trait]
impl ReportSource for ClaudeReportSource {
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
        let CredentialMaterial::Oauth { access_token, .. } = &credential.material else {
            return Err(SourceError::UnsupportedCredential {
                provider: PROVIDER_ID,
                expected: "OAuth",
            });
        };

        let base_url = normalize_claude_base_url(DEFAULT_ENDPOINT);
        let usage_url = format!("{base_url}/usage");
        let (payload, org_id) =
            fetch_usage_payload(client, &usage_url, access_token, &self.gate).await?;
        build_snapshot_from_payload(
            client,
            &base_url,
            &payload,
            org_id,
            credential,
            access_token,
        )
        .await
    }
}

async fn fetch_usage_payload(
    client: &reqwest::Client,
    url: &str,
    access_token: &str,
    gate: &RateLimitGate,
) -> Result<(Value, Option<String>), SourceError> {
    if gate.is_blocked(now_millis()) {
        return Err(SourceError::RateLimited {
            provider: PROVIDER_ID,
            provider_label: PROVIDER_LABEL,
        });
    }

    let retry =
        Retry::new(3).with_backoff(Duration::from_millis(500), Duration::from_millis(5_000));
    let mut last_payload = None;
    let mut last_org_id = None;

    for attempt in 0..retry.max_attempts {
        let response = client
            .get(url)
            .bearer_auth(access_token)
            .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, CLAUDE_USER_AGENT)
            .send()
            .await
            .map_err(|source| SourceError::Network {
                provider: PROVIDER_ID,
                source,
            })?;

        let status = response.status();
        if !status.is_success() {
            if attempt + 1 < retry.max_attempts {
                if let Some(delay) = retry.pause_for(status, response.headers(), attempt) {
                    tokio::time::sleep(delay).await;
                    continue;
                }
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                gate.block_for(now_millis(), backoff_from_headers(response.headers()));
                return Err(SourceError::RateLimited {
                    provider: PROVIDER_ID,
                    provider_label: PROVIDER_LABEL,
                });
            }

            return Err(SourceError::Http {
                provider: PROVIDER_ID,
                status: status.as_u16(),
            });
        }

        let org_id = response
            .headers()
            .get("anthropic-organization-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let payload = response
            .json::<Value>()
            .await
            .map_err(|source| SourceError::Decode {
                provider: PROVIDER_ID,
                source,
            })?;

        if org_id.is_some() {
            last_org_id = org_id.clone();
        }
        if claude_payload_has_window(&payload) {
            return Ok((payload, org_id.or(last_org_id)));
        }
        last_payload = Some(payload);

        if attempt + 1 < retry.max_attempts {
            tokio::time::sleep(retry.quiet_payload_pause(attempt)).await;
        }
    }

    last_payload
        .map(|payload| (payload, last_org_id))
        .ok_or(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "empty usage response",
        })
}

async fn build_snapshot_from_payload(
    client: &reqwest::Client,
    base_url: &str,
    payload: &Value,
    org_id: Option<String>,
    credential: &UsageCredential,
    access_token: &str,
) -> Result<ProviderSnapshot, SourceError> {
    let quotas = claude_quotas_from_payload(payload, now_millis());
    if quotas.is_empty() {
        return Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "no usable quota buckets",
        });
    }

    let account =
        resolve_identity(client, base_url, payload, org_id, credential, access_token).await?;
    Ok(ProviderSnapshot {
        provider: PROVIDER_ID.to_owned(),
        refreshed_at: now_millis(),
        account,
        metrics: quotas.into_iter().map(ProviderMetric::from).collect(),
        note: None,
    })
}

fn claude_quotas_from_payload(payload: &Value, _now_ms: i64) -> Vec<Quota> {
    let mut quotas = Vec::with_capacity(4);
    append_window_quota(
        &mut quotas,
        payload.get("five_hour"),
        SHORT_KEY,
        FIVE_HOURS_MS,
        "5 小时",
    );
    append_window_quota(
        &mut quotas,
        payload.get("seven_day"),
        LONG_KEY,
        SEVEN_DAYS_MS,
        "7 天",
    );
    append_window_quota(
        &mut quotas,
        payload.get("seven_day_opus"),
        "claude.7d.opus",
        SEVEN_DAYS_MS,
        "7 天",
    );
    append_window_quota(
        &mut quotas,
        payload.get("seven_day_sonnet"),
        "claude.7d.sonnet",
        SEVEN_DAYS_MS,
        "7 天",
    );
    quotas
}

fn append_window_quota(
    quotas: &mut Vec<Quota>,
    value: Option<&Value>,
    key: &str,
    duration_ms: i64,
    bucket_label: &str,
) {
    let Some(window) = read_window_utilization(value) else {
        return;
    };
    let progress = Progress::ratio(window.used_percent);
    quotas.push(Quota {
        key: key.to_owned(),
        display_name: bucket_label.to_owned(),
        bucket: Bucket::Rolling {
            duration_ms,
            label: bucket_label.to_owned(),
            resets_at: window.resets_at,
        },
        urgency: urgency_from_percent(progress.used_percent(), false),
        progress,
    });
}

fn read_window_utilization(value: Option<&Value>) -> Option<WindowUtilization> {
    let object = value?.as_object()?;
    let used_percent = number_from_keys(
        object,
        &["utilization", "used_percent", "usedPercent", "percentage"],
    )?;
    let resets_at = reset_millis_from_keys(
        object,
        ABSOLUTE_RESET_KEYS,
        RELATIVE_RESET_AFTER_SECONDS_KEYS,
        now_millis(),
    );
    Some(WindowUtilization {
        used_percent,
        resets_at,
    })
}

fn claude_payload_has_window(payload: &Value) -> bool {
    payload.as_object().is_some_and(|object| {
        [
            "five_hour",
            "seven_day",
            "seven_day_opus",
            "seven_day_sonnet",
        ]
        .iter()
        .any(|key| object.get(*key).is_some_and(|value| !value.is_null()))
    })
}

fn should_retry_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|retry_at| {
            Duration::from_millis((retry_at.timestamp_millis() - now_millis()).max(0) as u64)
        })
}

fn backoff_from_headers(headers: &reqwest::header::HeaderMap) -> i64 {
    retry_after(headers)
        .map(|delay| delay.as_millis().min(i64::MAX as u128) as i64)
        .filter(|delay| *delay > 0)
        .unwrap_or(DEFAULT_RATE_LIMIT_BACKOFF_MS)
}

async fn resolve_identity(
    client: &reqwest::Client,
    base_url: &str,
    payload: &Value,
    org_id: Option<String>,
    credential: &UsageCredential,
    access_token: &str,
) -> Result<Option<AccountInfo>, SourceError> {
    let (payload_identifier, payload_email) = identity_from_usage_payload(payload, org_id);
    let identifier = payload_identifier.or_else(|| credential.account_id.clone());
    let email = match payload_email.or_else(|| credential.email.clone()) {
        Some(email) => Some(email),
        None => fetch_profile_email(client, base_url, access_token).await?,
    };

    if identifier.is_none() && email.is_none() {
        Ok(None)
    } else {
        Ok(Some(AccountInfo {
            identifier,
            email,
            plan: None,
        }))
    }
}

fn identity_from_usage_payload(
    payload: &Value,
    org_id: Option<String>,
) -> (Option<String>, Option<String>) {
    let Some(object) = payload.as_object() else {
        return (org_id, None);
    };
    let identifier = [
        "account_id",
        "accountId",
        "user_id",
        "userId",
        "org_id",
        "orgId",
    ]
    .iter()
    .find_map(|key| trimmed_string(object.get(*key)))
    .or(org_id);
    let email = ["email", "user_email", "userEmail"]
        .iter()
        .find_map(|key| trimmed_string(object.get(*key)))
        .map(|email| email.to_ascii_lowercase());
    (identifier, email)
}

fn trimmed_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_claude_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let lower = trimmed.to_ascii_lowercase();
    if lower.ends_with("/api/oauth") {
        return trimmed.to_owned();
    }

    match url::Url::parse(trimmed) {
        Ok(mut url) => {
            let mut path = url.path().trim_end_matches('/').to_owned();
            if path == "/" {
                path.clear();
            }
            if path.to_ascii_lowercase().ends_with("/v1") {
                let len = path.len() - 3;
                path.truncate(len);
            }
            let next = if path.is_empty() {
                "/api/oauth".to_owned()
            } else {
                format!("{path}/api/oauth")
            };
            url.set_path(&next);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string().trim_end_matches('/').to_owned()
        }
        Err(_) => DEFAULT_ENDPOINT.to_owned(),
    }
}

async fn fetch_profile_email(
    client: &reqwest::Client,
    base_url: &str,
    access_token: &str,
) -> Result<Option<String>, SourceError> {
    let response = client
        .get(format!("{base_url}/profile"))
        .bearer_auth(access_token)
        .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
        .header("anthropic-beta", ANTHROPIC_BETA)
        .header(reqwest::header::USER_AGENT, CLAUDE_USER_AGENT)
        .send()
        .await
        .map_err(|source| SourceError::Network {
            provider: PROVIDER_ID,
            source,
        })?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let payload = response
        .json::<Value>()
        .await
        .map_err(|source| SourceError::Decode {
            provider: PROVIDER_ID,
            source,
        })?;

    Ok(payload
        .get("account")
        .and_then(Value::as_object)
        .and_then(|account| account.get("email"))
        .and_then(Value::as_str)
        .map(|email| email.trim().to_ascii_lowercase())
        .filter(|email| !email.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct EmptySecretStore;

    impl crate::credentials::SecretStore for EmptySecretStore {
        fn get_secret(&self, _provider: &str, _field: &str) -> Option<String> {
            None
        }
    }

    fn oauth_credential(email: Option<&str>) -> UsageCredential {
        UsageCredential {
            provider: PROVIDER_ID.to_owned(),
            material: crate::credentials::CredentialMaterial::Oauth {
                access_token: "token".to_owned(),
                id_token: None,
                refresh_token: None,
            },
            expires_at: None,
            account_id: None,
            email: email.map(ToOwned::to_owned),
        }
    }

    fn quota_at(snapshot: &ProviderSnapshot, index: usize) -> &Quota {
        match &snapshot.metrics[index] {
            ProviderMetric::Quota(quota) => quota,
            ProviderMetric::Balance(_) => panic!("expected quota metric"),
        }
    }

    #[tokio::test]
    async fn turns_claude_buckets_into_quotas() {
        let payload = json!({
            "five_hour": { "utilization": 12.5, "resets_at": "2026-05-17T00:00:00Z" },
            "seven_day": { "utilization": 95 },
            "seven_day_opus": { "utilization": 100 },
            "account_id": "acct"
        });
        let client = reqwest::Client::new();
        let snapshot = build_snapshot_from_payload(
            &client,
            DEFAULT_ENDPOINT,
            &payload,
            None,
            &oauth_credential(Some("cached@example.com")),
            "token",
        )
        .await
        .unwrap();

        assert_eq!(snapshot.metrics.len(), 3);
        assert_eq!(quota_at(&snapshot, 0).key, SHORT_KEY);
        assert_eq!(quota_at(&snapshot, 0).display_name, "5 小时");
        assert_eq!(quota_at(&snapshot, 0).progress.used_fraction(), Some(0.125));
        assert_eq!(quota_at(&snapshot, 1).urgency, super::super::Urgency::Tense);
        assert_eq!(quota_at(&snapshot, 1).display_name, "7 天");
        assert_eq!(
            quota_at(&snapshot, 2).urgency,
            super::super::Urgency::Capped
        );
    }

    #[test]
    fn reads_claude_window_aliases_and_numeric_reset() {
        let window = read_window_utilization(Some(&json!({
            "usedPercent": "44.5",
            "resetAt": 1_778_976_000_000_i64
        })))
        .unwrap();

        assert_eq!(
            window,
            WindowUtilization {
                used_percent: 44.5,
                resets_at: Some(1_778_976_000_000)
            }
        );

        let iso_window = read_window_utilization(Some(&json!({
            "percentage": 12,
            "resetsAt": "2026-05-17T00:00:00Z"
        })))
        .unwrap();
        assert_eq!(iso_window.used_percent, 12.0);
        assert_eq!(iso_window.resets_at, Some(1_778_976_000_000));
    }

    #[test]
    fn normalizes_base_url() {
        assert_eq!(
            normalize_claude_base_url("https://api.anthropic.com/v1"),
            DEFAULT_ENDPOINT,
        );
        assert_eq!(
            normalize_claude_base_url("https://example.com/custom"),
            "https://example.com/custom/api/oauth",
        );
    }

    #[tokio::test]
    async fn retries_short_rate_limit_response() {
        let gate = RateLimitGate::default();
        gate.clear();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/usage", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            write_response(
                &listener,
                "HTTP/1.1 429 Too Many Requests\r\nretry-after: 0\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;

            let body = r#"{"five_hour":{"utilization":12.5}}"#;
            write_response(
                &listener,
                &format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nanthropic-organization-id: org_123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
            )
            .await;
        });

        let (payload, org_id) = fetch_usage_payload(&reqwest::Client::new(), &url, "token", &gate)
            .await
            .unwrap();
        server.await.unwrap();

        assert!(claude_payload_has_window(&payload));
        assert_eq!(org_id.as_deref(), Some("org_123"));
    }

    #[tokio::test]
    async fn records_long_rate_limit_without_retrying() {
        let gate = RateLimitGate::default();
        gate.clear();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/usage", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            write_response(
                &listener,
                "HTTP/1.1 429 Too Many Requests\r\nretry-after: 60\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;
        });

        let error = fetch_usage_payload(&reqwest::Client::new(), &url, "token", &gate)
            .await
            .unwrap_err();
        server.await.unwrap();

        assert!(matches!(
            error,
            SourceError::RateLimited {
                provider: PROVIDER_ID,
                provider_label: PROVIDER_LABEL,
            }
        ));
        assert!(gate.is_blocked(now_millis()));
    }

    #[tokio::test]
    async fn uses_profile_email_only_when_usage_identity_is_missing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let body = r#"{"account":{"email":"PROFILE@Example.COM"}}"#;
            write_response(
                &listener,
                &format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
            )
            .await;
        });
        let payload = json!({ "five_hour": { "utilization": 12.5 } });

        let snapshot = build_snapshot_from_payload(
            &reqwest::Client::new(),
            &base_url,
            &payload,
            Some("org_123".to_owned()),
            &oauth_credential(None),
            "token",
        )
        .await
        .unwrap();
        server.await.unwrap();

        let account = snapshot.account.unwrap();
        assert_eq!(account.identifier.as_deref(), Some("org_123"));
        assert_eq!(account.email.as_deref(), Some("profile@example.com"));
    }

    #[tokio::test]
    async fn rejects_empty_claude_buckets() {
        let payload = json!({
            "five_hour": null,
            "seven_day": null,
            "seven_day_opus": null,
            "seven_day_sonnet": null
        });
        let error = build_snapshot_from_payload(
            &reqwest::Client::new(),
            DEFAULT_ENDPOINT,
            &payload,
            None,
            &oauth_credential(Some("cached@example.com")),
            "token",
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            SourceError::BadPayload {
                provider: PROVIDER_ID,
                message: "no usable quota buckets",
            }
        ));
    }

    async fn write_response(listener: &tokio::net::TcpListener, response: &str) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await.unwrap();
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires local Claude CLI credentials and network access"]
    async fn live_claude_quota_returns_window_values() {
        use crate::credentials::{claude_file::ClaudeFileCredentialSource, CredentialSource};

        let credential = ClaudeFileCredentialSource
            .load(&EmptySecretStore)
            .await
            .expect("local Claude credential file should load");
        let client = reqwest::Client::builder()
            .https_only(true)
            .user_agent("AlexBar/0.1.0")
            .build()
            .expect("HTTP client should build");
        let gate = Arc::new(RateLimitGate::default());
        let snapshot = ClaudeReportSource::new(gate)
            .fetch(&client, &credential)
            .await
            .expect("Claude report endpoint should return a snapshot");

        assert!(snapshot.note.is_none());
        assert!(snapshot.metrics.iter().any(|metric| matches!(
            metric,
            ProviderMetric::Quota(quota) if quota.progress.used_percent().is_some()
        )));
    }
}
