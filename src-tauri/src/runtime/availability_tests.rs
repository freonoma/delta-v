use super::*;
use crate::model::Limit;
use std::{path::PathBuf, sync::atomic::AtomicU64};

static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "delta-v-runtime-availability-{}-{}",
            std::process::id(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self { directory }
    }

    fn registry(&self) -> AvailabilityWatches {
        AvailabilityWatches::load(AvailabilityStore::new(Some(
            self.directory.join("watches.json"),
        )))
    }

    fn state(&self) -> Inner {
        let settings = Settings {
            providers: ProviderSelection::Codex,
            ..Settings::default()
        };
        Inner {
            view: AppState {
                revision: 0,
                settings,
                providers: PROVIDERS
                    .into_iter()
                    .map(|provider| ProviderState {
                        id: provider,
                        snapshot: Some(reading(provider, 100, 1.0)),
                        error: None,
                        recovery: None,
                        refreshing: false,
                        stale: false,
                        next_retry_at: None,
                    })
                    .collect(),
                settings_error: None,
                cooldown_error: None,
                paused: false,
            },
            schedules: [
                Schedule {
                    last_started: Some(100),
                    next_due: 400,
                    cadence: 300,
                    ..Schedule::default()
                },
                Schedule {
                    next_due: 1000,
                    cadence: 60,
                    ..Schedule::default()
                },
            ],
            notifications: Evaluator::default(),
            notification_generation: 0,
            notification_permission: None,
            availability: self.registry(),
            identity_demand: [false; 2],
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn reading(provider: ProviderId, fetched_at: i64, used_fraction: f64) -> ProviderSnapshot {
    ProviderSnapshot {
        provider,
        limits: vec![Limit {
            id: "session".into(),
            label: "5-hour".into(),
            kind: LimitKind::Quota,
            used_fraction: Some(used_fraction),
            resets_at: Some(2000),
            window_seconds: Some(18000),
            provenance: Provenance::Official,
            enabled: true,
            amount: None,
            detail: None,
        }],
        plan: None,
        fetched_at,
        warnings: Vec::new(),
        history_account: Some("a".repeat(64)),
        history_generation: None,
    }
}

fn active() -> Activity {
    Activity {
        idle_seconds: Some(0.0),
        locked: Some(false),
    }
}

fn arm(state: &mut Inner) -> String {
    arm_availability_watch(state, ProviderId::Claude, "session", 100).unwrap()
}

fn accept(state: &mut Inner, snapshot: ProviderSnapshot) -> AvailabilityTicket {
    let index = provider_index(snapshot.provider);
    let generation = state.schedules[index].generation;
    state.schedules[index].poll_generation = Some(generation);
    let now = snapshot.fetched_at;
    let accepted = finish_poll(state, index, generation, now, Ok(snapshot));
    availability_ticket(state, accepted.as_ref()).unwrap()
}

#[test]
fn restored_watches_monitor_hidden_connections_without_history_or_low_alerts() {
    let fixture = Fixture::new();
    arm(&mut fixture.state());
    let mut restarted = fixture.state();
    assert!(!restarted.view.settings.history_recording);
    assert!(!restarted.view.settings.notifications_enabled);
    assert!(!should_poll(&restarted.view.settings, ProviderId::Claude));
    assert!(should_poll_for(
        &restarted,
        &restarted.view.settings,
        ProviderId::Claude
    ));
    assert!(needs_notification_identity(&restarted, ProviderId::Claude));
    assert!(queue_polls(&mut restarted, active(), 399).is_empty());
    assert_eq!(
        queue_polls(&mut restarted, active(), 400),
        vec![(0, ProviderId::Claude, 0)]
    );
    change_connection(&mut restarted, 0, false, 401);
    assert!(!should_poll_for(
        &restarted,
        &restarted.view.settings,
        ProviderId::Claude
    ));
    assert!(restarted.availability.has_provider(ProviderId::Claude));
}

#[test]
fn identity_preparation_keeps_the_existing_schedule_and_service_wait() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    state.view.providers[0]
        .snapshot
        .as_mut()
        .unwrap()
        .history_account = None;
    state.schedules[0].blocked_until = 600;
    prepare_availability_watch(&mut state, ProviderId::Claude).unwrap();
    assert_eq!(state.schedules[0].next_due, 400);
    assert!(needs_notification_identity(&state, ProviderId::Claude));
    assert!(!needs_notification_identity(&state, ProviderId::Codex));
    assert!(arm_availability_watch(&mut state, ProviderId::Claude, "session", 100).is_err());
    assert!(state.identity_demand[0]);
    assert!(queue_polls(&mut state, active(), 599).is_empty());
    assert_eq!(
        queue_polls(&mut state, active(), 600),
        vec![(0, ProviderId::Claude, 0)]
    );
    let accepted = finish_poll(
        &mut state,
        0,
        0,
        600,
        Ok(reading(ProviderId::Claude, 600, 1.0)),
    );
    assert!(accepted.is_some());
    arm_availability_watch(&mut state, ProviderId::Claude, "session", 600).unwrap();
    assert!(!state.identity_demand[0]);
    assert!(needs_notification_identity(&state, ProviderId::Claude));
}

#[test]
fn preparation_does_not_connect_or_interrupt_recovery_and_disconnect_clears_demand() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    state.view.providers[0].recovery = Some(RecoveryStage::SigningIn);
    assert!(prepare_availability_watch(&mut state, ProviderId::Claude).is_err());
    assert!(!state.identity_demand[0]);
    state.view.providers[0].recovery = None;
    prepare_availability_watch(&mut state, ProviderId::Claude).unwrap();
    change_connection(&mut state, 0, false, 101);
    assert!(!state.identity_demand[0]);
    assert!(prepare_availability_watch(&mut state, ProviderId::Claude).is_err());
}

