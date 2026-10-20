use super::*;
use crate::model::{Limit, LimitKind, Provenance};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const ACCOUNT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ACCOUNT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "delta-v-watch-registry-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn store(&self) -> Store {
        Store::new(Some(self.0.join("watches.json")))
    }

    fn registry(&self) -> Registry {
        Registry::load(self.store())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn snapshot(at: i64, used: f64) -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Claude,
        limits: vec![Limit {
            id: "session".into(),
            label: "5-hour".into(),
            kind: LimitKind::Quota,
            used_fraction: Some(used),
            resets_at: Some(1000),
            window_seconds: Some(18_000),
            provenance: Provenance::Official,
            enabled: true,
            amount: None,
            detail: None,
        }],
        plan: None,
        fetched_at: at,
        warnings: Vec::new(),
        history_account: Some(ACCOUNT_A.into()),
        history_generation: None,
    }
}

fn armed(fixture: &Fixture) -> Registry {
    let mut registry = fixture.registry();
    assert_eq!(
        registry
            .arm(&snapshot(100, 1.0), "session", 100, 60)
            .unwrap(),
        "1"
    );
    registry
}

#[test]
fn explicit_arm_is_durable_idempotent_and_does_not_expose_account_keys() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    assert_eq!(
        registry
            .arm(&snapshot(101, 1.0), "session", 101, 60)
            .unwrap(),
        "1"
    );
    assert_eq!(registry.generation(), 1);
    let status = fixture.registry().status();
    assert_eq!(status.watches.len(), 1);
    assert_eq!(status.watches[0].armed_at, 100);
    let json = serde_json::to_string(&status).unwrap();
    assert!(!json.contains(ACCOUNT_A));
    assert!(!json.contains("account_key"));
    assert!(json.contains("\"id\":\"1\""));
}

#[test]
fn restored_watch_consumes_before_delivery_and_cannot_replay_after_failure_or_exit() {
    let fixture = Fixture::new();
    armed(&fixture);
    let mut restarted = fixture.registry();
    let recovered = snapshot(101, 0.5);
    let candidates = restarted.observe(&recovered, true, 101, 60).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].remaining_percent, 50.0);
    // Native submission has not occurred. A crash or failed send must not restore the watch.
    assert!(fixture.store().load().unwrap().entries.is_empty());
    drop(candidates);
    assert!(
        fixture
            .registry()
            .observe(&recovered, true, 101, 60)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn uncertain_durability_after_replacement_adopts_consumption_without_delivery() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    let result = registry.observe_with(
        &snapshot(101, 0.5),
        true,
        101,
        60,
        |store, expected, next| {
            store.save(expected, next)?;
            Err(StoreError::DurabilityAfterCommit(std::io::Error::other(
                "synthetic sync failure after replacement",
            )))
        },
    );
    assert!(result.is_err());
    assert!(registry.status().watches.is_empty());
    assert!(registry.status().error.is_some());
    assert!(fixture.store().load().unwrap().entries.is_empty());
    assert!(
        registry
            .observe(&snapshot(102, 0.5), true, 102, 60)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .registry()
            .observe(&snapshot(102, 0.5), true, 102, 60)
            .unwrap()
            .is_empty()
    );
    // The next explicit watch can be saved from the committed state without a false conflict.
    assert_eq!(
        registry
            .arm(&snapshot(103, 1.0), "session", 103, 60)
            .unwrap(),
        "2"
    );
}

#[test]
fn cancellation_survives_restart_and_invalidates_queued_generation() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    let queued_generation = registry.generation();
    registry.cancel("1").unwrap();
    assert_ne!(queued_generation, registry.generation());
    assert!(!fixture.registry().has_provider(ProviderId::Claude));
    let next = registry
        .arm(&snapshot(102, 1.0), "session", 102, 60)
        .unwrap();
    assert_eq!(next, "2");
}

