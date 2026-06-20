use std::sync::Arc;

use serde_json::Value;

use super::{
    now_millis, number_from_keys, number_from_value, reset_millis_from_keys, urgency_from_percent,
    AccountInfo, Bucket, CountUnit, Progress, ProviderMetric, ProviderSnapshot, Quota,
    RateLimitGate, ReportSource, SourceError, ABSOLUTE_RESET_KEYS,
    RELATIVE_RESET_AFTER_SECONDS_KEYS,
};
use crate::{
    credentials::{
        zai_secret::ZaiSecretCredentialSource, CredentialMaterial, CredentialSource,
        UsageCredential,
    },
    providers::ProviderDescriptor,
};

const PROVIDER_ID: &str = "zai";
const PROVIDER_LABEL: &str = "z.ai";
const ZAI_QUOTA_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const FIVE_HOURS_MS: i64 = 5 * 60 * 60 * 1000;
const SEVEN_DAYS_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const ZAI_UNIT_FIVE_HOUR: i64 = 3;
const ZAI_UNIT_MCP: i64 = 5;
const ZAI_UNIT_WEEKLY: i64 = 6;
const SHORT_KEY: &str = "zai.tokens.5h";
const LONG_KEY: &str = "zai.mcp";

#[derive(Debug, Default)]
pub struct ZaiReportSource;

fn report_source(_gate: Arc<RateLimitGate>) -> Box<dyn ReportSource> {
    Box::<ZaiReportSource>::default()
}

fn credential_source() -> Box<dyn CredentialSource> {
    Box::<ZaiSecretCredentialSource>::default()
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: PROVIDER_ID,
    label: PROVIDER_LABEL,
    report: report_source,
    credentials: credential_source,
    short_quota_key: SHORT_KEY,
    long_quota_key: LONG_KEY,
};

#[derive(Debug, Clone, PartialEq)]
struct ZaiQuotaItem {
    kind: String,
    unit: Option<i64>,
    total: Option<f64>,
    used: Option<f64>,
    used_percent: Option<f64>,
    remaining: Option<f64>,
    resets_at: Option<i64>,
}

#[async_trait::async_trait]
impl ReportSource for ZaiReportSource {
    fn provider(&self) -> &'static str {
        PROVIDER_ID
    }

    async fn fetch(
        &self,
        client: &reqwest::Client,
        credential: &UsageCredential,
    ) -> Result<ProviderSnapshot, SourceError> {
        let CredentialMaterial::ApiKey(api_key) = &credential.material else {
            return Err(SourceError::UnsupportedCredential {
                provider: PROVIDER_ID,
                expected: "API key",
            });
        };

        fetch_from_url(client, ZAI_QUOTA_URL, credential, api_key).await
    }
}

async fn fetch_from_url(
    client: &reqwest::Client,
    url: &str,
    credential: &UsageCredential,
    api_key: &str,
) -> Result<ProviderSnapshot, SourceError> {
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, api_key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, "AlexBar/0.1.0")
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

    zai_snapshot_from_payload(
        &payload,
        credential.account_id.clone(),
        credential.email.clone(),
    )
}

fn zai_snapshot_from_payload(
    payload: &Value,
    account_id: Option<String>,
    email: Option<String>,
) -> Result<ProviderSnapshot, SourceError> {
    if payload.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "usage response was not successful",
        });
    }

    let data = payload
        .get("data")
        .and_then(Value::as_object)
        .ok_or(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "missing quota data",
        })?;
    let quota_payload =
        data.get("limits")
            .and_then(Value::as_array)
            .ok_or(SourceError::BadPayload {
                provider: PROVIDER_ID,
                message: "missing quota limits",
            })?;
    let plan = data
        .get("level")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let mut five_hour = None;
    let mut weekly = None;
    let mut mcp = None;
    for value in quota_payload {
        let Some(item) = parse_quota_item(value) else {
            continue;
        };
        match (item.kind.as_str(), item.unit) {
            ("TOKENS_LIMIT", Some(ZAI_UNIT_FIVE_HOUR)) if five_hour.is_none() => {
                five_hour = Some(zai_quota(
                    &item,
                    SHORT_KEY,
                    "5 小时",
                    CountUnit::Tokens,
                    Bucket::Rolling {
                        duration_ms: FIVE_HOURS_MS,
                        label: "5 小时".to_owned(),
                        resets_at: item.resets_at,
                    },
                ));
            }
            ("TOKENS_LIMIT", Some(ZAI_UNIT_WEEKLY)) if weekly.is_none() => {
                weekly = Some(zai_quota(
                    &item,
                    "zai.tokens.7d",
                    "7 天",
                    CountUnit::Tokens,
                    Bucket::Rolling {
                        duration_ms: SEVEN_DAYS_MS,
                        label: "7 天".to_owned(),
                        resets_at: item.resets_at,
                    },
                ));
            }
            ("TIME_LIMIT", Some(ZAI_UNIT_MCP)) if mcp.is_none() => {
                mcp = Some(zai_quota(
                    &item,
                    LONG_KEY,
                    "MCP",
                    CountUnit::Requests,
                    Bucket::OpenEnded {
                        label: "MCP".to_owned(),
                        resets_at: item.resets_at,
                    },
                ));
            }
            _ => {}
        }
    }

    let mut quotas = Vec::with_capacity(3);
    if let Some(quota) = five_hour {
        quotas.push(quota);
    }
    if let Some(quota) = weekly {
        quotas.push(quota);
    }
    if let Some(quota) = mcp {
        quotas.push(quota);
    }

    if quotas.is_empty() {
        return Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "no usable quota limits",
        });
    }

    Ok(ProviderSnapshot {
        provider: PROVIDER_ID.to_owned(),
        refreshed_at: now_millis(),
        account: account_info(account_id, email, plan),
        metrics: quotas.into_iter().map(ProviderMetric::from).collect(),
        note: None,
    })
}

