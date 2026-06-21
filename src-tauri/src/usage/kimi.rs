use std::sync::Arc;

use serde_json::Value;

use super::{
    now_millis, number_from_value, AccountInfo, Balance, ProviderMetric, ProviderSnapshot,
    RateLimitGate, ReportSource, SourceError, Urgency,
};
use crate::{
    credentials::{
        kimi_secret::KimiSecretCredentialSource, CredentialMaterial, CredentialSource,
        UsageCredential,
    },
    providers::ProviderDescriptor,
};

const PROVIDER_ID: &str = "kimi";
const PROVIDER_LABEL: &str = "Kimi";
const KIMI_BALANCE_URL: &str = "https://api.moonshot.cn/v1/users/me/balance";

#[derive(Debug, Default)]
pub struct KimiReportSource;

fn report_source(_gate: Arc<RateLimitGate>) -> Box<dyn ReportSource> {
    Box::<KimiReportSource>::default()
}

fn credential_source() -> Box<dyn CredentialSource> {
    Box::<KimiSecretCredentialSource>::default()
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
impl ReportSource for KimiReportSource {
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

        fetch_from_url(client, KIMI_BALANCE_URL, credential, api_key).await
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

    kimi_snapshot_from_payload(
        &payload,
        credential.account_id.clone(),
        credential.email.clone(),
    )
}

fn kimi_snapshot_from_payload(
    payload: &Value,
    account_id: Option<String>,
    email: Option<String>,
) -> Result<ProviderSnapshot, SourceError> {
    let object = payload.as_object().ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message: "missing balance object",
    })?;
    if object.get("status").and_then(Value::as_bool) != Some(true)
        || number_from_value(object.get("code")) != Some(0.0)
    {
        return Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "balance response was not successful",
        });
    }

    let data = object
        .get("data")
        .and_then(Value::as_object)
        .ok_or(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "missing balance data",
        })?;
    let available = required_amount(data.get("available_balance"), "missing available_balance")?;
    let voucher =
        required_non_negative_amount(data.get("voucher_balance"), "missing voucher_balance")?;
    let cash = required_amount(data.get("cash_balance"), "missing cash_balance")?;
    let is_available = available > 0.0;

    Ok(ProviderSnapshot {
        provider: PROVIDER_ID.to_owned(),
        refreshed_at: now_millis(),
        account: account_info(account_id, email),
        metrics: vec![ProviderMetric::from(Balance {
            key: "kimi.balance.CNY".to_owned(),
            display_name: "CNY 余额".to_owned(),
            amount: available,
            currency: "CNY".to_owned(),
            granted: Some(voucher),
            topped_up: Some(cash),
            is_available,
            urgency: if is_available {
                Urgency::Calm
            } else {
                Urgency::Capped
            },
        })],
        note: None,
    })
}

fn required_amount(value: Option<&Value>, message: &'static str) -> Result<f64, SourceError> {
    number_from_value(value).ok_or(SourceError::BadPayload {
        provider: PROVIDER_ID,
        message,
    })
}

fn required_non_negative_amount(
    value: Option<&Value>,
    message: &'static str,
) -> Result<f64, SourceError> {
    let amount = required_amount(value, message)?;
    if amount >= 0.0 {
        Ok(amount)
    } else {
        Err(SourceError::BadPayload {
            provider: PROVIDER_ID,
            message: "invalid balance amount",
        })
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
    fn parses_kimi_balance_payload() {
        let payload = json!({
            "code": 0,
            "status": true,
            "scode": "0x0",
            "data": {
                "available_balance": 49.58894,
                "voucher_balance": 46.58893,
                "cash_balance": 3.00001
            }
        });

        let snapshot = kimi_snapshot_from_payload(&payload, None, None).unwrap();
        assert_eq!(snapshot.provider, PROVIDER_ID);
        assert_eq!(snapshot.metrics.len(), 1);

        let ProviderMetric::Balance(balance) = &snapshot.metrics[0] else {
            panic!("Kimi balance should produce a balance metric");
        };
        assert_eq!(balance.key, "kimi.balance.CNY");
        assert_eq!(balance.display_name, "CNY 余额");
        assert_eq!(balance.amount, 49.58894);
        assert_eq!(balance.currency, "CNY");
        assert_eq!(balance.granted, Some(46.58893));
        assert_eq!(balance.topped_up, Some(3.00001));
        assert!(balance.is_available);
        assert_eq!(balance.urgency, Urgency::Calm);
    }

    #[test]
    fn unavailable_kimi_balance_is_capped() {
        let payload = json!({
            "code": 0,
            "status": true,
            "data": {
                "available_balance": 0,
                "voucher_balance": 0,
                "cash_balance": -1.5
            }
        });

        let snapshot = kimi_snapshot_from_payload(&payload, None, None).unwrap();
        let ProviderMetric::Balance(balance) = &snapshot.metrics[0] else {
            panic!("Kimi balance should produce a balance metric");
        };
        assert!(!balance.is_available);
        assert_eq!(balance.urgency, Urgency::Capped);
        assert_eq!(balance.topped_up, Some(-1.5));
    }
}
