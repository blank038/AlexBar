use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use thiserror::Error;

use crate::state::AppState;

const LATEST_RELEASE_API_URL: &str =
    "https://api.github.com/repos/blank038/AlexBar/releases/latest";
const LATEST_RELEASE_PAGE_URL: &str = "https://github.com/blank038/AlexBar/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub available: bool,
    pub release_url: Option<String>,
    pub published_at: Option<String>,
    pub release_notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
    body: Option<String>,
}

pub async fn check_for_update(app: &AppHandle) -> Result<UpdateCheck, UpdateError> {
    let state = app.state::<AppState>();
    let response = state
        .client()
        .get(LATEST_RELEASE_API_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    let status = response.status();

    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(no_release_update_check());
    }
    if !status.is_success() {
        return Err(UpdateError::Http {
            status: status.as_u16(),
        });
    }

    let release = response.json::<GitHubRelease>().await?;
    update_check_from_release(release)
}

pub fn open_release_page() -> Result<(), UpdateError> {
    open::that_detached(LATEST_RELEASE_PAGE_URL).map_err(UpdateError::Open)
}

fn no_release_update_check() -> UpdateCheck {
    UpdateCheck {
        current_version: CURRENT_VERSION.to_owned(),
        latest_version: None,
        available: false,
        release_url: None,
        published_at: None,
        release_notes: None,
    }
}

fn update_check_from_release(release: GitHubRelease) -> Result<UpdateCheck, UpdateError> {
    let current = parse_version(CURRENT_VERSION)?;
    let latest = parse_version(&release.tag_name)?;

    Ok(UpdateCheck {
        current_version: current.to_string(),
        latest_version: Some(latest.to_string()),
        available: latest > current,
        release_url: Some(release.html_url),
        published_at: release.published_at,
        release_notes: release.body,
    })
}

fn parse_version(version: &str) -> Result<Version, UpdateError> {
    let trimmed = version.trim();
    let normalized = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    Version::parse(normalized).map_err(|source| UpdateError::Version {
        value: version.to_owned(),
        source,
    })
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("GitHub latest release endpoint returned HTTP {status}")]
    Http { status: u16 },
    #[error("failed to request GitHub latest release: {0}")]
    Request(#[from] reqwest::Error),
    #[error("failed to parse release version {value}: {source}")]
    Version {
        value: String,
        source: semver::Error,
    },
    #[error("failed to open GitHub release page: {0}")]
    Open(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_tag_prefix() {
        assert_eq!(parse_version("v1.2.3").unwrap(), Version::new(1, 2, 3));
    }

    #[test]
    fn compares_latest_release_to_current_version() {
        let current = parse_version(CURRENT_VERSION).unwrap();
        let check = update_check_from_release(GitHubRelease {
            tag_name: format!("v{}.0.0", current.major + 1),
            html_url: LATEST_RELEASE_PAGE_URL.to_owned(),
            published_at: None,
            body: None,
        })
        .unwrap();

        assert!(check.available);
    }
}
