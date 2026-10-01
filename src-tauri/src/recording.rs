use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::Serialize;

use crate::{
    history::{HistoryLimit, HistoryRecord, Retention, Store, StoreInfo},
    model::{LimitKind, Provenance, ProviderId, ProviderSnapshot},
};

#[derive(Clone, PartialEq, Serialize)]
pub struct ProviderIssue {
    provider: ProviderId,
    message: String,
}

#[derive(Clone, PartialEq, Serialize)]
pub struct HistoryState {
    pub recording: bool,
    pub retention: Retention,
    pub info: StoreInfo,
    pub error: Option<String>,
    pub provider_issues: Vec<ProviderIssue>,
}

struct Inner {
    view: HistoryState,
    store: Option<Store>,
    last_saved: [Option<(String, i64)>; 2],
}

pub struct Recorder {
    inner: Mutex<Inner>,
    // The low bit enables recording; the other bits invalidate pending requests.
    epoch: AtomicU64,
}

impl Recorder {
    pub fn new(directory: Option<PathBuf>, recording: bool, retention: Retention) -> Self {
        Self {
            inner: Mutex::new(Inner {
                view: HistoryState {
                    recording,
                    retention,
                    info: StoreInfo::default(),
                    error: directory.is_none().then(|| {
                        "The home directory is unavailable. History cannot be saved.".to_owned()
                    }),
                    provider_issues: Vec::new(),
                },
                store: directory.map(Store::new),
                last_saved: [None, None],
            }),
            epoch: AtomicU64::new(u64::from(recording)),
        }
    }

    pub fn ticket(&self) -> Option<u64> {
        let epoch = self.epoch.load(Ordering::SeqCst);
        (epoch & 1 == 1).then_some(epoch)
    }

    pub fn invalidate(&self) {
        self.epoch.fetch_add(2, Ordering::SeqCst);
    }

    pub fn clear_provider_issue(&self, provider: ProviderId) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .view
                .provider_issues
                .retain(|issue| issue.provider != provider);
        }
    }

    pub fn state(&self) -> Result<HistoryState, String> {
        self.with_state(Clone::clone)
    }

    pub fn with_state<T>(&self, read: impl FnOnce(&HistoryState) -> T) -> Result<T, String> {
        self.inner
            .lock()
            .map(|inner| read(&inner.view))
            .map_err(|_| "Could not read history settings.".to_owned())
    }

    pub fn set_recording(&self, enabled: bool) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not change history recording.")?;
        self.epoch
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                Some((value.wrapping_add(2) & !1) | u64::from(enabled))
            })
            .map_err(|_| "Could not change history recording.")?;
        inner.view.recording = enabled;
        inner.view.provider_issues.clear();
        Ok(())
    }

    pub fn set_retention(&self, retention: Retention, now: i64) -> Result<(), String> {
        self.invalidate();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not change history retention.")?;
        inner.view.retention = retention;
        maintain(&mut inner, now, true)
    }

    pub fn maintain(&self, now: i64) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not read local history.")?;
        maintain(&mut inner, now, true)
    }

    pub fn refresh(&self, now: i64) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not read local history.")?;
        maintain(&mut inner, now, false)
    }

    pub fn clear(&self) -> Result<(), String> {
        self.invalidate();
        let mut inner = self.inner.lock().map_err(|_| "Could not clear history.")?;
        let result = inner.store.as_mut().ok_or_else(missing_directory)?.clear();
        update_info(&mut inner, result.map_err(|error| error.to_string()))?;
        inner.last_saved = [None, None];
        Ok(())
    }

    pub fn record(&self, snapshot: &ProviderSnapshot, now: i64) -> Result<(), String> {
        let Some(ticket) = snapshot.history_generation else {
            return Ok(());
        };
        if self.ticket() != Some(ticket) {
            return Ok(());
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not save local history.")?;
        if self.ticket() != Some(ticket) {
            return Ok(());
        }
        let index = match snapshot.provider {
            ProviderId::Claude => 0,
            ProviderId::Codex => 1,
        };
        inner
            .view
            .provider_issues
            .retain(|issue| issue.provider != snapshot.provider);
        let Some(account_key) = &snapshot.history_account else {
            inner.view.provider_issues.push(ProviderIssue {
                provider: snapshot.provider,
                message:
                    "History is waiting for a verified account. Live usage is still available."
                        .to_owned(),
            });
            return Ok(());
        };
        let identity = (account_key.clone(), snapshot.fetched_at);
        if inner.last_saved[index].as_ref() == Some(&identity) {
            return Ok(());
        }
        let record = make_record(snapshot, account_key);
        if record.limits.is_empty() {
            return Ok(());
        }
        let retention = inner.view.retention;
        let result = inner
            .store
            .as_mut()
            .ok_or_else(missing_directory)?
            .append(&record, retention, now);
        update_info(&mut inner, result.map_err(|error| error.to_string()))?;
        inner.last_saved[index] = Some(identity);
        Ok(())
    }
}

