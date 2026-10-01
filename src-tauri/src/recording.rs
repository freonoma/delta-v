use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::Serialize;

use crate::{
    history::{HistoryLimit, HistoryRecord, Retention, Store, StoreInfo},
    history_query::{Accumulator, LimitKey, QueryError, QueryRequest, QueryResult},
    model::{LimitKind, Provenance, ProviderId, ProviderSnapshot},
    platform::calendar::Calendar,
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

#[derive(Debug, Serialize)]
pub struct SavedWindow {
    pub key: LimitKey,
    pub label: String,
    pub first_recorded_at: i64,
    pub last_recorded_at: i64,
}

#[derive(Debug, Serialize)]
pub struct SavedAccount {
    pub provider: ProviderId,
    pub account_key: String,
    pub first_recorded_at: i64,
    pub last_recorded_at: i64,
    pub windows: Vec<SavedWindow>,
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
        self.invalidate();
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

    pub fn catalog(&self, now: i64) -> Result<Vec<SavedAccount>, QueryError> {
        let epoch = self.epoch.load(Ordering::SeqCst);
        let inner = self.inner.lock().map_err(|_| QueryError::Unavailable)?;
        self.check_epoch(epoch)?;
        let store = inner.store.as_ref().ok_or(QueryError::Unavailable)?;
        let mut catalog = Catalog::default();
        store.visit_records(
            inner.view.retention.cutoff(now),
            now.saturating_add(1),
            || self.check_epoch(epoch),
            |record| catalog.observe(record),
        )?;
        self.check_epoch(epoch)?;
        Ok(catalog.finish())
    }

    pub fn query(
        &self,
        request: QueryRequest,
        now: i64,
        threshold: u8,
    ) -> Result<QueryResult, QueryError> {
        let epoch = self.epoch.load(Ordering::SeqCst);
        let calendar = Calendar::local()?;
        let mut query = Accumulator::new(request, calendar, now, f64::from(threshold))?;
        let inner = self.inner.lock().map_err(|_| QueryError::Unavailable)?;
        self.check_epoch(epoch)?;
        let store = inner.store.as_ref().ok_or(QueryError::Unavailable)?;
        let (from, until) = query.reader_boundaries();
        let from = from.max(inner.view.retention.cutoff(now));
        store.visit_records(
            from,
            until,
            || self.check_epoch(epoch),
            |record| query.observe(record),
        )?;
        let result = query.finish()?;
        self.check_epoch(epoch)?;
        Ok(result)
    }

    fn check_epoch(&self, expected: u64) -> Result<(), QueryError> {
        if self.epoch.load(Ordering::SeqCst) == expected {
            Ok(())
        } else {
            Err(QueryError::Cancelled)
        }
    }
}

#[derive(Default)]
struct Catalog {
    accounts: BTreeMap<(u8, String), AccountWindows>,
    window_count: usize,
}

struct AccountWindows {
    provider: ProviderId,
    first: i64,
    last: i64,
    windows: BTreeMap<(String, Option<u64>, u8), SavedWindow>,
}

impl Catalog {
    fn observe(&mut self, record: HistoryRecord) -> Result<(), QueryError> {
        let provider = match record.provider {
            ProviderId::Claude => 0,
            ProviderId::Codex => 1,
        };
        let key = (provider, record.account_key);
        if !self.accounts.contains_key(&key) && self.accounts.len() >= 256 {
            return Err(QueryError::TooLarge);
        }
        let account = self.accounts.entry(key).or_insert_with(|| AccountWindows {
            provider: record.provider,
            first: record.observed_at,
            last: record.observed_at,
            windows: BTreeMap::new(),
        });
        account.first = account.first.min(record.observed_at);
        account.last = account.last.max(record.observed_at);
        for limit in record.limits {
            let provenance = match limit.provenance {
                Provenance::Official => 0,
                Provenance::LocalEstimate => 1,
                Provenance::Unknown => 2,
            };
            let index = (limit.id.clone(), limit.window_seconds, provenance);
            if !account.windows.contains_key(&index) {
                if self.window_count >= 4096 {
                    return Err(QueryError::TooLarge);
                }
                self.window_count += 1;
            }
            let window = account.windows.entry(index).or_insert_with(|| SavedWindow {
                key: LimitKey {
                    id: limit.id,
                    window_seconds: limit.window_seconds,
                    provenance: limit.provenance,
                },
                label: limit.label.clone(),
                first_recorded_at: record.observed_at,
                last_recorded_at: record.observed_at,
            });
            window.first_recorded_at = window.first_recorded_at.min(record.observed_at);
            if record.observed_at > window.last_recorded_at {
                window.last_recorded_at = record.observed_at;
                window.label = limit.label;
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<SavedAccount> {
        let mut accounts: Vec<_> = self
            .accounts
            .into_iter()
            .map(|((_, account_key), saved)| SavedAccount {
                provider: saved.provider,
                account_key,
                first_recorded_at: saved.first,
                last_recorded_at: saved.last,
                windows: saved.windows.into_values().collect(),
            })
            .collect();
        accounts.sort_by(|left, right| {
            right
                .last_recorded_at
                .cmp(&left.last_recorded_at)
                .then_with(|| left.account_key.cmp(&right.account_key))
        });
        accounts
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

    #[test]
    fn queries_saved_readings_while_paused_without_creating_or_changing_files() {
        use crate::history_query::HistoryRange;
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        let now = 1_790_856_000;
        let request = QueryRequest {
            provider: ProviderId::Claude,
            account_key: "a".repeat(64),
            limit: LimitKey {
                id: "session".into(),
                window_seconds: Some(18_000),
                provenance: Provenance::Official,
            },
            range: HistoryRange::Today {},
        };
        assert!(recorder.catalog(now).unwrap().is_empty());
        assert!(
            recorder
                .query(request.clone(), now, 20)
                .unwrap()
                .points
                .is_empty()
        );
        assert!(!directory.0.exists());
        for at in [now - 1, now, now + 1] {
            recorder.record(&reading(&recorder, at), at).unwrap();
        }
        let mut other = reading(&recorder, now);
        other.history_account = Some("b".repeat(64));
        other.limits[0].used_fraction = Some(1.0);
        recorder.record(&other, now).unwrap();
        recorder.set_recording(false).unwrap();
        let file = fs::read_dir(&directory.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let original = fs::read(&file).unwrap();
        let result = recorder.query(request, now, 20).unwrap();
        assert_eq!(result.points.len(), 2);
        assert_eq!(result.observations.days_with_readings, 1);
        assert_eq!(result.observations.days_below_threshold, 0);
        assert_eq!(result.observations.peak.unwrap().used_fraction, 0.25);
        assert_eq!(recorder.catalog(now).unwrap().len(), 2);
        assert_eq!(fs::read(&file).unwrap(), original);
    }

    #[test]
    fn catalog_keeps_provider_and_window_variants_separate_and_uses_latest_labels() {
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        let mut first = reading(&recorder, 1000);
        let mut catalog = Catalog::default();
        let key = "a".repeat(64);
        catalog.observe(make_record(&first, &key)).unwrap();
        first.fetched_at = 1001;
        first.limits[0].label = "Renamed window".into();
        catalog.observe(make_record(&first, &key)).unwrap();
        first.fetched_at = 1002;
        first.limits[0].window_seconds = Some(36_000);
        let mut changed = make_record(&first, &key);
        catalog.observe(changed.clone()).unwrap();
        changed.limits[0].provenance = Provenance::LocalEstimate;
        catalog.observe(changed).unwrap();
        first.provider = ProviderId::Codex;
        first.fetched_at = 1003;
        catalog.observe(make_record(&first, &key)).unwrap();
        let accounts = catalog.finish();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].provider, ProviderId::Codex);
        let claude = &accounts[1];
        assert_eq!(claude.provider, ProviderId::Claude);
        assert_eq!(claude.first_recorded_at, 1000);
        assert_eq!(claude.last_recorded_at, 1002);
        assert_eq!(claude.windows.len(), 3);
        let original = claude
            .windows
            .iter()
            .find(|window| window.key.window_seconds == Some(18_000))
            .unwrap();
        assert_eq!(original.label, "Renamed window");
        assert_eq!(original.first_recorded_at, 1000);
        assert_eq!(original.last_recorded_at, 1001);
    }

    #[test]
    fn queries_apply_retention_even_before_the_next_cleanup() {
        use crate::history_query::HistoryRange;
        let directory = Directory::new();
        let recorder = directory.recorder(true);
        let now = 1_790_856_000;
        let cutoff = now - 30 * 86_400;
        for at in [cutoff - 1, cutoff, now] {
            recorder.record(&reading(&recorder, at), at).unwrap();
        }
        let restarted = Recorder::new(Some(directory.0.clone()), false, Retention::Days30);
        let accounts = restarted.catalog(now).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].first_recorded_at, cutoff);
        let result = restarted
            .query(
                QueryRequest {
                    provider: ProviderId::Claude,
                    account_key: accounts[0].account_key.clone(),
                    limit: accounts[0].windows[0].key.clone(),
                    range: HistoryRange::AllTime {},
                },
                now,
                20,
            )
            .unwrap();
        assert_eq!(
            result.days.iter().map(|day| day.sample_count).sum::<u64>(),
            2
        );
        assert_eq!(Store::new(directory.0.clone()).info().unwrap().records, 3);
    }
}
