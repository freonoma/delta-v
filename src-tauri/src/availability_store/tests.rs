use super::*;
use crate::{
    model::{Limit, LimitKind, Provenance, ProviderId, ProviderSnapshot},
    notifications::availability::WatchState,
};

const ACCOUNT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "delta-v-availability-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join("availability-watches.json")
    }

    fn store(&self) -> Store {
        Store::new(Some(self.file()))
    }

    fn write(&self, bytes: &[u8]) {
        let path = self.file();
        if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        private_options()
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pending() -> PendingWatch {
    PendingWatch {
        provider: ProviderId::Claude,
        account_key: ACCOUNT.to_owned(),
        limit_id: "session".to_owned(),
        window_seconds: Some(18_000),
        armed_at: 100,
        last_observed_at: 90,
    }
}

fn saved() -> SavedWatches {
    SavedWatches {
        next_id: 2,
        entries: vec![SavedEntry {
            id: 1,
            label: "5-hour".to_owned(),
            watch: pending(),
        }],
    }
}

fn encoded(watches: SavedWatches) -> Vec<u8> {
    serde_json::to_vec(&Envelope {
        version: FILE_VERSION,
        watches,
    })
    .unwrap()
}

fn snapshot(at: i64, used: f64) -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Claude,
        limits: vec![Limit {
            id: "session".to_owned(),
            label: "5-hour".to_owned(),
            kind: LimitKind::Quota,
            used_fraction: Some(used),
            resets_at: Some(1000),
            window_seconds: Some(18_000),
            provenance: Provenance::Official,
            enabled: true,
            amount: None,
            detail: None,
        }],
        fetched_at: at,
        plan: None,
        warnings: Vec::new(),
        history_account: None,
        history_generation: None,
    }
}

#[test]
fn missing_store_and_empty_noop_create_nothing() {
    let directory = Directory::new();
    let path = directory.0.join("absent/availability-watches.json");
    let store = Store::new(Some(path));
    assert_eq!(store.load().unwrap(), SavedWatches::default());
    store
        .save(&SavedWatches::default(), &SavedWatches::default())
        .unwrap();
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    assert!(matches!(Store::new(None).load(), Err(Error::MissingHome)));
    assert!(matches!(
        Store::new(None).save(&SavedWatches::default(), &saved()),
        Err(Error::MissingHome)
    ));
}

#[test]
fn round_trip_updates_and_removal_preserve_id_sequence() {
    let directory = Directory::new();
    let initial = saved();
    directory
        .store()
        .save(&SavedWatches::default(), &initial)
        .unwrap();
    assert_eq!(directory.store().load().unwrap(), initial);
    let mut updated = initial.clone();
    updated.entries[0].watch.last_observed_at = 200;
    directory.store().save(&initial, &updated).unwrap();
    assert_eq!(directory.store().load().unwrap(), updated);
    let removed = SavedWatches {
        next_id: updated.next_id,
        entries: Vec::new(),
    };
    directory.store().save(&updated, &removed).unwrap();
    assert_eq!(directory.store().load().unwrap(), removed);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
    assert!(matches!(
        directory.store().save(&removed, &SavedWatches::default()),
        Err(Error::InvalidFile)
    ));
}

#[test]
fn restored_watch_preserves_account_and_watermark_without_reset_inference() {
    let directory = Directory::new();
    let mut watch = Watch::restore(&pending()).unwrap();
    assert!(watch.observe(&snapshot(150, 0.5), None, 150, 60).is_none());
    let persisted = SavedWatches {
        entries: vec![SavedEntry {
            watch: watch.pending().unwrap(),
            ..saved().entries.remove(0)
        }],
        ..saved()
    };
    directory
        .store()
        .save(&SavedWatches::default(), &persisted)
        .unwrap();
    let restored = directory.store().load().unwrap();
    let mut watch = Watch::restore(&restored.entries[0].watch).unwrap();
    assert_eq!(watch.state(), WatchState::Waiting);
    assert!(
        watch
            .observe(&snapshot(149, 0.5), Some(ACCOUNT), 151, 60)
            .is_none()
    );
    let mut elapsed = snapshot(152, 0.5);
    elapsed.limits[0].resets_at = Some(151);
    assert!(watch.observe(&elapsed, Some(ACCOUNT), 152, 60).is_none());
    assert!(
        watch
            .observe(&snapshot(153, 0.5), Some(ACCOUNT), 153, 60)
            .is_some()
    );
    assert!(watch.pending().is_none());
}

