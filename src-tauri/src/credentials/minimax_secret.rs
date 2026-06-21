use super::{CredentialError, CredentialMaterial, CredentialSource, SecretStore, UsageCredential};

const PROVIDER_ID: &str = "minimax";
const API_KEY_FIELD: &str = "api_key";

#[derive(Debug, Default)]
pub struct MinimaxSecretCredentialSource;

#[async_trait::async_trait]
impl CredentialSource for MinimaxSecretCredentialSource {
    fn provider(&self) -> &'static str {
        PROVIDER_ID
    }

    async fn load(&self, secrets: &dyn SecretStore) -> Result<UsageCredential, CredentialError> {
        let api_key = secrets.get_secret(PROVIDER_ID, API_KEY_FIELD).ok_or(
            CredentialError::MissingSecret {
                provider: PROVIDER_ID,
                field: API_KEY_FIELD,
            },
        )?;

        Ok(UsageCredential {
            provider: PROVIDER_ID.to_owned(),
            material: CredentialMaterial::ApiKey(api_key),
            expires_at: None,
            account_id: None,
            email: None,
        })
    }
}
