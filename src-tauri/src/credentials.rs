use std::{
    fs::{self, File},
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{model::ProviderId, platform};

const MAX_CREDENTIAL_BYTES: u64 = 65_536;
const MAX_CONFIG_BYTES: u64 = 262_144;
const KEYCHAIN_ITEM_MISSING: &str = "No CLI sign-in was found in macOS Keychain";

pub struct Credential {
    pub access_token: String,
    pub account_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("The home directory is unavailable.")]
    MissingHome,
    #[error("Sign in through Claude Code, then refresh Delta-V.")]
    ClaudeSignIn,
    #[error("Sign in to Codex with your ChatGPT account, then refresh Delta-V.")]
    CodexSignIn,
    #[error("Could not read the CLI credential store. Check file and Keychain access.")]
    Unreadable,
    #[error("The Codex credential storage setting is unsupported or invalid.")]
    CodexStorage,
    #[error(
        "Codex keeps this sign-in in memory. Choose file or keyring storage in Codex so Delta-V can read it."
    )]
    CodexEphemeral,
}

impl CredentialError {
    pub fn requires_sign_in(&self) -> bool {
        matches!(self, Self::ClaudeSignIn | Self::CodexSignIn)
    }
}

fn home() -> Result<PathBuf, CredentialError> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(CredentialError::MissingHome)
}

fn read_optional(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, CredentialError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CredentialError::Unreadable),
    };
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(CredentialError::Unreadable);
    }
    let file = File::open(path).map_err(|_| CredentialError::Unreadable)?;
    let mut data = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut data)
        .map_err(|_| CredentialError::Unreadable)?;
    if data.len() as u64 > maximum {
        return Err(CredentialError::Unreadable);
    }
    Ok(Some(data))
}

fn read_json(path: &Path) -> Result<Option<Value>, CredentialError> {
    read_optional(path, MAX_CREDENTIAL_BYTES)?
        .map(|data| serde_json::from_slice(&data).map_err(|_| CredentialError::Unreadable))
        .transpose()
}

fn keychain_json(service: &str, account: &str) -> Result<Option<Value>, CredentialError> {
    decode_keychain(platform::read_keychain(service, account))
}

fn decode_keychain(result: Result<String, String>) -> Result<Option<Value>, CredentialError> {
    match result {
        Ok(data) if data.len() as u64 <= MAX_CREDENTIAL_BYTES => serde_json::from_str(&data)
            .map(Some)
            .map_err(|_| CredentialError::Unreadable),
        Err(error) if error == KEYCHAIN_ITEM_MISSING => Ok(None),
        _ => Err(CredentialError::Unreadable),
    }
}

fn token(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 16_384
                && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        })
        .map(String::from)
}

fn claude() -> Result<Credential, CredentialError> {
    let override_dir = std::env::var("CLAUDE_SECURESTORAGE_CONFIG_DIR")
        .ok()
        .or_else(|| std::env::var("CLAUDE_CONFIG_DIR").ok());
    let raw_dir = override_dir.as_deref().filter(|value| !value.is_empty());
    let directory = raw_dir.map_or_else(
        || home().map(|path| path.join(".claude")),
        |path| Ok(PathBuf::from(platform::normalize_nfc(path))),
    )?;
    let suffix = raw_dir.map_or_else(String::new, |path| {
        let hash = format!(
            "{:x}",
            Sha256::digest(platform::normalize_nfc(path).as_bytes())
        );
        format!("-{}", &hash[..8])
    });
    let service = format!("Claude Code-credentials{suffix}");
    let account = std::env::var("USER").map_err(|_| CredentialError::Unreadable)?;
    let value = match keychain_json(&service, &account)? {
        Some(value) => value,
        None => {
            read_json(&directory.join(".credentials.json"))?.ok_or(CredentialError::ClaudeSignIn)?
        }
    };
    parse_claude(&value)
}

