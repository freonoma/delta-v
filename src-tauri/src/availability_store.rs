//! Pending one-time watches, kept independently of usage history.

use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::notifications::availability::{PendingWatch, Watch};

const FILE_VERSION: u32 = 1;
const MAX_FILE_BYTES: u64 = 128 * 1024;
const MAX_WATCHES: usize = 64;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedEntry {
    pub id: u64,
    pub label: String,
    pub watch: PendingWatch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedWatches {
    pub next_id: u64,
    pub entries: Vec<SavedEntry>,
}

impl Default for SavedWatches {
    fn default() -> Self {
        Self {
            next_id: 1,
            entries: Vec::new(),
        }
    }
}

impl SavedWatches {
    fn validate(&self) -> Result<(), Error> {
        if self.entries.len() > MAX_WATCHES {
            return Err(Error::TooLarge);
        }
        if self.next_id == 0 {
            return Err(Error::InvalidFile);
        }
        let mut ids = BTreeSet::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.id == 0
                || entry.id >= self.next_id
                || !ids.insert(entry.id)
                || entry.label.trim().is_empty()
                || entry.label.len() > 256
                || entry.label.chars().any(char::is_control)
                || Watch::restore(&entry.watch).is_none()
                || self.entries[..index].iter().any(|other| {
                    other.watch.provider == entry.watch.provider
                        && other.watch.account_key == entry.watch.account_key
                        && other.watch.limit_id == entry.watch.limit_id
                        && other.watch.window_seconds == entry.watch.window_seconds
                })
            {
                return Err(Error::InvalidFile);
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    watches: SavedWatches,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("The home directory is unavailable, so availability watches cannot be saved.")]
    MissingHome,
    #[error("Could not read or save availability watches.")]
    Io(#[from] std::io::Error),
    #[error("The saved availability watches are invalid and could not be restored.")]
    InvalidFile,
    #[error("The saved availability watches exceed the storage limit.")]
    TooLarge,
    #[error("The saved availability watches use a newer format. This version cannot update them.")]
    UnsupportedVersion,
    #[error("The availability watch file is not a private regular file and could not be used.")]
    UnsafeFile,
    #[error("Another copy of Delta-V is updating availability watches. Try again.")]
    Busy,
    #[error("The saved availability watches changed. Restart Delta-V before trying again.")]
    Conflict,
    #[error("Availability watches were updated, but their durable storage could not be confirmed.")]
    DurabilityAfterCommit(#[source] std::io::Error),
}

impl Error {
    /// The replacement already happened. Adopt the new state, but do not send an alert.
    pub fn committed(&self) -> bool {
        matches!(self, Self::DurabilityAfterCommit(_))
    }
}

pub struct Store {
    path: Option<PathBuf>,
}

impl Store {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<SavedWatches, Error> {
        read_saved(self.path()?)
    }

    pub fn save(&self, expected: &SavedWatches, next: &SavedWatches) -> Result<(), Error> {
        self.save_with_sync(expected, next, File::sync_all)
    }

    fn save_with_sync(
        &self,
        expected: &SavedWatches,
        next: &SavedWatches,
        sync_parent: impl FnOnce(&File) -> std::io::Result<()>,
    ) -> Result<(), Error> {
        expected.validate()?;
        next.validate()?;
        if next.next_id < expected.next_id {
            return Err(Error::InvalidFile);
        }
        let path = self.path()?;
        // A read-only no-op must not create a directory or a coordination file.
        if expected == &SavedWatches::default() && next == expected && read_file(path)?.is_none() {
            return Ok(());
        }
        let parent = path.parent().ok_or(Error::UnsafeFile)?;
        let mut directories = fs::DirBuilder::new();
        directories.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directories.mode(0o700);
        }
        directories.create(parent)?;
        if !fs::symlink_metadata(parent)?.is_dir() {
            return Err(Error::UnsafeFile);
        }
        let parent_file = File::open(parent)?;
        let _lock = acquire_lock(&lock_path(path))?;
        let current = read_saved(path)?;
        if &current != expected {
            return Err(Error::Conflict);
        }
        if current == *next {
            return Ok(());
        }
        let content = serde_json::to_vec(&Envelope {
            version: FILE_VERSION,
            watches: next.clone(),
        })
        .map_err(|_| Error::InvalidFile)?;
        if content.len() as u64 > MAX_FILE_BYTES {
            return Err(Error::TooLarge);
        }
        let (temporary, mut file) = create_temporary(path)?;
        let result = (|| {
            file.write_all(&content)?;
            file.sync_all()?;
            drop(file);
            // Preserve a replacement made without our coordination lock as well.
            if read_saved(path)? != current {
                return Err(Error::Conflict);
            }
            fs::rename(&temporary, path)?;
            sync_parent(&parent_file).map_err(Error::DurabilityAfterCommit)?;
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

fn read_saved(path: &Path) -> Result<SavedWatches, Error> {
    read_file(path)?.map_or_else(|| Ok(SavedWatches::default()), |content| decode(&content))
}

fn decode(content: &[u8]) -> Result<SavedWatches, Error> {
    let value: serde_json::Value =
        serde_json::from_slice(content).map_err(|_| Error::InvalidFile)?;
    if value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|version| version > u64::from(FILE_VERSION))
    {
        return Err(Error::UnsupportedVersion);
    }
    let envelope: Envelope = serde_json::from_slice(content).map_err(|_| Error::InvalidFile)?;
    if envelope.version != FILE_VERSION {
        return Err(Error::InvalidFile);
    }
    envelope.watches.validate()?;
    Ok(envelope.watches)
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
        if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
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

fn checked_open(path: &Path, before: &Metadata, options: &OpenOptions) -> Result<File, Error> {
    let file = options.open(path)?;
    let opened = file.metadata()?;
    let after = regular_metadata(path)?.ok_or(Error::UnsafeFile)?;
    if !opened.is_file() || !same_file(before, &opened) || !same_file(&opened, &after) {
        return Err(Error::UnsafeFile);
    }
    Ok(file)
}

fn read_file(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    let Some(before) = regular_metadata(path)? else {
        return Ok(None);
    };
    if before.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge);
    }
    let file = checked_open(path, &before, OpenOptions::new().read(true))?;
    let mut content = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut content)?;
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(Some(content))
}

fn lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

fn acquire_lock(path: &Path) -> Result<File, Error> {
    let file = if let Some(before) = regular_metadata(path)? {
        checked_open(path, &before, OpenOptions::new().write(true))?
    } else {
        match private_options().open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let before = regular_metadata(path)?.ok_or(Error::UnsafeFile)?;
                checked_open(path, &before, OpenOptions::new().write(true))?
            }
            Err(error) => return Err(error.into()),
        }
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(Error::Busy),
        Err(TryLockError::Error(error)) => return Err(error.into()),
    }
    let after = regular_metadata(path)?.ok_or(Error::UnsafeFile)?;
    if !same_file(&file.metadata()?, &after) {
        return Err(Error::UnsafeFile);
    }
    // Keep the inode on disk: unlinking it would let another writer lock a different file.
    Ok(file)
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn create_temporary(path: &Path) -> Result<(PathBuf, File), Error> {
    for _ in 0..16 {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_extension(format!("json.{}.{sequence}.tmp", std::process::id()));
        match private_options().open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Could not create a temporary availability watch file.",
    )
    .into())
}

#[cfg(test)]
mod tests;