#[test]
fn arm_rejects_unavailable_current_readings() {
    for reason in 0..8 {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        match reason {
            0 => state.view.paused = true,
            1 => state.view.settings.claude_enabled = false,
            2 => state.view.providers[0].error = Some(ProviderError::Network.issue()),
            3 => state.view.providers[0].recovery = Some(RecoveryStage::Checking),
            4 => state.view.providers[0].snapshot = None,
            5 => {
                state.view.providers[0]
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .fetched_at = 101;
            }
            6 => {
                state.view.providers[0]
                    .snapshot
                    .as_mut()
                    .unwrap()
                    .fetched_at = -231;
            }
            _ => {
                state.view.providers[0].snapshot.as_mut().unwrap().limits[0].resets_at = Some(100);
            }
        }
        assert!(
            arm_availability_watch(&mut state, ProviderId::Claude, "session", 100).is_err(),
            "reason {reason}"
        );
        assert!(!state.availability.has_provider(ProviderId::Claude));
    }
}

#[test]
fn elapsed_sibling_does_not_prevent_arming_or_confirming_the_watched_quota() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    let snapshot = state.view.providers[0].snapshot.as_mut().unwrap();
    let mut sibling = snapshot.limits[0].clone();
    sibling.id = "weekly".into();
    sibling.resets_at = Some(100);
    snapshot.limits.push(sibling.clone());
    state.view.providers[0].stale = true;
    arm(&mut state);
    let mut recovered = reading(ProviderId::Claude, 101, 0.5);
    recovered.limits.push(sibling);
    let ticket = accept(&mut state, recovered);
    assert!(state.view.providers[0].stale);
    let candidates = availability_candidates(&mut state, &ticket, true, 101).unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].other_exhausted.is_empty());
}