#[test]
fn pending_export_preserves_prearm_time_and_excludes_terminal_states() {
    let watch = Watch::arm(&snapshot(90, 1.0), Some(ACCOUNT), "session", 100, 60).unwrap();
    assert_eq!(watch.pending().unwrap(), pending());
    let mut restored = Watch::restore(&watch.pending().unwrap()).unwrap();
    assert_eq!(restored, watch);
    assert!(
        restored
            .observe(&snapshot(99, 0.5), Some(ACCOUNT), 99, 60)
            .is_none()
    );
    assert_eq!(restored.pending().unwrap(), pending());
    restored.cancel();
    assert!(restored.pending().is_none());
}

#[test]
fn restore_rejects_invalid_identity_window_and_clocks() {
    let mut invalid = Vec::new();
    for account in ["", "account", &"A".repeat(64)] {
        let mut watch = pending();
        watch.account_key = account.to_owned();
        invalid.push(watch);
    }
    for id in [
        "",
        " ",
        "session\n",
        "session:row:2",
        "additional:0:primary",
    ] {
        let mut watch = pending();
        watch.limit_id = id.to_owned();
        invalid.push(watch);
    }
    let mut long_id = pending();
    long_id.limit_id = "a".repeat(257);
    invalid.push(long_id);
    let mut zero_duration = pending();
    zero_duration.window_seconds = Some(0);
    invalid.push(zero_duration);
    for time in [i64::MIN, i64::MAX] {
        let mut armed = pending();
        armed.armed_at = time;
        invalid.push(armed);
        let mut observed = pending();
        observed.last_observed_at = time;
        invalid.push(observed);
    }
    for watch in invalid {
        assert!(Watch::restore(&watch).is_none(), "{watch:?}");
    }
    let mut wide = pending();
    wide.armed_at = time::Date::MIN.midnight().assume_utc().unix_timestamp();
    wide.last_observed_at = time::Date::MAX.midnight().assume_utc().unix_timestamp();
    wide.window_seconds = None;
    assert_eq!(Watch::restore(&wide).unwrap().pending(), Some(wide));
}

#[test]
fn invalid_saved_records_are_rejected_without_partial_restore() {
    let mut invalid = Vec::new();
    let mut zero_next = saved();
    zero_next.next_id = 0;
    invalid.push(zero_next);
    for id in [0, 2, u64::MAX] {
        let mut watches = saved();
        watches.entries[0].id = id;
        invalid.push(watches);
    }
    for label in ["", " \t", "bad\nlabel", &"a".repeat(257)] {
        let mut watches = saved();
        watches.entries[0].label = label.to_owned();
        invalid.push(watches);
    }
    let mut bad_watch = saved();
    bad_watch.entries[0].watch.account_key = "bad".to_owned();
    invalid.push(bad_watch);
    let directory = Directory::new();
    for watches in invalid {
        let bytes = encoded(watches.clone());
        directory.write(&bytes);
        assert!(matches!(directory.store().load(), Err(Error::InvalidFile)));
        assert!(matches!(
            directory.store().save(&SavedWatches::default(), &saved()),
            Err(Error::InvalidFile)
        ));
        assert_eq!(fs::read(directory.file()).unwrap(), bytes);
    }
}

#[test]
fn duplicate_record_ids_and_watch_identities_are_rejected() {
    let mut duplicate_id = saved();
    let mut other = duplicate_id.entries[0].clone();
    other.watch.provider = ProviderId::Codex;
    duplicate_id.entries.push(other);
    assert!(matches!(duplicate_id.validate(), Err(Error::InvalidFile)));
    let mut duplicate_watch = saved();
    let mut other = duplicate_watch.entries[0].clone();
    other.id = 2;
    duplicate_watch.next_id = 3;
    duplicate_watch.entries.push(other);
    assert!(matches!(
        duplicate_watch.validate(),
        Err(Error::InvalidFile)
    ));
    for change in 0..4 {
        let mut distinct = duplicate_watch.clone();
        let watch = &mut distinct.entries[1].watch;
        match change {
            0 => watch.provider = ProviderId::Codex,
            1 => watch.account_key = "b".repeat(64),
            2 => watch.limit_id = "weekly".to_owned(),
            _ => watch.window_seconds = None,
        }
        assert!(distinct.validate().is_ok());
    }
}

