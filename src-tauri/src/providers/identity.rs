use reqwest::{Client, StatusCode};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{now, retry_after};

const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const MAX_PROFILE_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct IdentityCache {
    state: Mutex<CacheState>,
}

#[derive(Default)]
struct CacheState {
    cached: Option<([u8; 32], String)>,
    failures: u32,
    retry_at: Option<i64>,
}

impl CacheState {
    fn account(&self, fingerprint: &[u8; 32]) -> Option<String> {
        self.cached
            .as_ref()
            .filter(|(saved, _)| saved == fingerprint)
            .map(|(_, account)| account.clone())
    }

    fn can_request(&self, at: i64) -> bool {
        self.retry_at.is_none_or(|retry| at >= retry)
    }

    fn success(&mut self, fingerprint: [u8; 32], account: String) {
        self.cached = Some((fingerprint, account));
        self.failures = 0;
        self.retry_at = None;
    }

    fn failed(&mut self, at: i64, failure: ProfileFailure) {
        self.failures = self.failures.saturating_add(1);
        let delay = (60_i64 * (1_i64 << self.failures.saturating_sub(1).min(6)))
            .min(3600)
            .max(if failure.denied { 900 } else { 60 });
        self.retry_at = Some(at.saturating_add(delay).max(failure.retry_at.unwrap_or(at)));
    }
}

impl IdentityCache {
    pub async fn claude(&self, client: &Client, access_token: &str) -> Option<String> {
        // Only the fingerprint stays in memory. Token rotation must never become an account key.
        let fingerprint: [u8; 32] = Sha256::digest(access_token.as_bytes()).into();
        let mut state = self.state.lock().await;
        if let Some(account) = state.account(&fingerprint) {
            return Some(account);
        }
        if !state.can_request(now()) {
            return None;
        }
        match fetch_claude(client, access_token).await {
            Ok(account) => {
                state.success(fingerprint, account.clone());
                Some(account)
            }
            Err(failure) => {
                state.failed(now(), failure);
                None
            }
        }
    }
}

struct ProfileFailure {
    retry_at: Option<i64>,
    denied: bool,
}

impl ProfileFailure {
    fn unavailable() -> Self {
        Self {
            retry_at: None,
            denied: false,
        }
    }
}

async fn fetch_claude(client: &Client, access_token: &str) -> Result<String, ProfileFailure> {
    let mut response = client
        .get(CLAUDE_PROFILE_URL)
        .bearer_auth(access_token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .map_err(|_| ProfileFailure::unavailable())?;
    if !response.status().is_success() {
        return Err(ProfileFailure {
            retry_at: retry_after(response.headers()),
            denied: matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROFILE_BYTES as u64)
    {
        return Err(ProfileFailure::unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ProfileFailure::unavailable())?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_PROFILE_BYTES {
            return Err(ProfileFailure::unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ProfileFailure::unavailable())?;
    claude(&value).ok_or_else(ProfileFailure::unavailable)
}

pub(super) fn codex(value: &Value) -> Option<String> {
    let user = identifier(&value["user_id"])?;
    let account = identifier(&value["account_id"])?;
    Some(account_key("codex", user, account))
}

fn claude(value: &Value) -> Option<String> {
    let user = identifier(&value["account"]["uuid"])?;
    let organization = identifier(&value["organization"]["uuid"])?;
    Some(account_key("claude", user, organization))
}

fn identifier(value: &Value) -> Option<&str> {
    value.as_str().filter(|id| {
        !id.is_empty() && id.len() <= 256 && id.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    })
}

fn account_key(provider: &str, user: &str, account: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"delta-v-history-account-v1");
    for field in [provider, user, account] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn account_keys_require_both_user_and_workspace() {
        assert!(codex(&json!({"account_id": "workspace"})).is_none());
        assert!(codex(&json!({"user_id": "user"})).is_none());
        assert!(codex(&json!({"user_id": "", "account_id": "workspace"})).is_none());
        assert!(claude(&json!({"account": {"uuid": "user"}})).is_none());
        assert!(claude(&json!({"organization": {"uuid": "organization"}})).is_none());
    }

    #[test]
    fn hashes_stable_identity_and_separates_users_workspaces_and_providers() {
        let original = codex(&json!({"user_id": "a", "account_id": "bc"})).unwrap();
        assert_eq!(original.len(), 64);
        assert!(original.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            original,
            codex(&json!({"user_id": "a", "account_id": "bc", "email": "unused@example.test"}))
                .unwrap()
        );
        for other in [
            codex(&json!({"user_id": "ab", "account_id": "c"})).unwrap(),
            codex(&json!({"user_id": "b", "account_id": "bc"})).unwrap(),
            codex(&json!({"user_id": "a", "account_id": "other"})).unwrap(),
            claude(&json!({"account": {"uuid": "a"}, "organization": {"uuid": "bc"}})).unwrap(),
        ] {
            assert_ne!(original, other);
        }
    }

    #[test]
    fn cached_identity_is_bound_to_the_exact_successful_token() {
        let mut state = CacheState::default();
        let first = [1; 32];
        let refreshed = [2; 32];
        let account = account_key("claude", "user", "organization");
        state.success(first, account.clone());
        assert_eq!(state.account(&first), Some(account.clone()));
        assert_eq!(state.account(&refreshed), None);
        state.success(refreshed, account.clone());
        assert_eq!(state.account(&refreshed), Some(account));
        assert_eq!(state.account(&first), None);
    }

    #[test]
    fn profile_failures_back_off_and_respect_server_cooldowns() {
        let mut state = CacheState::default();
        state.failed(100, ProfileFailure::unavailable());
        assert!(!state.can_request(159));
        assert!(state.can_request(160));
        state.failed(160, ProfileFailure::unavailable());
        assert_eq!(state.retry_at, Some(280));
        state.failed(
            280,
            ProfileFailure {
                retry_at: Some(10_000),
                denied: false,
            },
        );
        assert!(!state.can_request(9_999));
        assert!(state.can_request(10_000));
        state.success([1; 32], "verified".into());
        assert_eq!(state.failures, 0);
        assert!(state.can_request(10_000));
    }

    #[test]
    fn denied_profile_does_not_retry_on_every_usage_poll() {
        let mut state = CacheState::default();
        state.failed(
            100,
            ProfileFailure {
                retry_at: None,
                denied: true,
            },
        );
        assert_eq!(state.retry_at, Some(1000));
    }
}
