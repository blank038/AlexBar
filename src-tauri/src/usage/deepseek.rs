use std::sync::Arc;

use serde_json::Value;

use super::{
    now_millis, number_from_value, AccountInfo, Balance, ProviderMetric, ProviderSnapshot,
    RateLimitGate, ReportSource, SourceError, Urgency,
};
use crate::{
    credentials::{
        deepseek_secret::DeepSeekSecretCredentialSource, CredentialMaterial, CredentialSource,
        UsageCredential,
    },
    providers::ProviderDescriptor,
};

const PROVIDER_ID: &str = "deepseek";
const PROVIDER_LABEL: &str = "DeepSeek";
const DEEPSEEK_BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

#[derive(Debug, Default)]
pub struct DeepSeekReportSource;

fn report_source(_gate: Arc<RateLimitGate>) -> Box<dyn ReportSource> {
    Box::<DeepSeekReportSource>::default()
}

fn credential_source() -> Box<dyn CredentialSource> {
    Box::<DeepSeekSecretCredentialSource>::default()
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    id: PROVIDER_ID,
    label: PROVIDER_LABEL,
    report: report_source,
    credentials: credential_source,
    short_quota_key: "",
    long_quota_key: "",
};

#[async_trait::async_trait]
impl ReportSource for DeepSeekReportSource {
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

        fetch_from_url(client, DEEPSEEK_BALANCE_URL, credential, api_key).await
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

    deepseek_snapshot_from_payload(
        &payload,
        credential.account_id.clone(),
        credential.email.clone(),
    )
}

fn deepseek_snapshot_from_payload(
    payload: &Value,
    account_id: Option<String>,
    email: Option<String>,
) -> Result<ProviderSnapshot, SourceError> {
    let object = payload.as_object().ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message: "missing balance object",
    })?;
    let is_available =
        object
            .get("is_available")
            .and_then(Value::as_bool)
            .ok_or(SourceError::BadPayload {
                provider: PROVIDER_ID,
                message: "missing is_available",
            })?;
    let balance_infos = object
        .get("balance_infos")
        .and_then(Value::as_array)
        .ok_or(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "missing balance_infos",
        })?;

    let mut metrics = Vec::with_capacity(balance_infos.len());
    for value in balance_infos {
        metrics.push(ProviderMetric::from(parse_balance(value, is_available)?));
    }
    if metrics.is_empty() {
        return Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "empty balance_infos",
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

fn parse_balance(value: &Value, is_available: bool) -> Result<Balance, SourceError> {
    let object = value.as_object().ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message: "invalid balance info",
    })?;
    let currency = object
        .get("currency")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_uppercase())
        .ok_or(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "missing balance currency",
        })?;
    let amount = required_amount(object.get("total_balance"), "missing total_balance")?;
    let granted = optional_amount(object.get("granted_balance"))?;
    let topped_up = optional_amount(object.get("topped_up_balance"))?;
    let urgency = if is_available {
        Urgency::Calm
    } else {
        Urgency::Capped
    };

    Ok(Balance {
        key: format!("deepseek.balance.{currency}"),
        display_name: format!("{currency} 余额"),
        amount,
        currency,
        granted,
        topped_up,
        is_available,
        urgency,
    })
}

fn required_amount(value: Option<&Value>, message: &'static str) -> Result<f64, SourceError> {
    optional_amount(value)?.ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message,
    })
}

fn optional_amount(value: Option<&Value>) -> Result<Option<f64>, SourceError> {
    match value {
        Some(value) => number_from_value(Some(value))
            .filter(|amount| *amount >= 0.0)
            .map(Some)
            .ok_or(SourceError::BadPayload {
                provider: PROVIDER_ID,
                message: "invalid balance amount",
            }),
        None => Ok(None),
    }
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

    #[test]
    fn parses_balance_payload_into_currency_metrics() {
        let payload = json!({
            "is_available": true,
            "balance_infos": [
                {
                    "currency": "CNY",
                    "total_balance": "110.00",
                    "granted_balance": "10.00",
                    "topped_up_balance": "100.00"
                }
            ]
        });

        let snapshot = deepseek_snapshot_from_payload(&payload, None, None).unwrap();
        assert_eq!(snapshot.provider, "deepseek");
        assert_eq!(snapshot.metrics.len(), 1);

        let ProviderMetric::Balance(balance) = &snapshot.metrics[0] else {
            panic!("DeepSeek balance should produce a balance metric");
        };
        assert_eq!(balance.key, "deepseek.balance.CNY");
        assert_eq!(balance.display_name, "CNY 余额");
        assert_eq!(balance.amount, 110.0);
        assert_eq!(balance.currency, "CNY");
        assert_eq!(balance.granted, Some(10.0));
        assert_eq!(balance.topped_up, Some(100.0));
        assert!(balance.is_available);
        assert_eq!(balance.urgency, Urgency::Calm);
    }

    #[test]
    fn unavailable_balance_is_capped() {
        let payload = json!({
            "is_available": false,
            "balance_infos": [
                {
                    "currency": "USD",
                    "total_balance": "0.00",
                    "granted_balance": "0.00",
                    "topped_up_balance": "0.00"
                }
            ]
        });

        let snapshot = deepseek_snapshot_from_payload(&payload, None, None).unwrap();
        let ProviderMetric::Balance(balance) = &snapshot.metrics[0] else {
            panic!("DeepSeek balance should produce a balance metric");
        };
        assert_eq!(balance.urgency, Urgency::Capped);
        assert!(!balance.is_available);
    }
}
