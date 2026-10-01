use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Date, Month, OffsetDateTime};

use crate::model::{Provenance, ProviderId};

const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const SECONDS_PER_DAY: i64 = 86_400;
static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    #[default]
    Forever,
    Days30,
    Days90,
}

impl Retention {
    pub(crate) fn cutoff(self, now: i64) -> Option<i64> {
        match self {
            Self::Forever => None,
            Self::Days30 => Some(now.saturating_sub(30 * SECONDS_PER_DAY)),
            Self::Days90 => Some(now.saturating_sub(90 * SECONDS_PER_DAY)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRecord {
    pub version: u32,
    pub provider: ProviderId,
    pub account_key: String,
    pub observed_at: i64,
    pub limits: Vec<HistoryLimit>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryLimit {
    pub id: String,
    pub label: String,
    pub used_fraction: f64,
    pub resets_at: Option<i64>,
    pub window_seconds: Option<u64>,
    pub provenance: Provenance,
}

impl HistoryRecord {
    fn validate(&self) -> Result<Date, HistoryError> {
        if self.version != 1 {
            return Err(HistoryError::UnsupportedVersion);
        }
        let date = OffsetDateTime::from_unix_timestamp(self.observed_at)
            .map_err(|_| HistoryError::InvalidRecord)?
            .date();
        if !(1970..=9999).contains(&date.year())
            || self.account_key.len() != 64
            || !self
                .account_key
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.limits.is_empty()
            || self.limits.len() > 128
        {
            return Err(HistoryError::InvalidRecord);
        }
        let mut ids = BTreeSet::new();
        for limit in &self.limits {
            if !valid_label(&limit.id)
                || !valid_label(&limit.label)
                || !ids.insert(&limit.id)
                || !limit.used_fraction.is_finite()
                || !(0.0..=1.0).contains(&limit.used_fraction)
                || limit.window_seconds == Some(0)
                || limit
                    .resets_at
                    .is_some_and(|reset| OffsetDateTime::from_unix_timestamp(reset).is_err())
            {
                return Err(HistoryError::InvalidRecord);
            }
        }
        Ok(date)
    }
}

fn valid_label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StoreInfo {
    pub bytes: u64,
    pub records: u64,
    pub last_recorded_at: Option<i64>,
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error(
        "Could not read or save local history. Check the history folder's permissions and available disk space."
    )]
    Io(#[from] std::io::Error),
    #[error(
        "The history folder contains a symbolic link or an unexpected file type. No linked files were changed."
    )]
    UnsafePath,
    #[error("The usage reading could not be saved in history.")]
    InvalidRecord,
    #[error("A history file is invalid. Saved readings have been left in place.")]
    InvalidFile,
    #[error("A history file uses a newer format. Update Delta-V before recording more history.")]
    UnsupportedVersion,
    #[error(
        "A history file contains an interrupted write. Delta-V will repair it when recording resumes."
    )]
    InterruptedWrite,
    #[error(
        "A history file or reading is larger than the supported limit. Saved readings have been left in place."
    )]
    TooLarge,
}

#[derive(Clone, Debug, PartialEq)]
struct Fingerprint {
    bytes: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl Fingerprint {
    fn read(metadata: &Metadata) -> Result<Self, HistoryError> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            bytes: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tail {
    Complete,
    MissingNewline,
    Interrupted { valid_bytes: u64 },
}

#[derive(Clone, Debug)]
struct CachedFile {
    fingerprint: Fingerprint,
    info: StoreInfo,
    tail: Tail,
    oldest: Option<i64>,
}

pub struct Store {
    directory: PathBuf,
    files: BTreeMap<String, CachedFile>,
}

impl Store {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            files: BTreeMap::new(),
        }
    }

