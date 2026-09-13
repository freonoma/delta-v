use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::ProviderId;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderSelection {
    Claude,
    Codex,
    #[default]
    Both,
}

impl ProviderSelection {
    pub fn includes(self, provider: ProviderId) -> bool {
        matches!(
            (self, provider),
            (Self::Both, _) | (Self::Claude, ProviderId::Claude) | (Self::Codex, ProviderId::Codex)
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PercentageMode {
    #[default]
    Remaining,
    Used,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub providers: ProviderSelection,
    pub tracked_limit: String,
    pub threshold: u8,
    pub refresh_seconds: u64,
    pub theme: Theme,
    pub percentage_mode: PercentageMode,
    pub claude_windows: Vec<String>,
    pub codex_windows: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            providers: ProviderSelection::Both,
            tracked_limit: "auto".into(),
            threshold: 20,
            refresh_seconds: 60,
            theme: Theme::System,
            percentage_mode: PercentageMode::Remaining,
            claude_windows: Vec::new(),
            codex_windows: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("The home directory is unavailable.")]
    MissingHome,
    #[error("Could not read or save Delta-V settings.")]
    Io(#[from] std::io::Error),
    #[error("The settings file is invalid. Correct ~/.config/delta-v/config.toml and restart.")]
    InvalidFile,
    #[error("Choose a remaining threshold between 0 and 100.")]
    Threshold,
    #[error("Choose a refresh interval between 30 and 900 seconds.")]
    RefreshInterval,
    #[error("Choose an available Claude or Codex limit, or automatic tracking.")]
    TrackedLimit,
    #[error("Choose up to two different windows per provider, with a valid ID for each.")]
    CompactWindows,
}

impl Settings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.threshold > 100 {
            return Err(SettingsError::Threshold);
        }
        if !(30..=900).contains(&self.refresh_seconds) {
            return Err(SettingsError::RefreshInterval);
        }
        if self.tracked_limit != "auto"
            && (!self.tracked_limit.starts_with("claude:")
                && !self.tracked_limit.starts_with("codex:"))
        {
            return Err(SettingsError::TrackedLimit);
        }
        if self.tracked_limit.len() > 256 {
            return Err(SettingsError::TrackedLimit);
        }
        for windows in [&self.claude_windows, &self.codex_windows] {
            if windows.len() > 2
                || windows.iter().enumerate().any(|(index, id)| {
                    id.trim().is_empty()
                        || id.len() > 256
                        || id.chars().any(char::is_control)
                        || windows[..index].contains(id)
                })
            {
                return Err(SettingsError::CompactWindows);
            }
        }
        Ok(())
    }
}

fn path() -> Result<PathBuf, SettingsError> {
    let home = std::env::var_os("HOME").ok_or(SettingsError::MissingHome)?;
    Ok(PathBuf::from(home).join(".config/delta-v/config.toml"))
}

pub fn load() -> Result<Settings, SettingsError> {
    let content = match fs::read_to_string(path()?) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => return Err(error.into()),
    };
    let settings: Settings = toml::from_str(&content).map_err(|_| SettingsError::InvalidFile)?;
    settings.validate()?;
    Ok(settings)
}

pub fn save(settings: &Settings) -> Result<(), SettingsError> {
    settings.validate()?;
    let path = path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(settings).map_err(|_| SettingsError::InvalidFile)?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, content)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unbounded_polling_and_invalid_thresholds() {
        let mut settings = Settings {
            refresh_seconds: 1,
            ..Settings::default()
        };
        assert!(matches!(
            settings.validate(),
            Err(SettingsError::RefreshInterval)
        ));
        settings.refresh_seconds = 60;
        settings.threshold = 101;
        assert!(matches!(settings.validate(), Err(SettingsError::Threshold)));
    }

    #[test]
    fn reads_partial_settings_without_losing_defaults() {
        let settings: Settings = toml::from_str("providers = 'claude'\nthreshold = 15").unwrap();
        assert_eq!(settings.providers, ProviderSelection::Claude);
        assert_eq!(settings.threshold, 15);
        assert_eq!(settings.refresh_seconds, 60);
        assert_eq!(settings.tracked_limit, "auto");
        assert_eq!(settings.percentage_mode, PercentageMode::Remaining);
        assert!(settings.claude_windows.is_empty());
        assert!(settings.codex_windows.is_empty());
    }

    #[test]
    fn older_settings_keep_remaining_percentages_and_used_mode_round_trips() {
        let old_settings = "providers = 'both'\ntracked_limit = 'claude:session'\nthreshold = 15\nrefresh_seconds = 120\ntheme = 'dark'";
        let mut settings: Settings = toml::from_str(old_settings).unwrap();
        assert_eq!(settings.percentage_mode, PercentageMode::Remaining);
        assert_eq!(settings.tracked_limit, "claude:session");
        assert_eq!(settings.threshold, 15);
        assert_eq!(settings.refresh_seconds, 120);
        assert_eq!(settings.theme, Theme::Dark);
        assert!(settings.claude_windows.is_empty());
        assert!(settings.codex_windows.is_empty());

        settings.percentage_mode = PercentageMode::Used;
        let saved = toml::to_string(&settings).unwrap();
        assert!(saved.contains("percentage_mode = \"used\""));
        assert_eq!(toml::from_str::<Settings>(&saved).unwrap(), settings);
    }

    #[test]
    fn window_choices_preserve_order_and_unknown_ids_when_saved() {
        let settings = Settings {
            claude_windows: vec!["future_window".into(), "weekly".into()],
            codex_windows: vec!["rate_limit:primary".into(), "é".repeat(128)],
            ..Settings::default()
        };
        settings.validate().unwrap();
        let saved = toml::to_string(&settings).unwrap();
        let restored: Settings = toml::from_str(&saved).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, settings);
    }

    #[test]
    fn rejects_invalid_window_choices_for_either_provider() {
        for windows in [
            vec!["session".into(), "weekly".into(), "future".into()],
            vec!["session".into(), "session".into()],
            vec![String::new()],
            vec!["   ".into()],
            vec!["session\n".into()],
            vec!["session\u{7f}".into()],
            vec!["x".repeat(257)],
            vec!["é".repeat(129)],
        ] {
            for provider in [ProviderId::Claude, ProviderId::Codex] {
                let mut settings = Settings::default();
                match provider {
                    ProviderId::Claude => settings.claude_windows = windows.clone(),
                    ProviderId::Codex => settings.codex_windows = windows.clone(),
                }
                assert!(matches!(
                    settings.validate(),
                    Err(SettingsError::CompactWindows)
                ));
            }
        }
    }
}