fn parse_claude(value: &Value) -> Result<Credential, CredentialError> {
    let oauth = &value["claudeAiOauth"];
    if !oauth.is_object() {
        return Err(CredentialError::ClaudeSignIn);
    }
    if !oauth["scopes"].is_null() && !oauth["scopes"].is_array() {
        return Err(CredentialError::Unreadable);
    }
    if let Some(scopes) = oauth["scopes"].as_array()
        && !scopes
            .iter()
            .any(|scope| scope.as_str() == Some("user:profile"))
    {
        return Err(CredentialError::ClaudeSignIn);
    }
    Ok(Credential {
        access_token: token(oauth, "accessToken").ok_or(CredentialError::ClaudeSignIn)?,
        account_id: None,
    })
}

fn codex_storage(content: Option<&[u8]>) -> Result<String, CredentialError> {
    let Some(content) = content else {
        return Ok("file".to_owned());
    };
    let content = std::str::from_utf8(content).map_err(|_| CredentialError::CodexStorage)?;
    let config: toml::Value = toml::from_str(content).map_err(|_| CredentialError::CodexStorage)?;
    match config.get("cli_auth_credentials_store") {
        None => Ok("file".to_owned()),
        Some(value) => match value.as_str() {
            Some(mode @ ("file" | "keyring" | "auto" | "ephemeral")) => Ok(mode.to_owned()),
            _ => Err(CredentialError::CodexStorage),
        },
    }
}

fn codex() -> Result<Credential, CredentialError> {
    let directory = match std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => home()?.join(".codex"),
    };
    let config = read_optional(&directory.join("config.toml"), MAX_CONFIG_BYTES)?;
    let mode = codex_storage(config.as_deref())?;
    let value = match mode.as_str() {
        "file" => read_json(&directory.join("auth.json"))?.ok_or(CredentialError::CodexSignIn)?,
        "keyring" | "auto" => {
            let canonical =
                fs::canonicalize(&directory).map_err(|_| CredentialError::Unreadable)?;
            let hash = format!(
                "{:x}",
                Sha256::digest(canonical.to_string_lossy().as_bytes())
            );
            let account = format!("cli|{}", &hash[..16]);
            match keychain_json("Codex Auth", &account)? {
                Some(value) => value,
                None if mode == "auto" => {
                    read_json(&directory.join("auth.json"))?.ok_or(CredentialError::CodexSignIn)?
                }
                None => return Err(CredentialError::CodexSignIn),
            }
        }
        "ephemeral" => return Err(CredentialError::CodexEphemeral),
        _ => return Err(CredentialError::CodexStorage),
    };
    parse_codex(&value)
}

fn parse_codex(value: &Value) -> Result<Credential, CredentialError> {
    match value.get("auth_mode") {
        Some(Value::String(mode)) if mode == "chatgpt" => {}
        None | Some(Value::Null) if value.get("OPENAI_API_KEY").is_none_or(Value::is_null) => {}
        _ => return Err(CredentialError::CodexSignIn),
    }
    let tokens = &value["tokens"];
    Ok(Credential {
        access_token: token(tokens, "access_token").ok_or(CredentialError::CodexSignIn)?,
        account_id: Some(token(tokens, "account_id").ok_or(CredentialError::CodexSignIn)?),
    })
}

