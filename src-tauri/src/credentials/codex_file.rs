use serde::Deserialize;
use serde_json::Value;

use super::{
    home_path, normalize_email, parse_epoch_millis, read_json_file, required_string,
    CredentialError, CredentialMaterial, CredentialSource, SecretStore, UsageCredential,
};
use crate::usage::codex::read_chatgpt_claims;

#[derive(Debug, Default)]
pub struct CodexFileCredentialSource;

#[derive(Debug, Deserialize)]
struct CodexAuthFile {
    tokens: Option<CodexTokens>,
}

#[derive(Debug, Deserialize)]
struct CodexTokens {
    access_token: Option<String>,
    id_token: Option<String>,
    refresh_token: Option<String>,
    #[serde(default)]
    expiry_date: Option<Value>,
    #[serde(default)]
    expires_at: Option<Value>,
}

#[async_trait::async_trait]
impl CredentialSource for CodexFileCredentialSource {
    fn provider(&self) -> &'static str {
        "openai-codex"
    }

    async fn load(&self, _secrets: &dyn SecretStore) -> Result<UsageCredential, CredentialError> {
        let path = home_path(&[".codex", "auth.json"])?;
        let file: CodexAuthFile = read_json_file(path.clone())?;
        let tokens = file.tokens.ok_or_else(|| CredentialError::MissingField {
            path: path.clone(),
            field: "tokens",
        })?;

        let access_token = required_string(&path, tokens.access_token, "tokens.access_token")?;
        let id_token = tokens
            .id_token
            .and_then(|value| required_optional_string(value));
        let refresh_token = tokens
            .refresh_token
            .and_then(|value| required_optional_string(value));
        let expires_at = parse_epoch_millis(tokens.expiry_date.as_ref())
            .or_else(|| parse_epoch_millis(tokens.expires_at.as_ref()));
        let id_claims = id_token.as_deref().and_then(read_chatgpt_claims);
        let access_claims = read_chatgpt_claims(&access_token);
        let account_id = id_claims
            .as_ref()
            .and_then(|claims| claims.account_id.clone())
            .or_else(|| {
                access_claims
                    .as_ref()
                    .and_then(|claims| claims.account_id.clone())
            });
        let email = normalize_email(
            id_claims
                .as_ref()
                .and_then(|claims| claims.email.clone())
                .or_else(|| {
                    access_claims
                        .as_ref()
                        .and_then(|claims| claims.email.clone())
                }),
        );

        Ok(UsageCredential {
            provider: self.provider().to_owned(),
            material: CredentialMaterial::Oauth {
                access_token,
                id_token,
                refresh_token,
            },
            expires_at,
            account_id,
            email,
        })
    }
}

fn required_optional_string(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}
