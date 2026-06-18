use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::Value;

use super::{
    home_path, normalize_email, parse_epoch_millis, read_json_file, required_string,
    CredentialError, CredentialMaterial, CredentialSource, SecretStore, UsageCredential,
};

use crate::usage::now_millis;

pub(crate) const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const CREDENTIAL_PATH: &[&str] = &[".claude", ".credentials.json"];

#[derive(Debug, Default)]
pub struct ClaudeFileCredentialSource;

#[derive(Debug, Deserialize)]
struct ClaudeCredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOauth>,
}

#[derive(Debug, Deserialize)]
struct ClaudeOauth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<Value>,
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "accountId", default)]
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeRefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

#[derive(Debug)]
struct RefreshedClaudeTokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

#[async_trait::async_trait]
impl CredentialSource for ClaudeFileCredentialSource {
    fn provider(&self) -> &'static str {
        "anthropic"
    }

    async fn load(&self, _secrets: &dyn SecretStore) -> Result<UsageCredential, CredentialError> {
        let path = credentials_path()?;
        load_credential_from_path(self.provider(), &path)
    }

    async fn refresh(
        &self,
        client: &reqwest::Client,
        credential: &UsageCredential,
    ) -> Result<Option<UsageCredential>, CredentialError> {
        let now_ms = now_millis();
        if !credential.needs_refresh_at(now_ms, REFRESH_SKEW_MS) {
            return Ok(None);
        }

        let Some(refresh_token) = credential.material.refresh_token() else {
            return Ok(None);
        };

        let response = client
            .post(TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await
            .map_err(|source| CredentialError::RefreshNetwork {
                provider: self.provider(),
                source,
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .map_err(|source| CredentialError::RefreshNetwork {
                    provider: self.provider(),
                    source,
                })?;
            return Err(CredentialError::RefreshHttp {
                provider: self.provider(),
                status: status.as_u16(),
                body: refresh_error_body(body),
            });
        }

        let tokens = response
            .json::<ClaudeRefreshResponse>()
            .await
            .map_err(|source| CredentialError::RefreshDecode {
                provider: self.provider(),
                source,
            })?
            .into_tokens(self.provider(), now_millis())?;
        let path = credentials_path()?;
        let mut file: Value = read_json_file(path.clone())?;
        if !claude_file_needs_refresh_at(&path, &file, now_millis(), REFRESH_SKEW_MS)? {
            return load_credential_from_path(self.provider(), &path).map(Some);
        }

        apply_refresh_tokens(&path, &mut file, &tokens)?;
        write_json_file_atomic(&path, &file)?;

        Ok(Some(UsageCredential {
            provider: self.provider().to_owned(),
            material: CredentialMaterial::Oauth {
                access_token: tokens.access_token,
                id_token: None,
                refresh_token: Some(tokens.refresh_token),
            },
            expires_at: Some(tokens.expires_at),
            account_id: credential.account_id.clone(),
            email: credential.email.clone(),
        }))
    }
}

fn required_optional_string(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn credentials_path() -> Result<PathBuf, CredentialError> {
    home_path(CREDENTIAL_PATH)
}

fn load_credential_from_path(
    provider: &'static str,
    path: &Path,
) -> Result<UsageCredential, CredentialError> {
    let file: ClaudeCredentialsFile = read_json_file(path.to_path_buf())?;
    let oauth = file
        .claude_ai_oauth
        .ok_or_else(|| CredentialError::MissingField {
            path: path.to_path_buf(),
            field: "claudeAiOauth",
        })?;

    let access_token = required_string(path, oauth.access_token, "claudeAiOauth.accessToken")?;
    let refresh_token = oauth.refresh_token.and_then(required_optional_string);
    let expires_at = parse_epoch_millis(oauth.expires_at.as_ref());
    let account_id = oauth.account_id.and_then(required_optional_string);
    let email = normalize_email(oauth.email);

    Ok(UsageCredential {
        provider: provider.to_owned(),
        material: CredentialMaterial::Oauth {
            access_token,
            id_token: None,
            refresh_token,
        },
        expires_at,
        account_id,
        email,
    })
}

impl ClaudeRefreshResponse {
    fn into_tokens(
        self,
        provider: &'static str,
        now_ms: i64,
    ) -> Result<RefreshedClaudeTokens, CredentialError> {
        if self.access_token.trim().is_empty() {
            return Err(CredentialError::RefreshInvalidResponse {
                provider,
                field: "access_token",
            });
        }
        if self.refresh_token.trim().is_empty() {
            return Err(CredentialError::RefreshInvalidResponse {
                provider,
                field: "refresh_token",
            });
        }
        let expires_in_ms = self
            .expires_in
            .checked_mul(1000)
            .filter(|value| *value > 0)
            .ok_or(CredentialError::RefreshInvalidResponse {
                provider,
                field: "expires_in",
            })?;
        let expires_at =
            now_ms
                .checked_add(expires_in_ms)
                .ok_or(CredentialError::RefreshInvalidResponse {
                    provider,
                    field: "expires_in",
                })?;

        Ok(RefreshedClaudeTokens {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at,
        })
    }
}

fn claude_file_needs_refresh_at(
    path: &Path,
    file: &Value,
    now_ms: i64,
    skew_ms: i64,
) -> Result<bool, CredentialError> {
    let oauth = claude_oauth_object(path, file)?;
    Ok(parse_epoch_millis(oauth.get("expiresAt"))
        .is_some_and(|expires_at| expires_at <= now_ms.saturating_add(skew_ms)))
}

fn apply_refresh_tokens(
    path: &Path,
    file: &mut Value,
    tokens: &RefreshedClaudeTokens,
) -> Result<(), CredentialError> {
    let oauth = claude_oauth_object_mut(path, file)?;
    oauth.insert(
        "accessToken".to_owned(),
        Value::String(tokens.access_token.clone()),
    );
    oauth.insert(
        "refreshToken".to_owned(),
        Value::String(tokens.refresh_token.clone()),
    );
    oauth.insert(
        "expiresAt".to_owned(),
        Value::Number(serde_json::Number::from(tokens.expires_at)),
    );
    Ok(())
}

fn claude_oauth_object<'a>(
    path: &Path,
    file: &'a Value,
) -> Result<&'a serde_json::Map<String, Value>, CredentialError> {
    file.get("claudeAiOauth")
        .and_then(Value::as_object)
        .ok_or_else(|| CredentialError::MissingField {
            path: path.to_path_buf(),
            field: "claudeAiOauth",
        })
}