    pub fn visit_records<E: From<HistoryError>>(
        &self,
        from: Option<i64>,
        until: i64,
        mut check: impl FnMut() -> Result<(), E>,
        mut visit: impl FnMut(HistoryRecord) -> Result<(), E>,
    ) -> Result<(), E> {
        check()?;
        for (name, fingerprint) in self.entries(false)? {
            check()?;
            let date = date_from_filename(&name).ok_or(HistoryError::InvalidFile)?;
            let start = date
                .with_time(time::Time::MIDNIGHT)
                .assume_utc()
                .unix_timestamp();
            if start >= until
                || from.is_some_and(|from| start.saturating_add(SECONDS_PER_DAY) <= from)
            {
                continue;
            }
            if fingerprint.bytes > MAX_FILE_BYTES {
                return Err(HistoryError::TooLarge.into());
            }
            let path = self.directory.join(name);
            let file = open_checked(&path, &fingerprint, false)?;
            // An external append must not turn a bounded read into a growing stream.
            let mut reader = BufReader::new(file.take(fingerprint.bytes.saturating_add(1)));
            let mut line = Vec::new();
            let mut records = Vec::new();
            let mut bytes = 0_u64;
            while read_line(&mut reader, &mut line)? {
                check()?;
                bytes += line.len() as u64;
                if bytes > fingerprint.bytes {
                    return Err(HistoryError::InvalidFile.into());
                }
                if line.last() != Some(&b'\n')
                    && line.starts_with(b"{\"version\":1,")
                    && serde_json::from_slice::<serde_json::Value>(&line)
                        .is_err_and(|error| error.is_eof())
                {
                    return Err(HistoryError::InterruptedWrite.into());
                }
                let record = parse_record(&line, date)?;
                if record.observed_at < until && from.is_none_or(|from| record.observed_at >= from)
                {
                    records.push(record);
                }
            }
            check_unchanged(&path, &fingerprint)?;
            // Sorting one bounded file handles clock adjustments and hand-edited records.
            records.sort_by_key(|record| record.observed_at);
            for record in records {
                check()?;
                visit(record)?;
            }
        }
        check()?;
        Ok(())
    }

    pub fn info(&mut self) -> Result<StoreInfo, HistoryError> {
        self.refresh()?;
        if self
            .files
            .values()
            .any(|file| matches!(file.tail, Tail::Interrupted { .. }))
        {
            return Err(HistoryError::InterruptedWrite);
        }
        Ok(self.total())
    }

    pub fn append(
        &mut self,
        record: &HistoryRecord,
        retention: Retention,
        now: i64,
    ) -> Result<StoreInfo, HistoryError> {
        let date = record.validate()?;
        let mut content = serde_json::to_vec(record).map_err(|_| HistoryError::InvalidRecord)?;
        content.push(b'\n');
        if content.len() > MAX_LINE_BYTES {
            return Err(HistoryError::TooLarge);
        }
        ensure_directory(&self.directory)?;
        self.maintain(retention, now)?;
        if retention
            .cutoff(now)
            .is_some_and(|cutoff| record.observed_at < cutoff)
        {
            return Ok(self.total());
        }
        let name = filename(date);
        let path = self.directory.join(&name);
        let previous = self.files.get(&name).cloned();
        let previous_bytes = previous.as_ref().map_or(0, |file| file.fingerprint.bytes);
        let oldest = previous.as_ref().and_then(|file| file.oldest);
        if previous_bytes.saturating_add(content.len() as u64) > MAX_FILE_BYTES {
            return Err(HistoryError::TooLarge);
        }
        let mut file = match &previous {
            Some(cached) => open_checked(&path, &cached.fingerprint, true)?,
            None => create_private(&path)?,
        };
        // An interrupted write must be rescanned before the next append.
        self.files.remove(&name);
        file.write_all(&content)?;
        file.sync_data()?;
        let fingerprint = Fingerprint::read(&file.metadata()?)?;
        if fingerprint.bytes != previous_bytes + content.len() as u64 {
            return Err(HistoryError::InvalidFile);
        }
        check_unchanged(&path, &fingerprint)?;
        let mut info = previous.map_or_else(StoreInfo::default, |file| file.info);
        info.bytes = previous_bytes + content.len() as u64;
        info.records += 1;
        info.last_recorded_at = Some(
            info.last_recorded_at
                .map_or(record.observed_at, |last| last.max(record.observed_at)),
        );
        self.files.insert(
            name,
            CachedFile {
                fingerprint,
                info,
                tail: Tail::Complete,
                oldest: Some(
                    oldest.map_or(record.observed_at, |first| first.min(record.observed_at)),
                ),
            },
        );
        Ok(self.total())
    }

