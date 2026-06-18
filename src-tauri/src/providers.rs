use std::sync::Arc;

use crate::{
    credentials::CredentialSource,
    usage::{self, QuotaSource, RateLimitGate},
};

pub struct ProviderDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub quota: fn(Arc<RateLimitGate>) -> Box<dyn QuotaSource>,
    pub credentials: fn() -> Box<dyn CredentialSource>,
    pub short_quota_key: &'static str,
    pub long_quota_key: &'static str,
}

pub static DESCRIPTORS: &[&ProviderDescriptor] = &[
    &usage::codex::DESCRIPTOR,
    &usage::claude::DESCRIPTOR,
    &usage::zai::DESCRIPTOR,
];

pub fn find(id: &str) -> Option<&'static ProviderDescriptor> {
    DESCRIPTORS
        .iter()
        .copied()
        .find(|descriptor| descriptor.id == id)
}

pub fn ids() -> impl Iterator<Item = &'static str> {
    DESCRIPTORS.iter().map(|descriptor| descriptor.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_registered_providers() {
        assert!(find("openai-codex").is_some());
        assert!(find("anthropic").is_some());
        assert!(find("zai").is_some());
        assert!(find("gemini").is_none());
    }

    #[test]
    fn ids_follow_descriptor_order() {
        assert_eq!(
            ids().collect::<Vec<_>>(),
            ["openai-codex", "anthropic", "zai"]
        );
    }
}