fn parse_quota_item(value: &Value) -> Option<ZaiQuotaItem> {
    let object = value.as_object()?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let current_value = number_from_keys(object, &["currentValue", "current_value"]);
    let usage = number_from_keys(object, &["usage", "used"]);
    let number = number_from_value(object.get("number"));
    let explicit_total = number_from_keys(
        object,
        &["total", "totalValue", "total_value", "limit", "quota"],
    );
    let total = explicit_total.or_else(|| {
        if current_value.is_some() {
            usage.or(number)
        } else {
            number
        }
    });
    let used = current_value.or(usage);

    Some(ZaiQuotaItem {
        kind,
        unit: integer_from_value(object.get("unit")),
        total,
        used,
        used_percent: number_from_keys(
            object,
            &[
                "percentage",
                "used_percent",
                "usedPercent",
                "usage_percent",
                "usagePercentage",
            ],
        ),
        remaining: number_from_keys(object, &["remaining", "remain", "available"]),
        resets_at: reset_millis_from_keys(
            object,
            ABSOLUTE_RESET_KEYS,
            RELATIVE_RESET_AFTER_SECONDS_KEYS,
            now_millis(),
        ),
    })
}

fn zai_quota(
    item: &ZaiQuotaItem,
    key: &str,
    display_name: &str,
    unit: CountUnit,
    bucket: Bucket,
) -> Quota {
    let progress = Progress::counted(
        item.used,
        item.total,
        item.remaining,
        item.used_percent,
        unit,
    );
    let urgency = urgency_from_percent(progress.used_percent(), false);
    Quota {
        key: key.to_owned(),
        display_name: display_name.to_owned(),
        bucket,
        progress,
        urgency,
    }
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

fn integer_from_value(value: Option<&Value>) -> Option<i64> {
    let parsed = number_from_value(value)?;
    if parsed.fract() != 0.0 || parsed < i64::MIN as f64 || parsed > i64::MAX as f64 {
        return None;
    }
    Some(parsed as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn quota_at(snapshot: &ProviderSnapshot, index: usize) -> &Quota {
        match &snapshot.metrics[index] {
            ProviderMetric::Quota(quota) => quota,
            ProviderMetric::Balance(_) => panic!("expected quota metric"),
        }
    }

    #[test]
    fn descriptor_points_to_zai_quotas() {
        assert_eq!(DESCRIPTOR.id, "zai");
        assert_eq!(DESCRIPTOR.short_quota_key, SHORT_KEY);
        assert_eq!(DESCRIPTOR.long_quota_key, LONG_KEY);
    }

    #[test]
    fn parses_quota_payload_into_five_hour_weekly_and_mcp_quotas() {
        let payload = json!({
            "success": true,
            "code": 200,
            "data": {
                "level": "pro",
                "limits": [
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 3,
                        "usage": 1_000_000,
                        "currentValue": 250_000,
                        "percentage": 25,
                        "remaining": 750_000,
                        "nextResetTime": 1_778_976_000
                    },
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 6,
                        "usage": 2_000_000,
                        "currentValue": 1_000_000,
                        "percentage": 50,
                        "remaining": 1_000_000,
                        "nextResetTime": 1_779_580_800_000_i64
                    },
                    {
                        "type": "TIME_LIMIT",
                        "unit": 5,
                        "usage": "1000",
                        "currentValue": "950",
                        "percentage": "95",
                        "remaining": "50",
                        "nextResetTime": 1_778_976_000_000_i64
                    }
                ]
            }
        });

        let snapshot = zai_snapshot_from_payload(
            &payload,
            Some("acct".to_owned()),
            Some("user@example.com".to_owned()),
        )
        .unwrap();

        assert_eq!(snapshot.provider, PROVIDER_ID);
        assert_eq!(snapshot.metrics.len(), 3);
        assert_eq!(
            snapshot.account.as_ref().unwrap().plan.as_deref(),
            Some("pro")
        );

        let five_hour = quota_at(&snapshot, 0);
        assert_eq!(five_hour.key, SHORT_KEY);
        assert_eq!(five_hour.display_name, "5 小时");
        assert!(matches!(
            five_hour.progress,
            Progress::Counted {
                unit: CountUnit::Tokens,
                used_percent: Some(25.0),
                ..
            }
        ));
        assert!(matches!(
            five_hour.bucket,
            Bucket::Rolling {
                duration_ms: FIVE_HOURS_MS,
                resets_at: Some(1_778_976_000_000),
                ..
            }
        ));

        let weekly = quota_at(&snapshot, 1);
        assert_eq!(weekly.key, "zai.tokens.7d");
        assert_eq!(weekly.display_name, "7 天");
        assert!(matches!(
            weekly.progress,
            Progress::Counted {
                unit: CountUnit::Tokens,
                ..
            }
        ));
        assert!(matches!(
            weekly.bucket,
            Bucket::Rolling {
                duration_ms: SEVEN_DAYS_MS,
                ..
            }
        ));

        let mcp = quota_at(&snapshot, 2);
        assert_eq!(mcp.key, LONG_KEY);
        assert_eq!(mcp.display_name, "MCP");
        assert!(matches!(
            mcp.progress,
            Progress::Counted {
                unit: CountUnit::Requests,
                ..
            }
        ));
        assert!(matches!(mcp.bucket, Bucket::OpenEnded { .. }));
        assert_eq!(mcp.urgency, super::super::Urgency::Tense);
    }

    #[test]
    fn parses_snake_case_quota_aliases() {
        let payload = json!({
            "success": true,
            "code": 200,
            "data": {
                "limits": [
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 3,
                        "usage": 1_000,
                        "current_value": 250,
                        "used_percent": "25",
                        "remaining": 750,
                        "next_reset_time": 1_778_976_000_000_i64
                    }
                ]
            }
        });

        let snapshot = zai_snapshot_from_payload(&payload, None, None).unwrap();
        assert_eq!(snapshot.metrics.len(), 1);
        assert_eq!(quota_at(&snapshot, 0).key, SHORT_KEY);
        assert!(matches!(
            quota_at(&snapshot, 0).progress,
            Progress::Counted {
                used: Some(250.0),
                total: Some(1000.0),
                remaining: Some(750.0),
                used_percent: Some(25.0),
                unit: CountUnit::Tokens
            }
        ));
        assert!(matches!(
            quota_at(&snapshot, 0).bucket,
            Bucket::Rolling {
                resets_at: Some(1_778_976_000_000),
                ..
            }
        ));
    }

    #[test]
    fn parses_usage_as_used_when_number_is_total() {
        let payload = json!({
            "success": true,
            "code": 200,
            "data": {
                "limits": [
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 3,
                        "number": 1_000,
                        "usage": 250,
                        "remaining": 750,
                        "reset_time": "2026-05-17T00:00:00Z"
                    }
                ]
            }
        });

        let snapshot = zai_snapshot_from_payload(&payload, None, None).unwrap();
        assert_eq!(quota_at(&snapshot, 0).display_name, "5 小时");
        assert!(matches!(
            quota_at(&snapshot, 0).progress,
            Progress::Counted {
                used: Some(250.0),
                total: Some(1000.0),
                remaining: Some(750.0),
                used_percent: Some(25.0),
                ..
            }
        ));
        assert!(matches!(
            quota_at(&snapshot, 0).bucket,
            Bucket::Rolling {
                resets_at: Some(1_778_976_000_000),
                ..
            }
        ));
    }

    #[test]
    fn omits_weekly_when_zai_plan_has_no_weekly_bucket() {
        let payload = json!({
            "success": true,
            "code": 200,
            "data": {
                "level": "pro",
                "limits": [
                    {
                        "type": "TIME_LIMIT",
                        "unit": 5,
                        "usage": 1000,
                        "currentValue": 1000,
                        "remaining": 0,
                        "percentage": 100,
                        "nextResetTime": 1_777_391_696_996_i64
                    },
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 3,
                        "number": 5,
                        "percentage": 1,
                        "nextResetTime": 1_776_190_484_314_i64
                    },
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 4,
                        "number": 1,
                        "percentage": 70,
                        "nextResetTime": 1_776_276_884_314_i64
                    }
                ]
            }
        });

        let snapshot = zai_snapshot_from_payload(&payload, None, None).unwrap();
        let keys = snapshot
            .metrics
            .iter()
            .filter_map(|metric| match metric {
                ProviderMetric::Quota(quota) => Some(quota.key.as_str()),
                ProviderMetric::Balance(_) => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(keys, vec![SHORT_KEY, LONG_KEY]);
        assert!(matches!(
            quota_at(&snapshot, 0).bucket,
            Bucket::Rolling { .. }
        ));
        assert!(matches!(
            quota_at(&snapshot, 1).bucket,
            Bucket::OpenEnded { .. }
        ));
    }
}