    pub fn maintain(&mut self, retention: Retention, now: i64) -> Result<StoreInfo, HistoryError> {
        self.refresh()?;
        // A crash before the atomic rename can leave a second copy of retained readings.
        for (name, fingerprint) in self.entries(true)? {
            if is_temporary_filename(&name) {
                let path = self.directory.join(name);
                check_unchanged(&path, &fingerprint)?;
                fs::remove_file(path)?;
            }
        }
        let names: Vec<String> = self.files.keys().cloned().collect();
        for name in &names {
            let cached = self
                .files
                .get(name)
                .cloned()
                .ok_or(HistoryError::InvalidFile)?;
            if cached.tail != Tail::Complete {
                let path = self.directory.join(name);
                let mut file = open_checked(&path, &cached.fingerprint, true)?;
                self.files.remove(name);
                match cached.tail {
                    Tail::Interrupted { valid_bytes } => file.set_len(valid_bytes)?,
                    Tail::MissingNewline => file.write_all(b"\n")?,
                    Tail::Complete => {}
                }
                file.sync_data()?;
                let date = date_from_filename(name).ok_or(HistoryError::InvalidFile)?;
                self.files.insert(name.clone(), scan(&path, date)?);
            }
        }
        if let Some(cutoff) = retention.cutoff(now) {
            for name in names {
                let cached = self
                    .files
                    .get(&name)
                    .cloned()
                    .ok_or(HistoryError::InvalidFile)?;
                if cached
                    .info
                    .last_recorded_at
                    .is_none_or(|last| last < cutoff)
                {
                    let path = self.directory.join(&name);
                    check_unchanged(&path, &cached.fingerprint)?;
                    fs::remove_file(path)?;
                    self.files.remove(&name);
                } else if cached.oldest.is_some_and(|first| first < cutoff) {
                    let date = date_from_filename(&name).ok_or(HistoryError::InvalidFile)?;
                    let path = self.directory.join(&name);
                    prune_file(&path, &cached.fingerprint, cutoff, date)?;
                    self.files.remove(&name);
                    self.files.insert(name, scan(&path, date)?);
                }
            }
        }
        Ok(self.total())
    }

    pub fn clear(&mut self) -> Result<StoreInfo, HistoryError> {
        let files = self.entries(true)?;
        self.files.clear();
        for (name, fingerprint) in files {
            let path = self.directory.join(name);
            check_unchanged(&path, &fingerprint)?;
            fs::remove_file(path)?;
        }
        Ok(StoreInfo::default())
    }

    fn refresh(&mut self) -> Result<(), HistoryError> {
        let entries = self.entries(false)?;
        self.files.retain(|name, _| entries.contains_key(name));
        for (name, fingerprint) in entries {
            if self
                .files
                .get(&name)
                .is_some_and(|file| file.fingerprint == fingerprint)
            {
                continue;
            }
            self.files.remove(&name);
            let date = date_from_filename(&name).ok_or(HistoryError::InvalidFile)?;
            let cached = scan(&self.directory.join(&name), date)?;
            self.files.insert(name, cached);
        }
        Ok(())
    }

    fn entries(
        &self,
        include_temporary: bool,
    ) -> Result<BTreeMap<String, Fingerprint>, HistoryError> {
        if !check_directory(&self.directory)? {
            return Ok(BTreeMap::new());
        }
        let mut entries = BTreeMap::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if date_from_filename(&name).is_some()
                || (include_temporary && is_temporary_filename(&name))
            {
                let metadata = checked_metadata(&entry.path())?;
                entries.insert(name, Fingerprint::read(&metadata)?);
            }
        }
        Ok(entries)
    }

    fn total(&self) -> StoreInfo {
        let mut info = StoreInfo::default();
        for file in self.files.values() {
            info.bytes = info.bytes.saturating_add(file.info.bytes);
            info.records = info.records.saturating_add(file.info.records);
            info.last_recorded_at = info.last_recorded_at.max(file.info.last_recorded_at);
        }
        info
    }
}