pub async fn load(provider: ProviderId) -> Result<Credential, CredentialError> {
    tokio::task::spawn_blocking(move || match provider {
        ProviderId::Claude => claude(),
        ProviderId::Codex => codex(),
    })
    .await
    .map_err(|_| CredentialError::Unreadable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stale_subscription_tokens_cannot_override_an_api_key_login() {
        let tokens = json!({"access_token":"test-token","account_id":"test-account"});
        let explicit = json!({"auth_mode":"apikey","tokens":tokens});
        let legacy = json!({"OPENAI_API_KEY":"test-api-key","tokens":tokens});
        assert!(matches!(
            parse_codex(&explicit),
            Err(CredentialError::CodexSignIn)
        ));
        assert!(matches!(
            parse_codex(&legacy),
            Err(CredentialError::CodexSignIn)
        ));
        assert!(parse_codex(&json!({"auth_mode":"chatgpt","tokens":tokens})).is_ok());
        assert!(parse_codex(&json!({"OPENAI_API_KEY":null,"tokens":tokens})).is_ok());
        assert!(parse_codex(&json!({"auth_mode":"future","tokens":tokens})).is_err());
    }

    #[test]
    fn codex_requires_selected_account_context_and_safe_header_values() {
        assert!(parse_codex(&json!({"tokens":{"access_token":"test-token"}})).is_err());
        assert!(
            parse_codex(
                &json!({"tokens":{"access_token":"test\nvalue","account_id":"test-account"}})
            )
            .is_err()
        );
        assert!(
            parse_codex(&json!({"tokens":{"access_token":"test-token","account_id":" "}})).is_err()
        );
    }

    #[test]
    fn malformed_storage_settings_do_not_select_a_different_store() {
        assert_eq!(codex_storage(None).unwrap(), "file");
        assert_eq!(codex_storage(Some(b"model = 'test'\n")).unwrap(), "file");
        assert_eq!(
            codex_storage(Some(b"cli_auth_credentials_store = 'keyring'\n")).unwrap(),
            "keyring"
        );
        assert!(codex_storage(Some(b"cli_auth_credentials_store = 42\n")).is_err());
        assert!(codex_storage(Some(b"cli_auth_credentials_store = 'future'\n")).is_err());
        assert!(codex_storage(Some(b"not toml")).is_err());
    }

    #[test]
    fn claude_requires_a_subscription_token_and_profile_scope_when_reported() {
        assert!(
            parse_claude(
                &json!({"claudeAiOauth":{"accessToken":"test-token","scopes":["user:profile"]}})
            )
            .is_ok()
        );
        assert!(parse_claude(&json!({"claudeAiOauth":{"accessToken":"test-token"}})).is_ok());
        assert!(
            parse_claude(
                &json!({"claudeAiOauth":{"accessToken":"test-token","scopes":["user:inference"]}})
            )
            .is_err()
        );
        assert!(matches!(
            parse_claude(&json!({"claudeAiOauth":{"accessToken":"test-token","scopes":true}})),
            Err(CredentialError::Unreadable)
        ));
    }

    #[test]
    fn credential_reads_are_bounded_and_missing_is_distinct_from_unreadable() {
        let folder = std::env::temp_dir().join(format!(
            "delta-v-credential-test-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir(&folder).unwrap();
        assert!(read_optional(&folder.join("missing"), 8).unwrap().is_none());
        assert!(matches!(
            read_optional(&folder, 8),
            Err(CredentialError::Unreadable)
        ));
        let file = folder.join("synthetic.json");
        fs::write(&file, b"12345678").unwrap();
        assert_eq!(read_optional(&file, 8).unwrap().unwrap(), b"12345678");
        fs::write(&file, b"123456789").unwrap();
        assert!(matches!(
            read_optional(&file, 8),
            Err(CredentialError::Unreadable)
        ));
        fs::remove_file(file).unwrap();
        fs::remove_dir(folder).unwrap();
    }

    #[test]
    fn only_a_missing_keychain_item_permits_file_fallback() {
        assert!(
            decode_keychain(Err(KEYCHAIN_ITEM_MISSING.to_owned()))
                .unwrap()
                .is_none()
        );
        for error in [
            "Keychain access denied",
            "No accessible CLI sign-in was found in macOS Keychain",
            "Keychain access timed out. Allow access and try again.",
        ] {
            assert!(matches!(
                decode_keychain(Err(error.to_owned())),
                Err(CredentialError::Unreadable)
            ));
        }
        assert!(matches!(
            decode_keychain(Ok("invalid-json".to_owned())),
            Err(CredentialError::Unreadable)
        ));
        assert!(matches!(
            decode_keychain(Ok(" ".repeat(MAX_CREDENTIAL_BYTES as usize + 1))),
            Err(CredentialError::Unreadable)
        ));
    }
}
