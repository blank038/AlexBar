use std::sync::Arc;

use serde_json::Value;

use super::{
    epoch_millis_from_value, now_millis, number_from_value, urgency_from_percent, AccountInfo,
    Bucket, CountUnit, Progress, ProviderMetric, ProviderSnapshot, Quota, RateLimitGate,
    ReportSource, SourceError, Urgency,
};
use crate::{
    credentials::{
        minimax_secret::MinimaxSecretCredentialSource, CredentialMaterial, CredentialSource,
        UsageCredential,
    },
    providers::ProviderDescriptor,
};

const PROVIDER_ID: &str = "minimax";
const PROVIDER_LABEL: &str = "MiniMax";
const MINIMAX_REMAINS_URL: &str = "https://api.minimaxi.com/v1/token_plan/remains";
const SHORT_KEY: &str = "minimax.general.current";
const LONG_KEY: &str = "minimax.general.weekly";
const STATUS_EXHAUSTED: i64 = 2;
const STATUS_UNLIMITED: i64 = 3;

#[derive(Debug, Default)]
pub struct MinimaxReportSource;

fn report_source(_gate: Arc<RateLimitGate>) -> Box<dyn ReportSource> {
    Box::<MinimaxReportSource>::default()
}

fn credential_source() -> Box<dyn CredentialSource> {
    Box::<MinimaxSecretCredentialSource>::default()
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: PROVIDER_ID,
    label: PROVIDER_LABEL,
    report: report_source,
    credentials: credential_source,
    short_quota_key: SHORT_KEY,
    long_quota_key: LONG_KEY,
};

#[async_trait::async_trait]
impl ReportSource for MinimaxReportSource {
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

        fetch_from_url(client, MINIMAX_REMAINS_URL, credential, api_key).await
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
        .bearer_auth(api_key)
        .header(reqwest::header::ACCEPT, "application/json")
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

    minimax_snapshot_from_payload(
        &payload,
        credential.account_id.clone(),
        credential.email.clone(),
    )
}

fn minimax_snapshot_from_payload(
    payload: &Value,
    account_id: Option<String>,
    email: Option<String>,
) -> Result<ProviderSnapshot, SourceError> {
    let object = payload.as_object().ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message: "missing quota object",
    })?;
    validate_base_resp(object)?;

    let model_remains = object
        .get("model_remains")
        .and_then(Value::as_array)
        .ok_or(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "missing model_remains",
        })?;

    let mut metrics = Vec::with_capacity(model_remains.len() * 2);
    for value in model_remains {
        let mut quotas = parse_model_remain(value)?;
        metrics.append(&mut quotas);
    }
    if metrics.is_empty() {
        return Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "empty model_remains",
        });
    }

    Ok(ProviderSnapshot {
        provider: PROVIDER_ID.to_owned(),
        refreshed_at: now_millis(),
        account: account_info(account_id, email),
        metrics,
        note: None,
    })
}

fn validate_base_resp(object: &serde_json::Map<String, Value>) -> Result<(), SourceError> {
    let Some(base_resp) = object.get("base_resp").and_then(Value::as_object) else {
        return Ok(());
    };
    if integer_from_value(base_resp.get("status_code")) == Some(0) {
        Ok(())
    } else {
        Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "quota response was not successful",
        })
    }
}

fn parse_model_remain(value: &Value) -> Result<Vec<ProviderMetric>, SourceError> {
    let object = value.as_object().ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message: "invalid model remain",
    })?;
    let model_name = object
        .get("model_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "missing model_name",
        })?;
    let display_model_name = display_model_name(model_name);

    Ok(vec![
        ProviderMetric::from(interval_quota(object, model_name, &display_model_name)?),
        ProviderMetric::from(weekly_quota(object, model_name, &display_model_name)?),
    ])
}