#[test]
fn record_limit_accepts_boundary_and_rejects_overflow() {
    let mut watches = SavedWatches {
        next_id: MAX_WATCHES as u64 + 1,
        entries: Vec::new(),
    };
    for index in 1..=MAX_WATCHES {
        let mut entry = saved().entries.remove(0);
        entry.id = index as u64;
        entry.watch.limit_id = format!("quota-{index}");
        watches.entries.push(entry);
    }
    let directory = Directory::new();
    directory
        .store()
        .save(&SavedWatches::default(), &watches)
        .unwrap();
    assert_eq!(directory.store().load().unwrap(), watches);
    let mut excessive = watches.clone();
    excessive.entries.push(saved().entries.remove(0));
    assert!(matches!(
        directory.store().save(&watches, &excessive),
        Err(Error::TooLarge)
    ));
    assert_eq!(directory.store().load().unwrap(), watches);
}

#[test]
fn malformed_unknown_fields_and_versions_are_preserved() {
    let directory = Directory::new();
    let mut unknown_envelope =
        serde_json::from_slice::<serde_json::Value>(&encoded(saved())).unwrap();
    unknown_envelope["extra"] = true.into();
    let mut unknown_saved = serde_json::from_slice::<serde_json::Value>(&encoded(saved())).unwrap();
    unknown_saved["watches"]["extra"] = true.into();
    let mut unknown_entry = serde_json::from_slice::<serde_json::Value>(&encoded(saved())).unwrap();
    unknown_entry["watches"]["entries"][0]["extra"] = true.into();
    let mut unknown_watch = serde_json::from_slice::<serde_json::Value>(&encoded(saved())).unwrap();
    unknown_watch["watches"]["entries"][0]["watch"]["resets_at"] = 1000.into();
    let invalid = vec![
        b"not json".to_vec(),
        vec![0xff],
        br#"{"version":0,"watches":{"next_id":1,"entries":[]}}"#.to_vec(),
        br#"{"version":1,"version":1,"watches":{"next_id":1,"entries":[]}}"#.to_vec(),
        serde_json::to_vec(&unknown_envelope).unwrap(),
        serde_json::to_vec(&unknown_saved).unwrap(),
        serde_json::to_vec(&unknown_entry).unwrap(),
        serde_json::to_vec(&unknown_watch).unwrap(),
    ];
    for bytes in invalid {
        directory.write(&bytes);
        assert!(matches!(directory.store().load(), Err(Error::InvalidFile)));
        assert!(matches!(
            directory.store().save(&SavedWatches::default(), &saved()),
            Err(Error::InvalidFile)
        ));
        assert_eq!(fs::read(directory.file()).unwrap(), bytes);
    }
    let future = br#"{"version":2,"future_format":true}"#;
    directory.write(future);
    assert!(matches!(
        directory.store().load(),
        Err(Error::UnsupportedVersion)
    ));
    assert!(matches!(
        directory.store().save(&SavedWatches::default(), &saved()),
        Err(Error::UnsupportedVersion)
    ));
    assert_eq!(fs::read(directory.file()).unwrap(), future);
}

#[test]
fn oversized_file_is_preserved() {
    let directory = Directory::new();
    let bytes = vec![b' '; MAX_FILE_BYTES as usize + 1];
    directory.write(&bytes);
    assert!(matches!(directory.store().load(), Err(Error::TooLarge)));
    assert!(matches!(
        directory.store().save(&SavedWatches::default(), &saved()),
        Err(Error::TooLarge)
    ));
    assert_eq!(fs::read(directory.file()).unwrap(), bytes);
}

