pub mod claude;
pub mod codex;

use std::time::Duration;

use reqwest::{Client, StatusCode, header::HeaderMap};
use serde_json::Value;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc2822};

use crate::{
    credentials,
    model::{ProviderId, ProviderSnapshot},
};

const CLAUDE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const MAX_RESPONSE_BYTES: usize = 512 * 1024;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error(transparent)]
    Credential(#[from] credentials::CredentialError),
    #[error(
        "The saved CLI sign-in was rejected. Check your sign-in in the CLI, then refresh here."
    )]
    Authentication,
    #[error("The usage service denied access (403).")]
    AccessDenied { retry_at: Option<i64> },
    #[error("Could not reach the usage service. The last reading may be out of date.")]
    Network,
    #[error("The usage service asked us to wait before checking again.")]
    RateLimited { retry_at: Option<i64> },
    #[error("The usage service returned an error ({status}).")]
    Service { status: u16, retry_at: Option<i64> },
    #[error("The usage response could not be read: {0}")]
    Response(String),
}

impl ProviderError {
    pub fn retry_at(&self) -> Option<i64> {
        match self {
            Self::AccessDenied { retry_at }
            | Self::RateLimited { retry_at }
            | Self::Service { retry_at, .. } => *retry_at,
            _ => None,
        }
    }

    pub fn requires_sign_in(&self) -> bool {
        match self {
            Self::Authentication => true,
            Self::Credential(error) => error.requires_sign_in(),
            _ => false,
        }
    }
}

pub trait Provider {
    fn id(&self) -> ProviderId;
    async fn fetch(&self) -> Result<ProviderSnapshot, ProviderError>;
}

pub struct ClaudeProvider<'a> {
    pub client: &'a Client,
}

pub struct CodexProvider<'a> {
    pub client: &'a Client,
}

impl Provider for ClaudeProvider<'_> {
    fn id(&self) -> ProviderId {
        ProviderId::Claude
    }

    async fn fetch(&self) -> Result<ProviderSnapshot, ProviderError> {
        let value = fetch_json(self.id(), self.client).await?;
        claude::parse(value, now()).map_err(ProviderError::Response)
    }
}

impl Provider for CodexProvider<'_> {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }

    async fn fetch(&self) -> Result<ProviderSnapshot, ProviderError> {
        let value = fetch_json(self.id(), self.client).await?;
        codex::parse(value, now()).map_err(ProviderError::Response)
    }
}

pub fn client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(7))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("Delta-V/", env!("CARGO_PKG_VERSION")))
        .build()
}

pub fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

pub fn parse_retry_after(value: &str, now: i64) -> Option<i64> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = value.parse::<i64>().unwrap_or(i64::MAX);
        return Some(now.saturating_add(seconds));
    }
    OffsetDateTime::parse(value, &Rfc2822)
        .ok()
        .map(|date| date.unix_timestamp().max(now))
}

fn retry_after(headers: &HeaderMap) -> Option<i64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, now()))
}

fn response_error(status: StatusCode, retry_at: Option<i64>) -> Option<ProviderError> {
    if status.is_success() {
        return None;
    }
    Some(match status {
        StatusCode::UNAUTHORIZED => ProviderError::Authentication,
        StatusCode::FORBIDDEN => ProviderError::AccessDenied { retry_at },
        StatusCode::TOO_MANY_REQUESTS => ProviderError::RateLimited { retry_at },
        _ => ProviderError::Service {
            status: status.as_u16(),
            retry_at,
        },
    })
}