fn interval_quota(
    object: &serde_json::Map<String, Value>,
    model_name: &str,
    display_model_name: &str,
) -> Result<Quota, SourceError> {
    let total = required_non_negative_number(
        object.get("current_interval_total_count"),
        "missing current_interval_total_count",
    )?;
    let used = required_non_negative_number(
        object.get("current_interval_usage_count"),
        "missing current_interval_usage_count",
    )?;
    let remaining = remaining_count(used, total);
    let status = integer_from_value(object.get("current_interval_status"));
    let progress = Progress::counted(
        Some(used),
        Some(total),
        Some(remaining),
        used_percent_from_remaining(object.get("current_interval_remaining_percent")),
        CountUnit::Requests,
    );
    let urgency = if status == Some(STATUS_EXHAUSTED) {
        Urgency::Capped
    } else {
        urgency_from_percent(progress.used_percent(), false)
    };

    Ok(Quota {
        key: format!("minimax.{model_name}.current"),
        display_name: format!("{display_model_name} 当前窗口"),
        bucket: Bucket::OpenEnded {
            label: format!("{display_model_name} 当前窗口"),
            resets_at: required_epoch_millis(object.get("end_time"), "missing end_time")?,
        },
        progress,
        urgency,
    })
}

fn weekly_quota(
    object: &serde_json::Map<String, Value>,
    model_name: &str,
    display_model_name: &str,
) -> Result<Quota, SourceError> {
    let total = required_non_negative_number(
        object.get("current_weekly_total_count"),
        "missing current_weekly_total_count",
    )?;
    let used = required_non_negative_number(
        object.get("current_weekly_usage_count"),
        "missing current_weekly_usage_count",
    )?;
    let status = integer_from_value(object.get("current_weekly_status"));
    let unlimited = status == Some(STATUS_UNLIMITED);
    let remaining = remaining_count(used, total);
    let progress = if unlimited {
        Progress::counted(Some(used), None, None, Some(0.0), CountUnit::Requests)
    } else {
        Progress::counted(
            Some(used),
            Some(total),
            Some(remaining),
            used_percent_from_remaining(object.get("current_weekly_remaining_percent")),
            CountUnit::Requests,
        )
    };
    let urgency = if status == Some(STATUS_EXHAUSTED) {
        Urgency::Capped
    } else {
        urgency_from_percent(progress.used_percent(), false)
    };

    Ok(Quota {
        key: format!("minimax.{model_name}.weekly"),
        display_name: format!("{display_model_name} 周窗口"),
        bucket: Bucket::OpenEnded {
            label: format!("{display_model_name} 周窗口"),
            resets_at: required_epoch_millis(
                object.get("weekly_end_time"),
                "missing weekly_end_time",
            )?,
        },
        progress,
        urgency,
    })
}

fn display_model_name(model_name: &str) -> String {
    match model_name {
        "general" => "通用".to_owned(),
        "video" => "视频".to_owned(),
        value => value.to_owned(),
    }
}

fn remaining_count(used: f64, total: f64) -> f64 {
    (total - used).max(0.0)
}

fn used_percent_from_remaining(value: Option<&Value>) -> Option<f64> {
    let remaining = number_from_value(value)?;
    let remaining_percent = if remaining > 0.0 && remaining < 1.0 {
        remaining * 100.0
    } else {
        remaining
    };
    Some((100.0 - remaining_percent).clamp(0.0, 100.0))
}

fn required_non_negative_number(
    value: Option<&Value>,
    message: &'static str,
) -> Result<f64, SourceError> {
    let parsed = number_from_value(value).ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message,
    })?;
    if parsed >= 0.0 {
        Ok(parsed)
    } else {
        Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "invalid quota count",
        })
    }
}

fn required_epoch_millis(
    value: Option<&Value>,
    message: &'static str,
) -> Result<Option<i64>, SourceError> {
    epoch_millis_from_value(value)
        .map(Some)
        .ok_or(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message,
        })
}