#[test]
fn stale_writer_cannot_overwrite_another_instances_completion() {
    let directory = Directory::new();
    let first = directory.store();
    let second = directory.store();
    let original = saved();
    first.save(&SavedWatches::default(), &original).unwrap();
    let removed = SavedWatches {
        next_id: original.next_id,
        entries: Vec::new(),
    };
    first.save(&original, &removed).unwrap();
    assert!(matches!(
        second.save(&original, &removed),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        second.save(&original, &original),
        Err(Error::Conflict)
    ));
    assert_eq!(second.load().unwrap(), removed);
}

#[test]
fn held_lock_returns_busy_then_releases_on_drop() {
    let directory = Directory::new();
    let lock = acquire_lock(&lock_path(&directory.file())).unwrap();
    assert!(matches!(
        directory.store().save(&SavedWatches::default(), &saved()),
        Err(Error::Busy)
    ));
    assert!(!directory.file().exists());
    drop(lock);
    directory
        .store()
        .save(&SavedWatches::default(), &saved())
        .unwrap();
    assert!(lock_path(&directory.file()).exists());
    assert_eq!(directory.store().load().unwrap(), saved());
}

#[test]
fn failed_parent_sync_reports_committed_state_without_replaying_removal() {
    let directory = Directory::new();
    let original = saved();
    directory
        .store()
        .save(&SavedWatches::default(), &original)
        .unwrap();
    let removed = SavedWatches {
        next_id: original.next_id,
        entries: Vec::new(),
    };
    let error = directory
        .store()
        .save_with_sync(&original, &removed, |_| {
            Err(std::io::Error::other("synthetic directory sync failure"))
        })
        .unwrap_err();
    assert!(error.committed());
    assert_eq!(directory.store().load().unwrap(), removed);
    assert!(matches!(
        directory.store().save(&original, &removed),
        Err(Error::Conflict)
    ));
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn failure_before_replace_leaves_existing_file_untouched() {
    let directory = Directory::new();
    let original = saved();
    directory
        .store()
        .save(&SavedWatches::default(), &original)
        .unwrap();
    let bytes = fs::read(directory.file()).unwrap();
    let mut invalid = original.clone();
    invalid.entries[0].watch.armed_at = i64::MAX;
    let error = directory.store().save(&original, &invalid).unwrap_err();
    assert!(!error.committed());
    assert_eq!(fs::read(directory.file()).unwrap(), bytes);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn new_store_directory_and_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory::new();
    let parent = directory.0.join("private");
    let path = parent.join("availability-watches.json");
    Store::new(Some(path.clone()))
        .save(&SavedWatches::default(), &saved())
        .unwrap();
    assert_eq!(
        fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for path in [&path, &lock_path(&path)] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinks_hardlinks_directories_and_nonprivate_files_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = Directory::new();
    let outside = directory.0.join("other.json");
    private_options()
        .open(&outside)
        .unwrap()
        .write_all(&encoded(saved()))
        .unwrap();
    let bytes = fs::read(&outside).unwrap();
    for kind in 0..4 {
        let path = directory.file();
        match kind {
            0 => symlink(&outside, &path).unwrap(),
            1 => fs::hard_link(&outside, &path).unwrap(),
            2 => fs::create_dir(&path).unwrap(),
            _ => {
                fs::write(&path, &bytes).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            }
        }
        assert!(matches!(directory.store().load(), Err(Error::UnsafeFile)));
        assert!(matches!(
            directory.store().save(&SavedWatches::default(), &saved()),
            Err(Error::UnsafeFile)
        ));
        assert_eq!(fs::read(&outside).unwrap(), bytes);
        if kind == 2 {
            fs::remove_dir(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn unsafe_lock_file_is_never_followed_or_replaced() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let other = directory.0.join("other");
    fs::write(&other, b"unchanged").unwrap();
    let lock = lock_path(&directory.file());
    symlink(&other, &lock).unwrap();
    assert!(matches!(
        directory.store().save(&SavedWatches::default(), &saved()),
        Err(Error::UnsafeFile)
    ));
    assert_eq!(fs::read(&other).unwrap(), b"unchanged");
    assert!(!directory.file().exists());
}
