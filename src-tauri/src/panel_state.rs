use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_FILE_BYTES: u64 = 16 * 1024;
const MAX_POSITION_OFFSET: f64 = 100_000.0;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MiniLayout {
    #[default]
    Columns,
    Stacked,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PanelPreferences {
    pub pinned: bool,
    pub mini: bool,
    pub expanded: bool,
    pub layout: MiniLayout,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PanelPosition {
    pub display_id: u32,
    pub x: f64,
    pub top: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PanelState {
    pub preferences: PanelPreferences,
    pub position: Option<PanelPosition>,
}

#[derive(Debug, Error)]
pub enum PanelStateError {
    #[error("The home directory is unavailable.")]
    MissingHome,
    #[error("Could not read or save the panel layout.")]
    Io(#[from] std::io::Error),
    #[error("The saved panel layout is invalid. Delta-V will use the default layout.")]
    InvalidFile,
    #[error("The saved panel layout is too large. Delta-V will use the default layout.")]
    TooLarge,
    #[error("The saved panel position is invalid. Delta-V will use the default position.")]
    InvalidPosition,
}

impl PanelState {
    fn validate(&self) -> Result<(), PanelStateError> {
        if self.position.is_some_and(|position| {
            [position.x, position.top]
                .into_iter()
                .any(|offset| !offset.is_finite() || !(0.0..=MAX_POSITION_OFFSET).contains(&offset))
        }) {
            return Err(PanelStateError::InvalidPosition);
        }
        Ok(())
    }
}

fn path() -> Result<PathBuf, PanelStateError> {
    let home = std::env::var_os("HOME").ok_or(PanelStateError::MissingHome)?;
    Ok(PathBuf::from(home).join(".config/delta-v/panel.toml"))
}

pub fn load() -> Result<PanelState, PanelStateError> {
    load_from(&path()?)
}

fn load_from(path: &Path) -> Result<PanelState, PanelStateError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PanelState::default());
        }
        Err(error) => return Err(error.into()),
    };
    let mut content = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut content)?;
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(PanelStateError::TooLarge);
    }
    decode(&content)
}

fn decode(content: &[u8]) -> Result<PanelState, PanelStateError> {
    let text = std::str::from_utf8(content).map_err(|_| PanelStateError::InvalidFile)?;
    let state: PanelState = toml::from_str(text).map_err(|_| PanelStateError::InvalidFile)?;
    state.validate()?;
    Ok(state)
}

pub fn save(state: &PanelState) -> Result<(), PanelStateError> {
    save_to(state, &path()?)
}

fn save_to(state: &PanelState, path: &Path) -> Result<(), PanelStateError> {
    state.validate()?;
    let content = toml::to_string_pretty(state).map_err(|_| PanelStateError::InvalidFile)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let (temporary, mut file) = create_temporary(path)?;
    let result = (|| {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(PanelStateError::Io)
}

fn create_temporary(path: &Path) -> Result<(PathBuf, File), std::io::Error> {
    for _ in 0..16 {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_extension(format!("toml.{}.{sequence}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Could not create a temporary panel layout file.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "delta-v-panel-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("panel.toml")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn pinned_state() -> PanelState {
        PanelState {
            preferences: PanelPreferences {
                pinned: true,
                mini: true,
                expanded: true,
                layout: MiniLayout::Stacked,
            },
            position: Some(PanelPosition {
                display_id: 17,
                x: 132.5,
                top: 44.25,
            }),
        }
    }

    #[test]
    fn missing_file_and_missing_fields_use_defaults() {
        let directory = TestDirectory::new();
        assert_eq!(load_from(&directory.file()).unwrap(), PanelState::default());
        assert_eq!(decode(b"").unwrap(), PanelState::default());
        let partial = decode(b"[preferences]\npinned = true\n").unwrap();
        assert_eq!(
            partial,
            PanelState {
                preferences: PanelPreferences {
                    pinned: true,
                    ..PanelPreferences::default()
                },
                position: None,
            }
        );
    }

    #[test]
    fn replaces_saved_state_without_leaving_temporary_files() {
        let directory = TestDirectory::new();
        let path = directory.file();
        save_to(&PanelState::default(), &path).unwrap();
        save_to(&pinned_state(), &path).unwrap();
        assert_eq!(load_from(&path).unwrap(), pinned_state());
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        let saved = fs::read_to_string(path).unwrap();
        assert!(saved.contains("layout = \"stacked\""));
    }

    #[test]
    fn invalid_save_keeps_the_previous_readable_file() {
        let directory = TestDirectory::new();
        let path = directory.file();
        save_to(&pinned_state(), &path).unwrap();
        let mut invalid = pinned_state();
        invalid.position.as_mut().unwrap().x = f64::INFINITY;
        assert!(matches!(
            save_to(&invalid, &path),
            Err(PanelStateError::InvalidPosition)
        ));
        assert_eq!(load_from(&path).unwrap(), pinned_state());
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn failed_replacement_removes_the_temporary_file() {
        let directory = TestDirectory::new();
        let path = directory.file();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            save_to(&pinned_state(), &path),
            Err(PanelStateError::Io(_))
        ));
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn rejects_malformed_unknown_or_incomplete_values() {
        for content in [
            "[",
            "[preferences]\npinned = 'yes'",
            "[preferences]\nlayout = 'grid'",
            "[preferences]\npined = true",
            "[position]\ndisplay_id = 1\nx = 10.0",
            "[position]\ndisplay_id = -1\nx = 0.0\ntop = 0.0",
        ] {
            assert!(matches!(
                decode(content.as_bytes()),
                Err(PanelStateError::InvalidFile)
            ));
        }
        assert!(matches!(decode(&[0xff]), Err(PanelStateError::InvalidFile)));
    }

    #[test]
    fn rejects_nonfinite_negative_and_unbounded_positions() {
        for value in ["nan", "inf", "-inf", "-0.5", "100000.5"] {
            for field in ["x", "top"] {
                let other = if field == "x" { "top" } else { "x" };
                let content =
                    format!("[position]\ndisplay_id = 1\n{field} = {value}\n{other} = 0.0");
                assert!(matches!(
                    decode(content.as_bytes()),
                    Err(PanelStateError::InvalidPosition)
                ));
            }
        }
    }

    #[test]
    fn bounds_reads_before_parsing() {
        let directory = TestDirectory::new();
        let path = directory.file();
        fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize]).unwrap();
        assert_eq!(load_from(&path).unwrap(), PanelState::default());
        fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(matches!(load_from(&path), Err(PanelStateError::TooLarge)));
    }
}