#[test]
fn permission_loss_keeps_watch_pending_until_a_new_accepted_observation() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    assert!(
        registry
            .observe(&snapshot(101, 0.5), false, 101, 60)
            .unwrap()
            .is_empty()
    );
    assert!(fixture.registry().has_provider(ProviderId::Claude));
    assert_eq!(
        registry
            .observe(&snapshot(102, 0.5), true, 102, 60)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn another_account_does_not_retarget_a_watch_across_restart() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    let mut other = snapshot(101, 0.5);
    other.history_account = Some(ACCOUNT_B.into());
    assert!(registry.observe(&other, true, 101, 60).unwrap().is_empty());
    let mut restarted = fixture.registry();
    assert_eq!(
        fixture.store().load().unwrap().entries[0].watch.account_key,
        ACCOUNT_A
    );
    assert!(
        restarted
            .observe(&snapshot(100, 0.5), true, 101, 60)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        restarted
            .observe(&snapshot(102, 0.5), true, 102, 60)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn account_bindings_can_have_distinct_watches_for_the_same_quota() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    let mut other = snapshot(101, 1.0);
    other.history_account = Some(ACCOUNT_B.into());
    assert_eq!(registry.arm(&other, "session", 101, 60).unwrap(), "2");
    assert_eq!(
        registry
            .observe(&snapshot(102, 0.5), true, 102, 60)
            .unwrap()
            .len(),
        1
    );
    let saved = fixture.store().load().unwrap();
    assert_eq!(saved.entries.len(), 1);
    assert_eq!(saved.entries[0].watch.account_key, ACCOUNT_B);
}

#[test]
fn stale_elapsed_missing_and_unverified_readings_preserve_the_watch() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    assert!(
        registry
            .observe(&snapshot(101, 0.5), true, 200, 60)
            .unwrap()
            .is_empty()
    );
    let mut elapsed = snapshot(201, 0.5);
    elapsed.limits[0].resets_at = Some(201);
    assert!(
        registry
            .observe(&elapsed, true, 201, 60)
            .unwrap()
            .is_empty()
    );
    let mut missing = snapshot(202, 0.5);
    missing.limits[0].resets_at = None;
    assert!(
        registry
            .observe(&missing, true, 202, 60)
            .unwrap()
            .is_empty()
    );
    let mut unverified = snapshot(203, 0.5);
    unverified.history_account = None;
    assert!(
        registry
            .observe(&unverified, true, 203, 60)
            .unwrap()
            .is_empty()
    );
    assert!(fixture.registry().has_provider(ProviderId::Claude));
    assert_eq!(
        fixture.store().load().unwrap().entries[0]
            .watch
            .last_observed_at,
        203
    );
}

#[test]
fn write_failure_cannot_arm_or_cancel_or_deliver() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    let original = fixture.store().load().unwrap();
    // Replace the lock path with a directory to force a safe, deterministic write failure.
    let lock = fixture.0.join("watches.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    assert!(registry.cancel("1").is_err());
    let mut other = snapshot(101, 1.0);
    other.limits[0].id = "weekly".into();
    assert!(registry.arm(&other, "weekly", 101, 60).is_err());
    assert!(
        registry
            .observe(&snapshot(102, 0.5), true, 102, 60)
            .is_err()
    );
    assert_eq!(fixture.store().load().unwrap(), original);
    assert_eq!(registry.status().watches.len(), 1);
    assert!(registry.status().error.is_some());
    fs::remove_dir(&lock).unwrap();
    // A failed observation cannot be retried from an equal or older capture.
    assert!(
        registry
            .observe(&snapshot(102, 0.5), true, 102, 60)
            .unwrap()
            .is_empty()
    );
    assert!(
        registry
            .observe(&snapshot(101, 0.5), true, 102, 60)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        registry
            .observe(&snapshot(103, 0.5), true, 103, 60)
            .unwrap()
            .len(),
        1
    );
    assert!(registry.status().error.is_none());
}

#[test]
fn invalid_file_fails_closed_without_overwriting_it() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("watches.json"), b"not a watch store").unwrap();
    let mut registry = fixture.registry();
    assert!(registry.status().error.is_some());
    assert!(
        registry
            .arm(&snapshot(100, 1.0), "session", 100, 60)
            .is_err()
    );
    assert!(registry.cancel("1").is_err());
    assert!(
        registry
            .observe(&snapshot(101, 0.5), true, 101, 60)
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.0.join("watches.json")).unwrap(),
        b"not a watch store"
    );
}

#[test]
fn separate_instances_cannot_both_consume_the_same_watch() {
    let fixture = Fixture::new();
    let mut first = armed(&fixture);
    let mut second = fixture.registry();
    assert_eq!(
        first
            .observe(&snapshot(101, 0.5), true, 101, 60)
            .unwrap()
            .len(),
        1
    );
    assert!(second.observe(&snapshot(101, 0.5), true, 101, 60).is_err());
    assert!(fixture.store().load().unwrap().entries.is_empty());
}

#[test]
fn no_op_mutations_also_detect_another_instances_changes() {
    let fixture = Fixture::new();
    let mut first = armed(&fixture);
    let mut second = fixture.registry();
    first.cancel("1").unwrap();
    assert!(second.arm(&snapshot(101, 1.0), "session", 101, 60).is_err());
    let mut empty = fixture.registry();
    first.arm(&snapshot(102, 1.0), "session", 102, 60).unwrap();
    assert!(empty.cancel("2").is_err());
}

#[test]
fn successful_observations_do_not_invalidate_the_delivery_generation() {
    let fixture = Fixture::new();
    let mut registry = armed(&fixture);
    let generation = registry.generation();
    registry
        .observe(&snapshot(101, 0.5), true, 101, 60)
        .unwrap();
    assert_eq!(generation, registry.generation());
}
