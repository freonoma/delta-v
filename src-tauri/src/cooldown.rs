use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const FILE_VERSION: u32 = 1;
const MAX_FILE_BYTES: u64 = 4096;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wait {
    pub observed_at: i64,
    pub retry_at: i64,
    pub reason: Reason,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reason {
    RateLimited,
    AccessDenied,
    Service { status: u16 },
}

impl<'de> Deserialize<'de> for Reason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Empty struct variants reject fields that serde ignores on unit variants.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum StrictReason {
            RateLimited {},
            AccessDenied {},
            Service { status: u16 },
        }
        Ok(match StrictReason::deserialize(deserializer)? {
            StrictReason::RateLimited {} => Self::RateLimited,
            StrictReason::AccessDenied {} => Self::AccessDenied,
            StrictReason::Service { status } => Self::Service { status },
        })
    }
}

impl Wait {
    fn validate(&self) -> Result<(), Error> {
        if self.observed_at < 0 || self.retry_at <= self.observed_at {
            return Err(Error::InvalidFile);
        }
        if let Reason::Service { status } = self.reason
            && (!(100..=599).contains(&status)
                || (200..=299).contains(&status)
                || matches!(status, 401 | 403 | 429))
        {
            return Err(Error::InvalidFile);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait: Option<Wait>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("The home directory is unavailable, so Claude's wait cannot be saved.")]
    MissingHome,
    #[error("Could not read or save Claude's usage-check wait.")]
    Io(#[from] std::io::Error),
    #[error("The saved Claude wait is invalid and could not be restored.")]
    InvalidFile,
    #[error("The saved Claude wait is too large and could not be restored.")]
    TooLarge,
    #[error("The saved Claude wait uses a newer format. This version cannot update it.")]
    UnsupportedVersion,
    #[error("The saved Claude wait is not a regular file and could not be used.")]
    UnsafeFile,
    #[error("Could not update the saved Claude wait after the clock changed.")]
    Normalize { wait: Wait },
}

impl Error {
    pub fn recovered_wait(&self) -> Option<&Wait> {
        match self {
            Self::Normalize { wait } => Some(wait),
            _ => None,
        }
    }
}

pub struct Store {
    path: Option<PathBuf>,
}

impl Store {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    pub fn load(&self, now: i64) -> Result<Option<Wait>, Error> {
        self.load_with(now, |wait| self.save(Some(wait)))
    }

    fn load_with(
        &self,
        now: i64,
        save: impl FnOnce(&Wait) -> Result<(), Error>,
    ) -> Result<Option<Wait>, Error> {
        let Some(content) = read_file(self.path()?)? else {
            return Ok(None);
        };
        let Some(mut wait) = decode(&content)?.wait else {
            return Ok(None);
        };
        if now >= wait.retry_at {
            return Ok(None);
        }
        if now < wait.observed_at {
            if now < 0 {
                return Err(Error::InvalidFile);
            }
            let duration = wait
                .retry_at
                .checked_sub(wait.observed_at)
                .ok_or(Error::InvalidFile)?;
            wait.observed_at = now;
            wait.retry_at = now.checked_add(duration).ok_or(Error::InvalidFile)?;
            // Normalize once so restarting at the same clock cannot extend the wait.
            save(&wait).map_err(|_| Error::Normalize { wait: wait.clone() })?;
        }
        Ok(Some(wait))
    }

    pub fn save(&self, wait: Option<&Wait>) -> Result<(), Error> {
        if let Some(wait) = wait {
            wait.validate()?;
        }
        let path = self.path()?;
        let existing = read_file(path)?;
        if let Some(content) = &existing {
            reject_future_version(content)?;
            if decode(content).is_ok_and(|saved| saved.wait.as_ref() == wait) {
                return Ok(());
            }
        } else if wait.is_none() {
            return Ok(());
        }
        let content = toml::to_string(&Envelope {
            version: FILE_VERSION,
            wait: wait.cloned(),
        })
        .map_err(|_| Error::InvalidFile)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let (temporary, mut file) = create_temporary(path)?;
        let result = (|| {
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            drop(file);
            if let Some(content) = read_file(path)? {
                reject_future_version(&content)?;
            }
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn path(&self) -> Result<&Path, Error> {
        self.path.as_deref().ok_or(Error::MissingHome)
    }
}

fn reject_future_version(content: &[u8]) -> Result<(), Error> {
    let value = std::str::from_utf8(content)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(text).ok());
    if value
        .as_ref()
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_integer)
        .is_some_and(|version| version > i64::from(FILE_VERSION))
    {
        return Err(Error::UnsupportedVersion);
    }
    Ok(())
}

fn decode(content: &[u8]) -> Result<Envelope, Error> {
    reject_future_version(content)?;
    let text = std::str::from_utf8(content).map_err(|_| Error::InvalidFile)?;
    let envelope: Envelope = toml::from_str(text).map_err(|_| Error::InvalidFile)?;
    if envelope.version != FILE_VERSION {
        return Err(Error::InvalidFile);
    }
    if let Some(wait) = &envelope.wait {
        wait.validate()?;
    }
    Ok(envelope)
}

fn regular_metadata(path: &Path) -> Result<Option<Metadata>, Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err(Error::UnsafeFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(Error::UnsafeFile);
        }
    }
    Ok(Some(metadata))
}

fn same_file(first: &Metadata, second: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        first.dev() == second.dev() && first.ino() == second.ino()
    }
    #[cfg(not(unix))]
    {
        first.is_file() && second.is_file() && first.len() == second.len()
    }
}