fn claude_oauth_object_mut<'a>(
    path: &Path,
    file: &'a mut Value,
) -> Result<&'a mut serde_json::Map<String, Value>, CredentialError> {
    file.get_mut("claudeAiOauth")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| CredentialError::MissingField {
            path: path.to_path_buf(),
            field: "claudeAiOauth",
        })
}

fn write_json_file_atomic(path: &Path, value: &Value) -> Result<(), CredentialError> {
    let mut data =
        serde_json::to_vec_pretty(value).map_err(|source| CredentialError::SerializeFile {
            path: path.to_path_buf(),
            source,
        })?;
    data.push(b'\n');

    let tmp_path = temp_json_path(path);
    let mut file = fs::File::create(&tmp_path).map_err(|source| CredentialError::WriteFile {
        path: tmp_path.clone(),
        source,
    })?;
    file.write_all(&data)
        .and_then(|()| file.sync_all())
        .map_err(|source| CredentialError::WriteFile {
            path: tmp_path.clone(),
            source,
        })?;
    drop(file);

    fs::rename(&tmp_path, path).map_err(|source| CredentialError::ReplaceFile {
        path: path.to_path_buf(),
        source,
    })
}

fn temp_json_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn refresh_error_body(mut body: String) -> String {
    body.truncate(300);
    body
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::{
        apply_refresh_tokens, claude_file_needs_refresh_at, write_json_file_atomic,
        ClaudeRefreshResponse, RefreshedClaudeTokens,
    };

    #[test]
    fn computes_refreshed_expiration_in_millis() {
        let tokens = ClaudeRefreshResponse {
            access_token: "new-access".to_owned(),
            refresh_token: "new-refresh".to_owned(),
            expires_in: 3600,
        }
        .into_tokens("anthropic", 1_000)
        .unwrap();

        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token, "new-refresh");
        assert_eq!(tokens.expires_at, 3_601_000);
    }

    #[test]
    fn rejects_invalid_refreshed_expiration() {
        let error = ClaudeRefreshResponse {
            access_token: "new-access".to_owned(),
            refresh_token: "new-refresh".to_owned(),
            expires_in: 0,
        }
        .into_tokens("anthropic", 1_000)
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "refreshed anthropic credential response has invalid field expires_in"
        );
    }

    #[test]
    fn updates_only_claude_oauth_token_fields() {
        let mut file = json!({
            "claudeAiOauth": {
                "accessToken": "old-access",
                "refreshToken": "old-refresh",
                "expiresAt": 10,
                "scopes": ["user:inference"],
                "subscriptionType": "max",
                "rateLimitTier": "tier-1"
            },
            "unrelated": {
                "kept": true
            }
        });
        let tokens = RefreshedClaudeTokens {
            access_token: "new-access".to_owned(),
            refresh_token: "new-refresh".to_owned(),
            expires_at: 1_234_567,
        };

        apply_refresh_tokens(Path::new("credentials.json"), &mut file, &tokens).unwrap();

        assert_eq!(file["claudeAiOauth"]["accessToken"], json!("new-access"));
        assert_eq!(file["claudeAiOauth"]["refreshToken"], json!("new-refresh"));
        assert_eq!(file["claudeAiOauth"]["expiresAt"], json!(1_234_567));
        assert_eq!(file["claudeAiOauth"]["scopes"], json!(["user:inference"]));
        assert_eq!(file["claudeAiOauth"]["subscriptionType"], json!("max"));
        assert_eq!(file["claudeAiOauth"]["rateLimitTier"], json!("tier-1"));
        assert_eq!(file["unrelated"], json!({ "kept": true }));
    }

    #[test]
    fn atomic_write_replaces_existing_credential_file() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "alexbar-claude-refresh-{}-{nonce}-credentials.json",
            std::process::id()
        ));
        fs::write(&path, b"{\"old\":true}\n").unwrap();

        write_json_file_atomic(&path, &json!({ "new": true })).unwrap();
        let written = fs::read_to_string(&path).unwrap();

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&written).unwrap(),
            json!({ "new": true })
        );
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("json.tmp"));
    }

    #[test]
    fn detects_file_expiration_inside_refresh_skew() {
        let path = Path::new("credentials.json");
        let now_ms = 1_700_000_000_000;
        let skew_ms = 300_000;

        assert!(claude_file_needs_refresh_at(
            path,
            &json!({ "claudeAiOauth": { "expiresAt": now_ms + skew_ms } }),
            now_ms,
            skew_ms
        )
        .unwrap());
        assert!(!claude_file_needs_refresh_at(
            path,
            &json!({ "claudeAiOauth": { "expiresAt": now_ms + skew_ms + 1 } }),
            now_ms,
            skew_ms
        )
        .unwrap());
    }
}