async fn fetch_json(id: ProviderId, client: &Client) -> Result<Value, ProviderError> {
    let mut credential = credentials::load(id).await?;
    for attempt in 0..2 {
        let mut request = client
            .get(match id {
                ProviderId::Claude => CLAUDE_URL,
                ProviderId::Codex => CODEX_URL,
            })
            .bearer_auth(&credential.access_token)
            .header(reqwest::header::ACCEPT, "application/json");
        if id == ProviderId::Claude {
            request = request.header("anthropic-beta", "oauth-2025-04-20");
        } else if let Some(account) = &credential.account_id {
            request = request.header("ChatGPT-Account-Id", account);
        }
        let mut response = request.send().await.map_err(|_| ProviderError::Network)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED && attempt == 0 {
            let refreshed = credentials::load(id).await?;
            if refreshed.access_token != credential.access_token
                || refreshed.account_id != credential.account_id
            {
                credential = refreshed;
                continue;
            }
            return Err(ProviderError::Authentication);
        }
        if let Some(error) = response_error(status, retry_after(response.headers())) {
            return Err(error);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ProviderError::Response(
                "response is larger than expected".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ProviderError::Network)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(ProviderError::Response(
                    "response is larger than expected".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        return serde_json::from_slice(&bytes)
            .map_err(|_| ProviderError::Response("invalid JSON".into()));
    }
    Err(ProviderError::Authentication)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "reads the local Claude sign-in and calls its usage endpoint"]
    async fn live_claude_usage() {
        let client = client().unwrap();
        let snapshot = ClaudeProvider { client: &client }.fetch().await.unwrap();
        assert_eq!(snapshot.provider, ProviderId::Claude);
        assert!(!snapshot.limits.is_empty());
    }

    #[tokio::test]
    #[ignore = "reads the local Codex sign-in and calls its usage endpoint"]
    async fn live_codex_usage() {
        let client = client().unwrap();
        let snapshot = CodexProvider { client: &client }.fetch().await.unwrap();
        assert_eq!(snapshot.provider, ProviderId::Codex);
        assert!(!snapshot.limits.is_empty());
    }

    #[test]
    fn respects_retry_after_seconds_and_http_dates() {
        let now = OffsetDateTime::parse(
            "2026-09-13T19:06:53Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
        .unix_timestamp();
        assert_eq!(parse_retry_after("120", now), Some(now + 120));
        assert_eq!(
            parse_retry_after("Sun, 13 Sep 2026 19:08:53 GMT", now),
            Some(now + 120)
        );
        assert_eq!(
            parse_retry_after("Sun, 13 Sep 2026 19:00:00 GMT", now),
            Some(now)
        );
        assert_eq!(parse_retry_after("later", now), None);
        assert_eq!(parse_retry_after("-10", now), None);
        assert_eq!(parse_retry_after("+10", now), None);
        assert_eq!(parse_retry_after(" 0 ", now), Some(now));
        assert_eq!(
            parse_retry_after("18446744073709551616", now),
            Some(i64::MAX)
        );
        assert_eq!(parse_retry_after("", now), None);
    }

    #[test]
    fn access_and_configuration_errors_do_not_request_another_login() {
        assert!(ProviderError::Authentication.requires_sign_in());
        assert!(
            ProviderError::Credential(credentials::CredentialError::ClaudeSignIn)
                .requires_sign_in()
        );
        assert!(
            ProviderError::Credential(credentials::CredentialError::CodexSignIn).requires_sign_in()
        );
        assert!(
            !ProviderError::Credential(credentials::CredentialError::Unreadable).requires_sign_in()
        );
        assert!(
            !ProviderError::Credential(credentials::CredentialError::CodexStorage)
                .requires_sign_in()
        );
    }

    #[test]
    fn forbidden_usage_does_not_request_login_and_preserves_server_cooldown() {
        let forbidden = response_error(StatusCode::FORBIDDEN, Some(5000)).unwrap();
        assert!(matches!(forbidden, ProviderError::AccessDenied { .. }));
        assert!(!forbidden.requires_sign_in());
        assert_eq!(forbidden.retry_at(), Some(5000));

        let unauthorized = response_error(StatusCode::UNAUTHORIZED, None).unwrap();
        assert!(unauthorized.requires_sign_in());
        assert!(response_error(StatusCode::OK, None).is_none());
    }
}