fn integer_from_value(value: Option<&Value>) -> Option<i64> {
    let parsed = number_from_value(value)?;
    if parsed.fract() != 0.0 || parsed < i64::MIN as f64 || parsed > i64::MAX as f64 {
        return None;
    }
    Some(parsed as i64)
}

fn account_info(identifier: Option<String>, email: Option<String>) -> Option<AccountInfo> {
    if identifier.is_none() && email.is_none() {
        None
    } else {
        Some(AccountInfo {
            identifier,
            email,
            plan: None,
        })
    }
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
    fn parses_minimax_remains_payload() {
        let payload = json!({
            "base_resp": {
                "status_code": 0,
                "status_msg": ""
            },
            "model_remains": [
                {
                    "model_name": "general",
                    "start_time": 1_778_976_000_000_i64,
                    "end_time": 1_778_994_000_000_i64,
                    "remains_time": 18_000_000,
                    "current_interval_total_count": 1000,
                    "current_interval_usage_count": 250,
                    "current_interval_remaining_percent": 75,
                    "current_interval_status": 1,
                    "current_weekly_total_count": 5000,
                    "current_weekly_usage_count": 4750,
                    "current_weekly_remaining_percent": 5,
                    "current_weekly_status": 1,
                    "weekly_start_time": 1_778_976_000_000_i64,
                    "weekly_end_time": 1_779_580_800_000_i64,
                    "weekly_remains_time": 604_800_000
                }
            ]
        });

        let snapshot = minimax_snapshot_from_payload(&payload, None, None).unwrap();
        assert_eq!(snapshot.provider, PROVIDER_ID);
        assert_eq!(snapshot.metrics.len(), 2);

        let current = quota_at(&snapshot, 0);
        assert_eq!(current.key, SHORT_KEY);
        assert_eq!(current.display_name, "通用 当前窗口");
        assert!(matches!(
            current.progress,
            Progress::Counted {
                used: Some(250.0),
                total: Some(1000.0),
                remaining: Some(750.0),
                used_percent: Some(25.0),
                unit: CountUnit::Requests
            }
        ));
        assert!(matches!(
            current.bucket,
            Bucket::OpenEnded {
                resets_at: Some(1_778_994_000_000),
                ..
            }
        ));

        let weekly = quota_at(&snapshot, 1);
        assert_eq!(weekly.key, LONG_KEY);
        assert_eq!(weekly.display_name, "通用 周窗口");
        assert_eq!(weekly.urgency, Urgency::Tense);
        assert!(matches!(
            weekly.progress,
            Progress::Counted {
                used: Some(4750.0),
                total: Some(5000.0),
                remaining: Some(250.0),
                used_percent: Some(95.0),
                unit: CountUnit::Requests
            }
        ));
    }

    #[test]
    fn parses_minimax_unlimited_weekly_status() {
        let payload = json!({
            "model_remains": [
                {
                    "model_name": "video",
                    "end_time": 1_778_994_000_000_i64,
                    "current_interval_total_count": 100,
                    "current_interval_usage_count": 100,
                    "current_interval_remaining_percent": 0,
                    "current_interval_status": 2,
                    "current_weekly_total_count": 0,
                    "current_weekly_usage_count": 12,
                    "current_weekly_remaining_percent": 100,
                    "current_weekly_status": 3,
                    "weekly_end_time": 1_779_580_800_000_i64
                }
            ]
        });

        let snapshot = minimax_snapshot_from_payload(&payload, None, None).unwrap();
        let current = quota_at(&snapshot, 0);
        assert_eq!(current.display_name, "视频 当前窗口");
        assert_eq!(current.urgency, Urgency::Capped);

        let weekly = quota_at(&snapshot, 1);
        assert_eq!(weekly.display_name, "视频 周窗口");
        assert_eq!(weekly.urgency, Urgency::Calm);
        assert!(matches!(
            weekly.progress,
            Progress::Counted {
                used: Some(12.0),
                total: None,
                remaining: None,
                used_percent: Some(0.0),
                unit: CountUnit::Requests
            }
        ));
    }
}