fn filename(date: Date) -> String {
    format!(
        "usage-{:04}-{:02}-{:02}.jsonl",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

fn date_from_filename(name: &str) -> Option<Date> {
    let value = name.strip_prefix("usage-")?.strip_suffix(".jsonl")?;
    if value.len() != 10 || value.as_bytes()[4] != b'-' || value.as_bytes()[7] != b'-' {
        return None;
    }
    if value
        .bytes()
        .enumerate()
        .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return None;
    }
    Date::from_calendar_date(
        value[..4].parse().ok()?,
        Month::try_from(value[5..7].parse::<u8>().ok()?).ok()?,
        value[8..].parse().ok()?,
    )
    .ok()
}

fn is_temporary_filename(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".tmp") else {
        return false;
    };
    let mut parts = stem.rsplitn(3, '.');
    let (Some(counter), Some(process), Some(date)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let decimal =
        |value: &str| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    decimal(counter)
        && decimal(process)
        && counter.parse::<u64>().is_ok()
        && process.parse::<u32>().is_ok_and(|process| process > 0)
        && date_from_filename(&format!("{date}.jsonl")).is_some()
}

fn check_directory(path: &Path) -> Result<bool, HistoryError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(HistoryError::UnsafePath);
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(HistoryError::UnsafePath),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn ensure_directory(path: &Path) -> Result<(), HistoryError> {
    if !check_directory(path)? {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
        if !check_directory(path)? {
            return Err(HistoryError::UnsafePath);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn checked_metadata(path: &Path) -> Result<Metadata, HistoryError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(HistoryError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(HistoryError::UnsafePath);
        }
    }
    Ok(metadata)
}

fn check_unchanged(path: &Path, expected: &Fingerprint) -> Result<(), HistoryError> {
    if Fingerprint::read(&checked_metadata(path)?)? != *expected {
        return Err(HistoryError::InvalidFile);
    }
    Ok(())
}

fn open_checked(path: &Path, expected: &Fingerprint, writable: bool) -> Result<File, HistoryError> {
    check_unchanged(path, expected)?;
    let file = OpenOptions::new().read(true).append(writable).open(path)?;
    if Fingerprint::read(&file.metadata()?)? != *expected {
        return Err(HistoryError::InvalidFile);
    }
    check_unchanged(path, expected)?;
    #[cfg(unix)]
    if writable {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn create_private(path: &Path) -> Result<File, HistoryError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> Result<bool, HistoryError> {
    line.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(count) > MAX_LINE_BYTES {
            return Err(HistoryError::TooLarge);
        }
        let complete = available[count - 1] == b'\n';
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if complete {
            return Ok(true);
        }
    }
}

fn parse_record(line: &[u8], date: Date) -> Result<HistoryRecord, HistoryError> {
    let value: serde_json::Value =
        serde_json::from_slice(line).map_err(|_| HistoryError::InvalidFile)?;
    if value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|version| version != 1)
    {
        return Err(HistoryError::UnsupportedVersion);
    }
    let record: HistoryRecord =
        serde_json::from_value(value).map_err(|_| HistoryError::InvalidFile)?;
    if record.validate().map_err(|_| HistoryError::InvalidFile)? != date {
        return Err(HistoryError::InvalidFile);
    }
    Ok(record)
}

fn scan(path: &Path, date: Date) -> Result<CachedFile, HistoryError> {
    let metadata = checked_metadata(path)?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(HistoryError::TooLarge);
    }
    let fingerprint = Fingerprint::read(&metadata)?;
    let file = open_checked(path, &fingerprint, false)?;
    let mut reader = BufReader::new(file);
    let mut info = StoreInfo {
        bytes: metadata.len(),
        ..StoreInfo::default()
    };
    let mut oldest: Option<i64> = None;
    let mut valid_bytes = 0;
    let mut tail = Tail::Complete;
    let mut line = Vec::new();
    while read_line(&mut reader, &mut line)? {
        let complete = line.last() == Some(&b'\n');
        if !complete
            && line.starts_with(b"{\"version\":1,")
            && serde_json::from_slice::<serde_json::Value>(&line).is_err_and(|error| error.is_eof())
        {
            tail = Tail::Interrupted { valid_bytes };
            break;
        }
        let record = parse_record(&line, date)?;
        info.records += 1;
        info.last_recorded_at = Some(
            info.last_recorded_at
                .map_or(record.observed_at, |last| last.max(record.observed_at)),
        );
        oldest = Some(oldest.map_or(record.observed_at, |first| first.min(record.observed_at)));
        valid_bytes += line.len() as u64;
        if !complete {
            tail = Tail::MissingNewline;
        }
    }
    check_unchanged(path, &fingerprint)?;
    Ok(CachedFile {
        fingerprint,
        info,
        tail,
        oldest,
    })
}

fn prune_file(
    path: &Path,
    fingerprint: &Fingerprint,
    cutoff: i64,
    date: Date,
) -> Result<(), HistoryError> {
    let input = open_checked(path, fingerprint, false)?;
    let mut reader = BufReader::new(input);
    let suffix = TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), suffix));
    let mut output = create_private(&temporary)?;
    let result = (|| -> Result<(), HistoryError> {
        let mut line = Vec::new();
        while read_line(&mut reader, &mut line)? {
            if parse_record(&line, date)?.observed_at >= cutoff {
                output.write_all(&line)?;
            }
        }
        output.sync_data()?;
        check_unchanged(path, fingerprint)?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let id = TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().canonicalize().unwrap();
            Self(root.join(format!("delta-v-history-{}-{id}", std::process::id())))
        }

        fn store(&self) -> Store {
            Store::new(self.0.clone())
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn reading(timestamp: i64) -> HistoryRecord {
        HistoryRecord {
            version: 1,
            provider: ProviderId::Claude,
            account_key: "a".repeat(64),
            observed_at: timestamp,
            limits: vec![HistoryLimit {
                id: "five_hour".into(),
                label: "5-hour".into(),
                used_fraction: 0.3,
                resets_at: Some(timestamp + 18_000),
                window_seconds: Some(18_000),
                provenance: Provenance::Official,
            }],
        }
    }

    const NOW: i64 = 1_790_856_000;

    fn path(directory: &Directory, timestamp: i64) -> PathBuf {
        directory.0.join(filename(
            OffsetDateTime::from_unix_timestamp(timestamp)
                .unwrap()
                .date(),
        ))
    }

    #[test]
    fn restores_counts_and_records_across_days_without_creating_a_folder_on_read() {
        let directory = Directory::new();
        let mut store = directory.store();
        assert_eq!(store.info().unwrap(), StoreInfo::default());
        assert!(!directory.0.exists());
        store
            .append(&reading(NOW - SECONDS_PER_DAY), Retention::Forever, NOW)
            .unwrap();
        let info = store
            .append(&reading(NOW), Retention::Forever, NOW)
            .unwrap();
        assert_eq!(info.records, 2);
        assert_eq!(info.last_recorded_at, Some(NOW));
        assert_eq!(directory.store().info().unwrap(), info);
        let record: HistoryRecord =
            serde_json::from_slice(&fs::read(path(&directory, NOW)).unwrap()).unwrap();
        assert_eq!(record, reading(NOW));
    }

    #[test]
    fn query_reader_sorts_records_and_uses_half_open_bounds_without_writing() {
        let directory = Directory::new();
        let mut store = directory.store();
        let mut empty = Vec::new();
        store
            .visit_records::<HistoryError>(
                None,
                NOW,
                || Ok(()),
                |record| {
                    empty.push(record);
                    Ok(())
                },
            )
            .unwrap();
        assert!(empty.is_empty());
        assert!(!directory.0.exists());

        for at in [NOW + 2, NOW - SECONDS_PER_DAY, NOW, NOW + 1, NOW] {
            store
                .append(&reading(at), Retention::Forever, NOW + 2)
                .unwrap();
        }
        let original = fs::read(path(&directory, NOW)).unwrap();
        let mut times = Vec::new();
        store
            .visit_records::<HistoryError>(
                Some(NOW),
                NOW + 2,
                || Ok(()),
                |record| {
                    times.push(record.observed_at);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(times, [NOW, NOW, NOW + 1]);
        assert_eq!(fs::read(path(&directory, NOW)).unwrap(), original);
    }

    #[test]
    fn query_reader_skips_unselected_days_but_refuses_corrupt_selected_data() {
        let directory = Directory::new();
        let mut store = directory.store();
        store
            .append(&reading(NOW), Retention::Forever, NOW)
            .unwrap();
        let older = path(&directory, NOW - SECONDS_PER_DAY);
        fs::write(&older, b"invalid\n").unwrap();
        let mut count = 0;
        store
            .visit_records::<HistoryError>(
                Some(NOW),
                NOW + 1,
                || Ok(()),
                |_| {
                    count += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(count, 1);
        assert!(matches!(
            store.visit_records::<HistoryError>(None, NOW + 1, || Ok(()), |_| Ok(())),
            Err(HistoryError::InvalidFile)
        ));
        assert_eq!(fs::read(&older).unwrap(), b"invalid\n");
    }

    #[test]
    fn query_reader_can_cancel_while_parsing_records_outside_selected_bounds() {
        let directory = Directory::new();
        let mut store = directory.store();
        for at in [NOW - 2, NOW - 1] {
            store.append(&reading(at), Retention::Forever, NOW).unwrap();
        }
        let file = path(&directory, NOW);
        let original = fs::read(&file).unwrap();
        let mut checks = 0;
        let mut visits = 0;
        let result = store.visit_records::<HistoryError>(
            Some(NOW),
            NOW + 1,
            || {
                checks += 1;
                // Cancel on the second line, after the first reading has been filtered out.
                if checks == 4 {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into());
                }
                Ok(())
            },
            |_| {
                visits += 1;
                Ok(())
            },
        );
        assert!(matches!(
            result,
            Err(HistoryError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted
        ));
        assert_eq!(checks, 4);
        assert_eq!(visits, 0);
        assert_eq!(fs::read(file).unwrap(), original);
    }

    #[test]
    fn query_reader_stops_at_initial_file_size_when_an_external_writer_appends() {
        let directory = Directory::new();
        let mut store = directory.store();
        store
            .append(&reading(NOW), Retention::Forever, NOW)
            .unwrap();
        let file = path(&directory, NOW);
        let original = fs::read(&file).unwrap();
        let appended = vec![b'x'; MAX_LINE_BYTES + 1];
        let mut checks = 0;
        let mut visits = 0;
        let result = store.visit_records::<HistoryError>(
            None,
            NOW + 1,
            || {
                checks += 1;
                if checks == 3 {
                    OpenOptions::new()
                        .append(true)
                        .open(&file)
                        .unwrap()
                        .write_all(&appended)
                        .unwrap();
                }
                Ok(())
            },
            |_| {
                visits += 1;
                Ok(())
            },
        );
        assert!(matches!(result, Err(HistoryError::InvalidFile)));
        assert_eq!(checks, 4);
        assert_eq!(visits, 0);
        let saved = fs::read(file).unwrap();
        assert_eq!(saved.len(), original.len() + appended.len());
        assert!(saved.starts_with(&original));
        assert!(saved.ends_with(&appended));
    }

    #[test]
    fn query_reader_leaves_interrupted_writes_and_future_formats_untouched() {
        for (tail, interrupted) in [
            (b"{\"version\":1,\"provider\":\"clau".as_slice(), true),
            (b"{\"version\":2,\"future\":true}\n".as_slice(), false),
        ] {
            let directory = Directory::new();
            let mut store = directory.store();
            store
                .append(&reading(NOW), Retention::Forever, NOW)
                .unwrap();
            let file = path(&directory, NOW);
            let mut original = fs::read(&file).unwrap();
            original.extend_from_slice(tail);
            fs::write(&file, &original).unwrap();
            let result = store.visit_records::<HistoryError>(None, NOW + 1, || Ok(()), |_| Ok(()));
            if interrupted {
                assert!(matches!(result, Err(HistoryError::InterruptedWrite)));
            } else {
                assert!(matches!(result, Err(HistoryError::UnsupportedVersion)));
            }
            assert_eq!(fs::read(&file).unwrap(), original);
        }
    }

    #[test]
    fn retention_prunes_exactly_at_the_cutoff_and_leaves_other_files() {
        let directory = Directory::new();
        let mut store = directory.store();
        let cutoff = NOW - 30 * SECONDS_PER_DAY;
        for timestamp in [
            cutoff - SECONDS_PER_DAY,
            cutoff - 1,
            cutoff,
            cutoff + 1,
            NOW,
        ] {
            store
                .append(&reading(timestamp), Retention::Forever, NOW)
                .unwrap();
        }
        fs::write(directory.0.join("notes.jsonl"), b"keep me").unwrap();
        assert_eq!(store.maintain(Retention::Days30, NOW).unwrap().records, 3);
        assert_eq!(directory.store().info().unwrap().records, 3);
        assert!(!path(&directory, cutoff - SECONDS_PER_DAY).exists());
        assert_eq!(
            fs::read_to_string(directory.0.join("notes.jsonl")).unwrap(),
            "keep me"
        );
        assert_eq!(store.clear().unwrap(), StoreInfo::default());
        assert!(directory.0.join("notes.jsonl").exists());
    }

    #[test]
    fn repairs_an_interrupted_tail_after_restart_and_keeps_complete_readings() {
        let directory = Directory::new();
        let mut store = directory.store();
        store
            .append(&reading(NOW), Retention::Forever, NOW)
            .unwrap();
        let saved = fs::read(path(&directory, NOW)).unwrap();
        OpenOptions::new()
            .append(true)
            .open(path(&directory, NOW))
            .unwrap()
            .write_all(b"{\"version\":1,\"provider\":\"claude\",\"account_key\":\"aa")
            .unwrap();
        let mut restarted = directory.store();
        assert!(matches!(
            restarted.info(),
            Err(HistoryError::InterruptedWrite)
        ));
        let next = reading(NOW + SECONDS_PER_DAY);
        assert_eq!(
            restarted
                .append(&next, Retention::Forever, NOW + SECONDS_PER_DAY)
                .unwrap()
                .records,
            2
        );
        assert_eq!(fs::read(path(&directory, NOW)).unwrap(), saved);
    }

    #[test]
    fn preserves_valid_final_record_without_a_newline() {
        let directory = Directory::new();
        ensure_directory(&directory.0).unwrap();
        fs::write(
            path(&directory, NOW),
            serde_json::to_vec(&reading(NOW)).unwrap(),
        )
        .unwrap();
        let mut store = directory.store();
        assert_eq!(
            store
                .append(&reading(NOW + 1), Retention::Forever, NOW + 1)
                .unwrap()
                .records,
            2
        );
        assert_eq!(
            fs::read_to_string(path(&directory, NOW))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    #[test]
    fn refuses_future_schemas_and_corrupt_lines_without_modifying_them() {
        for content in [
            b"{\"version\":2,\"future\":true}\n".as_slice(),
            b"{broken}\n",
            b"{\"version\":2,\"future\":",
            b"{",
        ] {
            let directory = Directory::new();
            ensure_directory(&directory.0).unwrap();
            fs::write(path(&directory, NOW), content).unwrap();
            let mut store = directory.store();
            assert!(store.append(&reading(NOW), Retention::Days30, NOW).is_err());
            assert_eq!(fs::read(path(&directory, NOW)).unwrap(), content);
            assert!(store.clear().is_ok());
        }
    }

    #[test]
    fn notices_external_edits_even_when_the_store_is_cached() {
        let directory = Directory::new();
        let mut store = directory.store();
        store
            .append(&reading(NOW), Retention::Forever, NOW)
            .unwrap();
        fs::write(path(&directory, NOW), b"invalid\n").unwrap();
        assert!(matches!(store.info(), Err(HistoryError::InvalidFile)));
        assert!(
            store
                .append(&reading(NOW + 1), Retention::Forever, NOW + 1)
                .is_err()
        );
    }

    #[test]
    fn rescans_valid_external_appends_before_updating_the_count() {
        let directory = Directory::new();
        let mut store = directory.store();
        store
            .append(&reading(NOW), Retention::Forever, NOW)
            .unwrap();
        let mut other = OpenOptions::new()
            .append(true)
            .open(path(&directory, NOW))
            .unwrap();
        serde_json::to_writer(&mut other, &reading(NOW + 1)).unwrap();
        other.write_all(b"\n").unwrap();
        assert_eq!(
            store
                .append(&reading(NOW + 2), Retention::Forever, NOW + 2)
                .unwrap()
                .records,
            3
        );
        assert_eq!(directory.store().info().unwrap(), store.info().unwrap());
    }

    #[test]
    fn cleans_orphan_compaction_files_without_removing_unrelated_temporaries() {
        for clear in [false, true] {
            let directory = Directory::new();
            let mut store = directory.store();
            store
                .append(&reading(NOW), Retention::Forever, NOW)
                .unwrap();
            let orphan = path(&directory, NOW).with_extension("12345.7.tmp");
            fs::write(&orphan, b"interrupted compaction").unwrap();
            let unrelated = [
                "notes.tmp",
                "usage-2026-10-01.tmp",
                "usage-2026-10-01.draft.1.tmp",
                "usage-2026-99-99.123.1.tmp",
            ];
            for name in unrelated {
                fs::write(directory.0.join(name), b"keep me").unwrap();
            }
            if clear {
                assert_eq!(store.clear().unwrap().records, 0);
            } else {
                assert_eq!(store.maintain(Retention::Days30, NOW).unwrap().records, 1);
            }
            assert!(!orphan.exists());
            for name in unrelated {
                assert_eq!(fs::read(directory.0.join(name)).unwrap(), b"keep me");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn linked_compaction_file_stops_cleanup_before_removing_saved_readings() {
        use std::os::unix::fs::symlink;
        let directory = Directory::new();
        let mut store = directory.store();
        store
            .append(&reading(NOW), Retention::Forever, NOW)
            .unwrap();
        let saved = fs::read(path(&directory, NOW)).unwrap();
        let orphan = path(&directory, NOW).with_extension("12345.7.tmp");
        symlink(path(&directory, NOW), &orphan).unwrap();
        assert!(matches!(store.clear(), Err(HistoryError::UnsafePath)));
        assert_eq!(fs::read(path(&directory, NOW)).unwrap(), saved);
        assert!(matches!(
            store.maintain(Retention::Days30, NOW),
            Err(HistoryError::UnsafePath)
        ));
        assert_eq!(fs::read(path(&directory, NOW)).unwrap(), saved);
    }

    #[test]
    fn rejects_invalid_readings_and_oversized_input() {
        let directory = Directory::new();
        let mut record = reading(NOW);
        record.limits[0].used_fraction = f64::NAN;
        assert!(matches!(
            directory.store().append(&record, Retention::Forever, NOW),
            Err(HistoryError::InvalidRecord)
        ));
        assert!(!directory.0.exists());
        ensure_directory(&directory.0).unwrap();
        fs::write(path(&directory, NOW), vec![b'x'; MAX_LINE_BYTES + 1]).unwrap();
        assert!(matches!(
            directory.store().info(),
            Err(HistoryError::TooLarge)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_files_private_and_refuses_links_during_append_and_clear() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = Directory::new();
        let mut store = directory.store();
        store
            .append(&reading(NOW), Retention::Forever, NOW)
            .unwrap();
        assert_eq!(
            fs::metadata(&directory.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path(&directory, NOW))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let external = directory.0.join("unrelated");
        fs::write(&external, b"untouched").unwrap();
        fs::remove_file(path(&directory, NOW)).unwrap();
        symlink(&external, path(&directory, NOW)).unwrap();
        assert!(matches!(
            store.append(&reading(NOW), Retention::Forever, NOW),
            Err(HistoryError::UnsafePath)
        ));
        assert!(matches!(store.clear(), Err(HistoryError::UnsafePath)));
        assert_eq!(fs::read(&external).unwrap(), b"untouched");
    }
}