fn missing_directory() -> String {
    "The home directory is unavailable. History cannot be saved.".to_owned()
}

fn maintain(inner: &mut Inner, now: i64, requested: bool) -> Result<(), String> {
    let store = inner.store.as_mut().ok_or_else(missing_directory)?;
    let result = if requested || inner.view.retention != Retention::Forever {
        store.maintain(inner.view.retention, now)
    } else {
        store.info()
    };
    update_info(inner, result.map_err(|error| error.to_string()))
}

fn update_info(inner: &mut Inner, result: Result<StoreInfo, String>) -> Result<(), String> {
    match result {
        Ok(info) => {
            inner.view.info = info;
            inner.view.error = None;
            Ok(())
        }
        Err(error) => {
            inner.view.error = Some(error.clone());
            Err(error)
        }
    }
}

fn make_record(snapshot: &ProviderSnapshot, account_key: &str) -> HistoryRecord {
    let ambiguous: Vec<&str> = snapshot
        .limits
        .iter()
        .filter_map(|limit| limit.id.split_once(":row:").map(|(base, _)| base))
        .collect();
    let limits = snapshot
        .limits
        .iter()
        .filter_map(|limit| {
            let used_fraction = limit.used_fraction?;
            if !limit.enabled
                || limit.kind != LimitKind::Quota
                || limit.provenance != Provenance::Official
                || !used_fraction.is_finite()
                || !(0.0..=1.0).contains(&used_fraction)
                || !stable_limit_id(&limit.id)
                || ambiguous.contains(&limit.id.as_str())
                || !valid_text(&limit.id)
                || !valid_text(&limit.label)
                || limit.window_seconds == Some(0)
                || limit
                    .resets_at
                    .is_some_and(|reset| time::OffsetDateTime::from_unix_timestamp(reset).is_err())
            {
                return None;
            }
            Some(HistoryLimit {
                id: limit.id.clone(),
                label: limit.label.clone(),
                used_fraction,
                resets_at: limit.resets_at,
                window_seconds: limit.window_seconds,
                provenance: limit.provenance,
            })
        })
        .collect();
    HistoryRecord {
        version: 1,
        provider: snapshot.provider,
        account_key: account_key.to_owned(),
        observed_at: snapshot.fetched_at,
        limits,
    }
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn stable_limit_id(id: &str) -> bool {
    // Position-based parser fallbacks can identify a different limit on the next poll.
    !id.split(':').any(|part| part == "row")
        && !id.strip_prefix("additional:").is_some_and(|suffix| {
            suffix
                .split(':')
                .next()
                .is_some_and(|part| part.parse::<usize>().is_ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Limit;
    use std::fs;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap();
            Self(root.join(format!(
                "delta-v-recording-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn recorder(&self, enabled: bool) -> Recorder {
            Recorder::new(Some(self.0.clone()), enabled, Retention::Forever)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn reading(recorder: &Recorder, at: i64) -> ProviderSnapshot {
        ProviderSnapshot {
            provider: ProviderId::Claude,
            plan: None,
            fetched_at: at,
            warnings: vec![],
            history_account: Some("a".repeat(64)),
            history_generation: recorder.ticket(),
            limits: vec![Limit {
                id: "session".into(),
                label: "5-hour".into(),
                kind: LimitKind::Quota,
                used_fraction: Some(0.25),
                resets_at: Some(at + 18000),
                window_seconds: Some(18000),
                provenance: Provenance::Official,
                enabled: true,
                amount: None,
                detail: None,
            }],
        }
    }

    #[test]
    fn recording_is_opt_in_and_does_not_write_cached_readings() {
        let directory = Directory::new();
        let recorder = directory.recorder(false);
        let cached = reading(&recorder, 1000);
        recorder.record(&cached, 1000).unwrap();
        recorder.maintain(1000).unwrap();
        assert!(!directory.0.exists());
        recorder.set_recording(true).unwrap();
        recorder.record(&cached, 1001).unwrap();
        assert!(!directory.0.exists());
        let fresh = reading(&recorder, 1002);
        recorder.record(&fresh, 1002).unwrap();
        recorder.record(&fresh, 1003).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 1);
    }

    #[test]
    fn pause_clear_and_disconnect_reject_in_flight_results() {
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        recorder.record(&reading(&recorder, 1000), 1000).unwrap();
        let pending = reading(&recorder, 1001);
        recorder.set_recording(false).unwrap();
        recorder.record(&pending, 1001).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 1);
        recorder.set_recording(true).unwrap();
        recorder.record(&pending, 1001).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 1);
        let pending = reading(&recorder, 1002);
        recorder.clear().unwrap();
        recorder.record(&pending, 1003).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 0);
        assert!(recorder.state().unwrap().recording);
        let pending = reading(&recorder, 1004);
        recorder.invalidate();
        recorder.record(&pending, 1005).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 0);
        recorder.record(&reading(&recorder, 1006), 1006).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 1);
    }

    #[test]
    fn missing_identity_skips_recording_and_a_verified_account_recovers() {
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        let mut snapshot = reading(&recorder, 1000);
        snapshot.history_account = None;
        recorder.record(&snapshot, 1000).unwrap();
        assert!(!directory.0.exists());
        assert_eq!(recorder.state().unwrap().provider_issues.len(), 1);
        recorder.clear_provider_issue(ProviderId::Claude);
        assert!(recorder.state().unwrap().provider_issues.is_empty());
        recorder.record(&reading(&recorder, 1001), 1001).unwrap();
        assert!(recorder.state().unwrap().provider_issues.is_empty());
        assert_eq!(recorder.state().unwrap().info.records, 1);
    }

    #[test]
    fn records_only_valid_official_quotas_without_response_details() {
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        let mut snapshot = reading(&recorder, 1000);
        snapshot.warnings.push("private warning".into());
        snapshot.plan = Some("private plan".into());
        snapshot.limits[0].detail = Some("private detail".into());
        for index in 0..7 {
            let mut invalid = snapshot.limits[0].clone();
            invalid.id = format!("invalid{index}");
            match index {
                0 => invalid.kind = LimitKind::Credits,
                1 => invalid.enabled = false,
                2 => invalid.provenance = Provenance::Unknown,
                3 => invalid.used_fraction = Some(f64::NAN),
                4 => invalid.id = "additional:0:primary".into(),
                5 => invalid.label = "invalid\nlabel".into(),
                _ => invalid.label = "x".repeat(257),
            }
            snapshot.limits.push(invalid);
        }
        let record = make_record(&snapshot, snapshot.history_account.as_ref().unwrap());
        assert_eq!(record.limits.len(), 1);
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("private"));
        let frontend = serde_json::to_string(&snapshot).unwrap();
        assert!(!frontend.contains("history_account"));
        assert!(!frontend.contains("history_generation"));
        recorder.record(&snapshot, 1000).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 1);
    }

    #[test]
    fn duplicate_scope_families_are_excluded_even_when_reordered() {
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        let mut snapshot = reading(&recorder, 1000);
        let mut first = snapshot.limits[0].clone();
        first.id = "weekly:model:fable".into();
        let mut second = first.clone();
        second.id.push_str(":row:2");
        second.used_fraction = Some(0.9);
        snapshot.limits.extend([first, second]);
        for _ in 0..2 {
            let record = make_record(&snapshot, "account");
            assert_eq!(record.limits.len(), 1);
            assert_eq!(record.limits[0].id, "session");
            snapshot.limits.swap(1, 2);
        }
    }

    #[test]
    fn retention_still_expires_readings_while_recording_is_paused() {
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        recorder.record(&reading(&recorder, 1000), 1000).unwrap();
        recorder.set_recording(false).unwrap();
        recorder.set_retention(Retention::Days30, 1000).unwrap();
        recorder.maintain(1001 + 30 * 86400).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 0);
        assert!(!recorder.state().unwrap().recording);
    }

    #[test]
    fn a_storage_failure_is_visible_and_retry_can_recover() {
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        fs::write(&directory.0, b"not a directory").unwrap();
        assert!(recorder.record(&reading(&recorder, 1000), 1000).is_err());
        assert!(recorder.state().unwrap().error.is_some());
        fs::remove_file(&directory.0).unwrap();
        recorder.refresh(1001).unwrap();
        assert!(recorder.state().unwrap().error.is_none());
        recorder.record(&reading(&recorder, 1002), 1002).unwrap();
        assert_eq!(recorder.state().unwrap().info.records, 1);
    }
}