#[test]
fn permission_blocks_consumption_until_a_new_confirmed_reading() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    arm(&mut state);
    let unavailable = accept(&mut state, reading(ProviderId::Claude, 101, 0.5));
    assert!(
        availability_candidates(&mut state, &unavailable, false, 101)
            .unwrap()
            .is_empty()
    );
    assert!(state.availability.has_provider(ProviderId::Claude));
    let restored = accept(&mut state, reading(ProviderId::Claude, 102, 0.4));
    assert_eq!(
        availability_candidates(&mut state, &restored, true, 102)
            .unwrap()
            .len(),
        1
    );
    assert!(availability_ticket_current(&state, &restored, 102));
    assert!(!fixture.registry().has_provider(ProviderId::Claude));
    assert!(
        availability_candidates(&mut state, &restored, true, 102)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unknown_or_other_accounts_do_not_retarget_a_watch() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    arm(&mut state);
    for (at, account) in [(101, None), (102, Some("b".repeat(64)))] {
        let mut snapshot = reading(ProviderId::Claude, at, 0.5);
        snapshot.history_account = account;
        let ticket = accept(&mut state, snapshot);
        assert!(
            availability_candidates(&mut state, &ticket, true, at)
                .unwrap()
                .is_empty()
        );
    }
    let original = accept(&mut state, reading(ProviderId::Claude, 103, 0.5));
    assert_eq!(
        availability_candidates(&mut state, &original, true, 103)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn watch_tickets_ignore_low_alert_policy_changes_but_reject_cancellation() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    let id = arm(&mut state);
    let ticket = accept(&mut state, reading(ProviderId::Claude, 101, 0.5));
    save_notifications_enabled(&mut state, true, 101, |_| Ok(())).unwrap();
    save_notifications_enabled(&mut state, false, 101, |_| Ok(())).unwrap();
    save_settings_change(
        &mut state,
        |current| {
            let mut changed = current.clone();
            changed.threshold = 15;
            Ok(changed)
        },
        101,
        |_| Ok(()),
    )
    .unwrap();
    update_notification_permission(&mut state, false).unwrap();
    update_notification_permission(&mut state, true).unwrap();
    assert!(availability_ticket_current(&state, &ticket, 101));
    state.availability.cancel(&id).unwrap();
    assert!(!availability_ticket_current(&state, &ticket, 101));
    assert!(
        availability_candidates(&mut state, &ticket, true, 101)
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.registry().has_provider(ProviderId::Claude));
}

#[test]
fn watch_tickets_reject_superseded_connections_readings_and_unavailable_state() {
    for reason in 0..8 {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        arm(&mut state);
        let ticket = accept(&mut state, reading(ProviderId::Claude, 101, 0.5));
        let mut now = 101;
        match reason {
            0 => state.view.paused = true,
            1 => state.view.providers[0].error = Some(ProviderError::Network.issue()),
            2 => state.view.providers[0].recovery = Some(RecoveryStage::SigningIn),
            3 => {
                state.view.providers[0].snapshot = Some(reading(ProviderId::Claude, 102, 0.5));
            }
            4 => {
                change_connection(&mut state, 0, false, 101);
                change_connection(&mut state, 0, true, 101);
            }
            5 => state.schedules[0].generation += 1,
            6 => now = 432,
            _ => now = 100,
        }
        assert!(
            !availability_ticket_current(&state, &ticket, now),
            "reason {reason}"
        );
        assert!(
            availability_candidates(&mut state, &ticket, true, now)
                .unwrap()
                .is_empty()
        );
        assert!(state.availability.has_provider(ProviderId::Claude));
    }
}

#[test]
fn failed_and_obsolete_responses_do_not_create_watch_tickets() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    assert!(availability_ticket(&state, state.view.providers[0].snapshot.as_ref()).is_none());
    arm(&mut state);
    state.schedules[0].poll_generation = Some(0);
    let failed = finish_poll(&mut state, 0, 0, 101, Err(ProviderError::Network));
    assert!(availability_ticket(&state, failed.as_ref()).is_none());
    state.schedules[0].poll_generation = Some(0);
    state.schedules[0].generation = 1;
    let obsolete = finish_poll(
        &mut state,
        0,
        0,
        102,
        Ok(reading(ProviderId::Claude, 102, 0.5)),
    );
    assert!(availability_ticket(&state, obsolete.as_ref()).is_none());
    assert!(state.availability.has_provider(ProviderId::Claude));
}

#[test]
fn arm_preflight_rejects_nonexhausted_readings_without_saving() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    state.view.providers[0].snapshot = Some(reading(ProviderId::Claude, 100, 0.999));
    assert!(availability_arm_ticket(&state, ProviderId::Claude, "session", 100).is_err());
    assert!(!fixture.registry().has_provider(ProviderId::Claude));
}

#[test]
fn permission_wait_cannot_move_an_arm_to_another_account_quota_or_connection() {
    for reason in 0..4 {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        let ticket = availability_arm_ticket(&state, ProviderId::Claude, "session", 100).unwrap();
        let mut snapshot = reading(ProviderId::Claude, 101, 1.0);
        match reason {
            0 => snapshot.history_account = Some("b".repeat(64)),
            1 => snapshot.history_account = None,
            2 => snapshot.limits[0].window_seconds = Some(604800),
            _ => state.schedules[0].generation += 1,
        }
        state.view.providers[0].snapshot = Some(snapshot);
        assert!(
            arm_availability_from_ticket(&mut state, &ticket, 101).is_err(),
            "reason {reason}"
        );
        assert!(!state.availability.has_provider(ProviderId::Claude));
        assert!(!fixture.registry().has_provider(ProviderId::Claude));
    }
}

#[test]
fn permission_wait_accepts_a_newer_exhausted_reading_but_rejects_recovery() {
    for recovered in [false, true] {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        let ticket = availability_arm_ticket(&state, ProviderId::Claude, "session", 100).unwrap();
        state.view.providers[0].snapshot = Some(reading(
            ProviderId::Claude,
            101,
            if recovered { 0.5 } else { 1.0 },
        ));
        let result = arm_availability_from_ticket(&mut state, &ticket, 101);
        assert_eq!(result.is_ok(), !recovered);
        assert_eq!(
            fixture.registry().has_provider(ProviderId::Claude),
            !recovered
        );
    }
}