fn read_file(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    let Some(before) = regular_metadata(path)? else {
        return Ok(None);
    };
    if before.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge);
    }
    let file = File::open(path)?;
    let opened = file.metadata()?;
    let after = regular_metadata(path)?.ok_or(Error::UnsafeFile)?;
    if !opened.is_file() || !same_file(&before, &opened) || !same_file(&opened, &after) {
        return Err(Error::UnsafeFile);
    }
    let mut content = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut content)?;
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(Some(content))
}

fn create_temporary(path: &Path) -> Result<(PathBuf, File), Error> {
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
            Err(error) => return Err(error.into()),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Could not create a temporary Claude wait file.",
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "delta-v-cooldown-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("claude-cooldown.toml")
        }

        fn store(&self) -> Store {
            Store::new(Some(self.file()))
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn wait() -> Wait {
        Wait {
            observed_at: 100,
            retry_at: 3700,
            reason: Reason::RateLimited,
        }
    }

    #[test]
    fn missing_file_and_clearing_it_do_not_create_storage() {
        let directory = Directory::new();
        let path = directory.0.join("absent/claude-cooldown.toml");
        let store = Store::new(Some(path));
        assert_eq!(store.load(100).unwrap(), None);
        store.save(None).unwrap();
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
        assert!(matches!(
            Store::new(None).load(100),
            Err(Error::MissingHome)
        ));
        assert!(matches!(
            Store::new(None).save(None),
            Err(Error::MissingHome)
        ));
    }

    #[test]
    fn round_trip_all_reasons_and_clear_without_temporary_files() {
        let directory = Directory::new();
        let store = directory.store();
        for reason in [
            Reason::RateLimited,
            Reason::AccessDenied,
            Reason::Service { status: 503 },
        ] {
            let wait = Wait { reason, ..wait() };
            store.save(Some(&wait)).unwrap();
            assert_eq!(store.load(101).unwrap(), Some(wait));
            assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        }
        store.save(None).unwrap();
        assert_eq!(store.load(101).unwrap(), None);
        assert_eq!(
            fs::read_to_string(directory.file()).unwrap(),
            "version = 1\n"
        );
    }

    #[test]
    fn expiry_does_not_restore_or_rewrite_an_old_wait() {
        let directory = Directory::new();
        let store = directory.store();
        store.save(Some(&wait())).unwrap();
        let original = fs::read(directory.file()).unwrap();
        assert_eq!(store.load(3700).unwrap(), None);
        assert_eq!(store.load(10_000).unwrap(), None);
        assert_eq!(fs::read(directory.file()).unwrap(), original);
    }

    #[test]
    fn backward_clock_rebases_once_and_keeps_the_original_duration() {
        let directory = Directory::new();
        let store = directory.store();
        store.save(Some(&wait())).unwrap();
        let expected = Wait {
            observed_at: 50,
            retry_at: 3650,
            ..wait()
        };
        assert_eq!(store.load(50).unwrap(), Some(expected.clone()));
        assert_eq!(store.load(50).unwrap(), Some(expected.clone()));
        assert_eq!(store.load(60).unwrap(), Some(expected));
        assert_eq!(store.load(3650).unwrap(), None);
    }

    #[test]
    fn failed_clock_normalization_returns_the_wait_without_exposing_storage_errors() {
        let directory = Directory::new();
        let store = directory.store();
        store.save(Some(&wait())).unwrap();
        let original = fs::read(directory.file()).unwrap();
        let error = store
            .load_with(50, |_| {
                Err(std::io::Error::other("private file details").into())
            })
            .unwrap_err();
        assert_eq!(
            error.recovered_wait(),
            Some(&Wait {
                observed_at: 50,
                retry_at: 3650,
                ..wait()
            })
        );
        assert!(!error.to_string().contains("private file details"));
        assert_eq!(fs::read(directory.file()).unwrap(), original);
        assert!(Error::InvalidFile.recovered_wait().is_none());
    }

    #[test]
    fn long_valid_service_deadlines_are_not_shortened() {
        let directory = Directory::new();
        let store = directory.store();
        let wait = Wait {
            retry_at: i64::MAX,
            ..wait()
        };
        store.save(Some(&wait)).unwrap();
        assert_eq!(store.load(101).unwrap(), Some(wait));
    }

    #[test]
    fn malformed_and_unknown_fields_are_rejected_but_replaceable() {
        let directory = Directory::new();
        let store = directory.store();
        for content in [
            "",
            "version = 0",
            "version = 1\nunknown = true",
            "version = 1\n[wait]\nobserved_at = 100\nretry_at = 200",
            "version = 1\n[wait]\nobserved_at = 100\nretry_at = 200\nextra = 1\n[wait.reason]\nkind = 'rate_limited'",
            "version = 1\n[wait]\nobserved_at = 100\nretry_at = 200\n[wait.reason]\nkind = 'rate_limited'\nstatus = 429",
            "version = 1\n[wait]\nobserved_at = 100\nretry_at = 200\n[wait.reason]\nkind = 'network'",
        ] {
            fs::write(directory.file(), content).unwrap();
            assert!(
                matches!(store.load(100), Err(Error::InvalidFile)),
                "{content}"
            );
            store.save(Some(&wait())).unwrap();
            assert_eq!(store.load(100).unwrap(), Some(wait()));
        }
        fs::write(directory.file(), [0xff, 0xfe]).unwrap();
        assert!(matches!(store.load(100), Err(Error::InvalidFile)));
        store.save(Some(&wait())).unwrap();
    }

    #[test]
    fn invalid_records_cannot_replace_a_saved_wait() {
        let directory = Directory::new();
        let store = directory.store();
        store.save(Some(&wait())).unwrap();
        let original = fs::read(directory.file()).unwrap();
        let mut invalid = vec![
            Wait {
                observed_at: -1,
                ..wait()
            },
            Wait {
                retry_at: 100,
                ..wait()
            },
            Wait {
                retry_at: 99,
                ..wait()
            },
        ];
        for status in [0, 99, 200, 299, 401, 403, 429, 600, u16::MAX] {
            invalid.push(Wait {
                reason: Reason::Service { status },
                ..wait()
            });
        }
        for wait in invalid {
            assert!(matches!(store.save(Some(&wait)), Err(Error::InvalidFile)));
            assert_eq!(fs::read(directory.file()).unwrap(), original);
        }
        assert!(matches!(store.load(-1), Err(Error::InvalidFile)));
    }

    #[test]
    fn future_versions_and_oversized_files_are_preserved() {
        let directory = Directory::new();
        let store = directory.store();
        let future = "version = 2\nnew_format = true\n";
        fs::write(directory.file(), future).unwrap();
        assert!(matches!(store.load(100), Err(Error::UnsupportedVersion)));
        assert!(matches!(
            store.save(Some(&wait())),
            Err(Error::UnsupportedVersion)
        ));
        assert!(matches!(store.save(None), Err(Error::UnsupportedVersion)));
        assert_eq!(fs::read_to_string(directory.file()).unwrap(), future);

        let oversized = vec![b'x'; MAX_FILE_BYTES as usize + 1];
        fs::write(directory.file(), &oversized).unwrap();
        assert!(matches!(store.load(100), Err(Error::TooLarge)));
        assert!(matches!(store.save(Some(&wait())), Err(Error::TooLarge)));
        assert_eq!(fs::read(directory.file()).unwrap(), oversized);
    }

    #[test]
    fn regular_files_are_required_and_failed_save_leaves_no_temp_file() {
        let directory = Directory::new();
        fs::create_dir(directory.file()).unwrap();
        let store = directory.store();
        assert!(matches!(store.load(100), Err(Error::UnsafeFile)));
        assert!(matches!(store.save(Some(&wait())), Err(Error::UnsafeFile)));
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);

        let parent = directory.0.join("not-a-directory");
        fs::write(&parent, b"unchanged").unwrap();
        let store = Store::new(Some(parent.join("claude-cooldown.toml")));
        assert!(store.save(Some(&wait())).is_err());
        assert_eq!(fs::read(parent).unwrap(), b"unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hardlinks_are_rejected_and_saved_files_are_private() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = Directory::new();
        let external = directory.0.join("external");
        fs::write(&external, b"unchanged").unwrap();
        let store = directory.store();
        symlink(&external, directory.file()).unwrap();
        assert!(matches!(store.load(100), Err(Error::UnsafeFile)));
        assert!(matches!(store.save(Some(&wait())), Err(Error::UnsafeFile)));
        assert_eq!(fs::read(&external).unwrap(), b"unchanged");
        fs::remove_file(directory.file()).unwrap();
        fs::hard_link(&external, directory.file()).unwrap();
        assert!(matches!(store.load(100), Err(Error::UnsafeFile)));
        assert!(matches!(store.save(Some(&wait())), Err(Error::UnsafeFile)));
        fs::remove_file(directory.file()).unwrap();
        store.save(Some(&wait())).unwrap();
        assert_eq!(
            fs::metadata(directory.file()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
