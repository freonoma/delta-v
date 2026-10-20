use super::{ArmError, ExhaustedQuota, RecoveredAllowance, Watch, WatchState};
use crate::model::{Limit, LimitKind, Provenance, ProviderId, ProviderSnapshot};

const ACCOUNT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ACCOUNT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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
        plan: None,
        fetched_at: at,
        warnings: Vec::new(),
        history_account: None,
        history_generation: None,
    }
}

fn watch() -> Watch {
    Watch::arm(&snapshot(100, 1.0), Some(ACCOUNT_A), "session", 100, 60)
        .expect("eligible exhausted quota")
}

fn sample(watch: &mut Watch, at: i64, used: f64) -> Option<RecoveredAllowance> {
    watch.observe(&snapshot(at, used), Some(ACCOUNT_A), at, 60)
}

fn sibling(id: &str, label: &str) -> Limit {
    Limit {
        id: id.to_owned(),
        label: label.to_owned(),
        ..snapshot(101, 1.0).limits.remove(0)
    }
}

#[test]
fn arming_is_explicit_and_requires_exact_exhaustion() {
    let source = snapshot(100, 1.0);
    let original = source.clone();
    let armed = Watch::arm(&source, Some(ACCOUNT_A), "session", 100, 60);
    assert_eq!(armed.expect("exhausted").state(), WatchState::Waiting);
    assert_eq!(source, original);
    for used in [0.0, 0.5, 0.999, 0.999_999_999_999] {
        assert_eq!(
            Watch::arm(&snapshot(100, used), Some(ACCOUNT_A), "session", 100, 60),
            Err(ArmError::NotExhausted)
        );
    }
}

#[test]
fn arming_requires_a_verified_identity_without_borrowing_history_identity() {
    let mut source = snapshot(100, 1.0);
    source.history_account = Some(ACCOUNT_A.to_owned());
    source.history_generation = Some(42);
    for identity in [None, Some(""), Some("account"), Some(&ACCOUNT_A[..63])] {
        assert_eq!(
            Watch::arm(&source, identity, "session", 100, 60),
            Err(ArmError::UnverifiedAccount)
        );
    }
    let mut armed =
        Watch::arm(&source, Some(ACCOUNT_B), "session", 100, 60).expect("explicit identity");
    let mut recovered = snapshot(101, 0.5);
    recovered.history_account = Some(ACCOUNT_A.to_owned());
    assert!(
        armed
            .observe(&recovered, Some(ACCOUNT_B), 101, 60)
            .is_some()
    );
}

#[test]
fn arming_checks_freshness_and_rejects_invalid_clocks() {
    let source = snapshot(100, 1.0);
    assert!(Watch::arm(&source, Some(ACCOUNT_A), "session", 160, 60).is_ok());
    for now in [99, 161] {
        assert_eq!(
            Watch::arm(&source, Some(ACCOUNT_A), "session", now, 60),
            Err(ArmError::StaleReading)
        );
    }
    for invalid in [i64::MIN, i64::MAX] {
        assert_eq!(
            Watch::arm(&source, Some(ACCOUNT_A), "session", invalid, 60),
            Err(ArmError::InvalidClock)
        );
        assert!(Watch::arm(&snapshot(invalid, 1.0), Some(ACCOUNT_A), "session", 100, 60).is_err());
    }
}

#[test]
fn unavailable_quota_data_cannot_arm_a_watch() {
    let mut invalid = Vec::new();
    for used in [
        None,
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(-0.1),
        Some(1.1),
    ] {
        let mut limit = sibling("session", "5-hour");
        limit.used_fraction = used;
        invalid.push(limit);
    }
    for reset in [None, Some(99), Some(100), Some(i64::MAX)] {
        let mut limit = sibling("session", "5-hour");
        limit.resets_at = reset;
        invalid.push(limit);
    }
    for kind in [LimitKind::Spend, LimitKind::Credits, LimitKind::Unknown] {
        let mut limit = sibling("session", "5-hour");
        limit.kind = kind;
        invalid.push(limit);
    }
    for provenance in [Provenance::LocalEstimate, Provenance::Unknown] {
        let mut limit = sibling("session", "5-hour");
        limit.provenance = provenance;
        invalid.push(limit);
    }
    let mut disabled = sibling("session", "5-hour");
    disabled.enabled = false;
    invalid.push(disabled);
    let mut zero_window = sibling("session", "5-hour");
    zero_window.window_seconds = Some(0);
    invalid.push(zero_window);
    for label in ["", " ", "line\nbreak"] {
        invalid.push(sibling("session", label));
    }
    for limit in invalid {
        let mut source = snapshot(100, 1.0);
        source.limits = vec![limit];
        assert_eq!(
            Watch::arm(&source, Some(ACCOUNT_A), "session", 100, 60),
            Err(ArmError::UnavailableQuota)
        );
    }
    assert_eq!(
        Watch::arm(&snapshot(100, 1.0), Some(ACCOUNT_A), "missing", 100, 60),
        Err(ArmError::UnavailableQuota)
    );
}

#[test]
fn positional_or_ambiguous_quota_ids_cannot_arm_a_watch() {
    for id in ["additional:0:primary_window", "weekly:row:1", ""] {
        let mut source = snapshot(100, 1.0);
        source.limits[0].id = id.to_owned();
        assert_eq!(
            Watch::arm(&source, Some(ACCOUNT_A), id, 100, 60),
            Err(ArmError::UnavailableQuota)
        );
    }
    for duplicate_id in ["session", "session:row:1"] {
        let mut source = snapshot(100, 1.0);
        source.limits.push(sibling(duplicate_id, "Duplicate"));
        assert_eq!(
            Watch::arm(&source, Some(ACCOUNT_A), "session", 100, 60),
            Err(ArmError::UnavailableQuota)
        );
    }
}

#[test]
fn a_new_same_account_reading_confirms_only_the_watched_quota() {
    let mut armed = watch();
    let candidate = sample(&mut armed, 101, 0.875).expect("recovered quota");
    assert_eq!(
        candidate,
        RecoveredAllowance {
            provider: ProviderId::Claude,
            limit_id: "session".to_owned(),
            label: "5-hour".to_owned(),
            window_seconds: Some(18_000),
            remaining_percent: 12.5,
            resets_at: 1000,
            observed_at: 101,
            other_exhausted: Vec::new(),
        }
    );
    assert_eq!(armed.state(), WatchState::Completed);
}

#[test]
fn completed_watches_do_not_emit_again_or_rearm_after_exhaustion() {
    let mut armed = watch();
    assert!(sample(&mut armed, 101, 0.5).is_some());
    for (at, used) in [(101, 0.5), (102, 0.0), (103, 1.0), (104, 0.2)] {
        assert!(sample(&mut armed, at, used).is_none());
    }
    armed.cancel();
    assert_eq!(armed.state(), WatchState::Completed);
    let mut new_watch = Watch::arm(&snapshot(105, 1.0), Some(ACCOUNT_A), "session", 105, 60)
        .expect("new explicit watch");
    assert!(sample(&mut new_watch, 106, 0.5).is_some());
}

#[test]
fn cancellation_is_terminal_and_idempotent() {
    let mut armed = watch();
    armed.cancel();
    armed.cancel();
    assert_eq!(armed.state(), WatchState::Cancelled);
    assert!(sample(&mut armed, 101, 0.5).is_none());
    assert_eq!(armed.state(), WatchState::Cancelled);
}

#[test]
fn positive_remaining_must_survive_the_existing_numeric_precision() {
    let mut armed = watch();
    assert!(sample(&mut armed, 101, 0.999_999_999_999).is_none());
    assert_eq!(armed.state(), WatchState::Waiting);
    let candidate = sample(&mut armed, 102, 0.999_999_999_98).expect("positive remaining");
    assert!(candidate.remaining_percent > 0.0);
    assert!(candidate.remaining_percent < 0.001);
}

#[test]
fn a_reset_correction_without_recovered_usage_keeps_waiting() {
    let mut armed = watch();
    let mut corrected = snapshot(101, 1.0);
    corrected.limits[0].resets_at = Some(2000);
    assert!(
        armed
            .observe(&corrected, Some(ACCOUNT_A), 101, 60)
            .is_none()
    );
    corrected.fetched_at = 102;
    corrected.limits[0].used_fraction = Some(0.4);
    assert!(
        armed
            .observe(&corrected, Some(ACCOUNT_A), 102, 60)
            .is_some()
    );
}

#[test]
fn elapsed_reset_and_missing_future_reset_never_confirm_recovery() {
    let mut armed = watch();
    assert!(sample(&mut armed, 1000, 0.0).is_none());
    assert_eq!(armed.state(), WatchState::Waiting);
    let mut recovered = snapshot(1001, 0.0);
    recovered.limits[0].resets_at = Some(2000);
    assert!(
        armed
            .observe(&recovered, Some(ACCOUNT_A), 1001, 60)
            .is_some()
    );
}

#[test]
fn missing_or_invalid_target_data_preserves_the_watch() {
    let mut cases = Vec::new();
    let mut missing = snapshot(101, 0.5);
    missing.limits.clear();
    cases.push(missing);
    for reset in [None, Some(101), Some(i64::MAX)] {
        let mut source = snapshot(101, 0.5);
        source.limits[0].resets_at = reset;
        cases.push(source);
    }
    for used in [
        None,
        Some(f64::NAN),
        Some(f64::NEG_INFINITY),
        Some(-0.1),
        Some(1.1),
    ] {
        let mut source = snapshot(101, 0.5);
        source.limits[0].used_fraction = used;
        cases.push(source);
    }
    for source in cases {
        let mut armed = watch();
        assert!(armed.observe(&source, Some(ACCOUNT_A), 101, 60).is_none());
        assert_eq!(armed.state(), WatchState::Waiting);
        assert!(sample(&mut armed, 102, 0.5).is_some());
    }
}

#[test]
fn stale_future_and_invalid_clock_observations_do_not_mutate_the_watch() {
    for (at, now) in [(101, 162), (300, 200), (i64::MAX, 200), (101, i64::MIN)] {
        let mut armed = watch();
        let before = watch();
        assert!(
            armed
                .observe(&snapshot(at, 0.5), Some(ACCOUNT_A), now, 60)
                .is_none()
        );
        assert_eq!(armed, before);
        assert!(sample(&mut armed, 102, 0.5).is_some());
    }
}

#[test]
fn a_recovered_reading_at_the_freshness_boundary_is_eligible() {
    let mut armed = watch();
    assert!(
        armed
            .observe(&snapshot(101, 0.5), Some(ACCOUNT_A), 161, 60)
            .is_some()
    );
}

#[test]
fn confirmation_must_be_captured_after_arming_not_just_after_the_old_reading() {
    let mut armed = Watch::arm(&snapshot(100, 1.0), Some(ACCOUNT_A), "session", 120, 60)
        .expect("fresh exhaustion");
    for at in [99, 100, 110, 120] {
        assert!(
            armed
                .observe(&snapshot(at, 0.5), Some(ACCOUNT_A), 120, 60)
                .is_none()
        );
    }
    assert!(sample(&mut armed, 121, 0.5).is_some());
}

#[test]
fn repeated_and_out_of_order_responses_cannot_confirm_recovery() {
    let mut armed = watch();
    assert!(sample(&mut armed, 110, 1.0).is_none());
    assert!(sample(&mut armed, 109, 0.5).is_none());
    assert!(sample(&mut armed, 110, 0.5).is_none());
    assert!(sample(&mut armed, 111, 0.5).is_some());
}

#[test]
fn unavailable_target_advances_response_ordering_without_completing_the_watch() {
    let mut armed = watch();
    let mut unavailable = snapshot(110, 0.5);
    unavailable.limits.clear();
    assert!(
        armed
            .observe(&unavailable, Some(ACCOUNT_A), 110, 60)
            .is_none()
    );
    assert!(sample(&mut armed, 109, 0.5).is_none());
    assert!(sample(&mut armed, 110, 0.5).is_none());
    assert!(sample(&mut armed, 111, 0.5).is_some());
}

#[test]
fn unknown_identity_requires_a_new_verified_reading() {
    for identity in [None, Some("unverified")] {
        let mut armed = watch();
        assert!(
            armed
                .observe(&snapshot(110, 0.5), identity, 110, 60)
                .is_none()
        );
        assert_eq!(armed.state(), WatchState::Waiting);
        assert!(sample(&mut armed, 109, 0.5).is_none());
        assert!(sample(&mut armed, 110, 0.5).is_none());
        assert!(sample(&mut armed, 111, 0.5).is_some());
    }
}

#[test]
fn another_account_never_satisfies_or_retargets_the_watch() {
    let mut armed = watch();
    assert!(
        armed
            .observe(&snapshot(110, 0.5), Some(ACCOUNT_B), 110, 60)
            .is_none()
    );
    assert_eq!(armed.state(), WatchState::Waiting);
    assert!(sample(&mut armed, 109, 0.5).is_none());
    assert!(sample(&mut armed, 110, 0.5).is_none());
    assert!(sample(&mut armed, 111, 0.5).is_some());
}

#[test]
fn another_provider_cannot_change_the_watch_or_its_response_ordering() {
    let mut armed = watch();
    let before = watch();
    let mut other = snapshot(500, 0.0);
    other.provider = ProviderId::Codex;
    assert!(armed.observe(&other, Some(ACCOUNT_A), 500, 60).is_none());
    assert_eq!(armed, before);
    assert!(sample(&mut armed, 101, 0.5).is_some());
}

#[test]
fn quota_identity_and_window_length_must_match_the_armed_reading() {
    let mut cases = Vec::new();
    let mut renamed = snapshot(101, 0.5);
    renamed.limits[0].id = "weekly".to_owned();
    cases.push(renamed);
    for duration in [None, Some(0), Some(604_800)] {
        let mut changed = snapshot(101, 0.5);
        changed.limits[0].window_seconds = duration;
        cases.push(changed);
    }
    for source in cases {
        let mut armed = watch();
        assert!(armed.observe(&source, Some(ACCOUNT_A), 101, 60).is_none());
        assert!(sample(&mut armed, 102, 0.5).is_some());
    }
}

#[test]
fn an_unspecified_window_length_remains_bound_to_the_same_identity() {
    let mut source = snapshot(100, 1.0);
    source.limits[0].window_seconds = None;
    let mut armed =
        Watch::arm(&source, Some(ACCOUNT_A), "session", 100, 60).expect("unspecified window");
    assert!(sample(&mut armed, 101, 0.5).is_none());
    source.fetched_at = 102;
    source.limits[0].used_fraction = Some(0.5);
    assert!(armed.observe(&source, Some(ACCOUNT_A), 102, 60).is_some());
}

#[test]
fn ambiguous_recovered_rows_cannot_complete_the_watch() {
    for duplicate_id in ["session", "session:row:1"] {
        let mut armed = watch();
        let mut source = snapshot(101, 0.5);
        source.limits.push(sibling(duplicate_id, "Duplicate"));
        assert!(armed.observe(&source, Some(ACCOUNT_A), 101, 60).is_none());
        assert!(sample(&mut armed, 102, 0.5).is_some());
    }
}

#[test]
fn recovery_reports_other_exhausted_quotas_without_claiming_provider_availability() {
    let mut armed = watch();
    let mut recovered = snapshot(101, 0.5);
    recovered.limits.extend([
        sibling("weekly", "Weekly"),
        sibling("model", "Model allowance"),
    ]);
    let candidate = armed
        .observe(&recovered, Some(ACCOUNT_A), 101, 60)
        .expect("watched quota recovered");
    assert_eq!(candidate.limit_id, "session");
    assert_eq!(
        candidate.other_exhausted,
        vec![
            ExhaustedQuota {
                limit_id: "model".to_owned(),
                label: "Model allowance".to_owned(),
                window_seconds: Some(18_000),
            },
            ExhaustedQuota {
                limit_id: "weekly".to_owned(),
                label: "Weekly".to_owned(),
                window_seconds: Some(18_000),
            },
        ]
    );
}

#[test]
fn unavailable_or_ambiguous_siblings_do_not_become_confirmed_exhausted_quotas() {
    let mut armed = watch();
    let mut recovered = snapshot(101, 0.5);
    let mut expired = sibling("expired", "Expired");
    expired.resets_at = Some(101);
    let mut missing = sibling("missing", "Missing");
    missing.resets_at = None;
    let mut approximate = sibling("approximate", "Approximate");
    approximate.used_fraction = Some(0.999_999_999_999);
    let mut estimate = sibling("estimate", "Estimate");
    estimate.provenance = Provenance::LocalEstimate;
    let mut disabled = sibling("disabled", "Disabled");
    disabled.enabled = false;
    recovered.limits.extend([
        expired,
        missing,
        approximate,
        estimate,
        disabled,
        sibling("duplicate", "Duplicate"),
        sibling("duplicate", "Duplicate"),
        sibling("suffix", "Suffix"),
        sibling("suffix:row:1", "Suffix duplicate"),
        sibling("additional:0:primary_window", "Positional"),
    ]);
    let candidate = armed
        .observe(&recovered, Some(ACCOUNT_A), 101, 60)
        .expect("watched quota recovered");
    assert!(candidate.other_exhausted.is_empty());
}

#[test]
fn delayed_processing_cannot_confirm_a_quota_after_its_reported_reset_expires() {
    let mut source = snapshot(100, 1.0);
    source.limits[0].resets_at = Some(130);
    let mut armed = Watch::arm(&source, Some(ACCOUNT_A), "session", 100, 60)
        .expect("exhausted quota before reset");
    source.fetched_at = 101;
    source.limits[0].used_fraction = Some(0.5);
    assert!(armed.observe(&source, Some(ACCOUNT_A), 161, 60).is_none());
    assert_eq!(armed.state(), WatchState::Waiting);
    assert!(sample(&mut armed, 162, 0.5).is_some());
}

#[test]
fn named_codex_quotas_recover_with_current_labels_without_satisfying_claude_watches() {
    let mut source = snapshot(100, 1.0);
    source.provider = ProviderId::Codex;
    source.limits[0].id = "additional:code_review:primary_window".to_owned();
    source.limits[0].label = "Code review".to_owned();
    let mut codex = Watch::arm(
        &source,
        Some(ACCOUNT_A),
        "additional:code_review:primary_window",
        100,
        60,
    )
    .expect("named Codex quota");
    let mut claude_source = source.clone();
    claude_source.provider = ProviderId::Claude;
    let mut claude = Watch::arm(
        &claude_source,
        Some(ACCOUNT_A),
        "additional:code_review:primary_window",
        100,
        60,
    )
    .expect("same quota ID on another provider");
    source.fetched_at = 101;
    source.limits[0].used_fraction = Some(0.25);
    source.limits[0].label = "Code review allowance".to_owned();
    let candidate = codex
        .observe(&source, Some(ACCOUNT_A), 101, 60)
        .expect("Codex recovery");
    assert_eq!(candidate.provider, ProviderId::Codex);
    assert_eq!(candidate.limit_id, "additional:code_review:primary_window");
    assert_eq!(candidate.label, "Code review allowance");
    assert!(claude.observe(&source, Some(ACCOUNT_A), 101, 60).is_none());
    assert_eq!(claude.state(), WatchState::Waiting);
    source.provider = ProviderId::Claude;
    source.fetched_at = 102;
    assert!(claude.observe(&source, Some(ACCOUNT_A), 102, 60).is_some());
}

#[test]
fn wide_valid_clock_ranges_do_not_overflow_freshness_checks() {
    let earliest = time::Date::MIN.midnight().assume_utc().unix_timestamp();
    let latest = time::Date::MAX.midnight().assume_utc().unix_timestamp();
    let mut source = snapshot(earliest, 1.0);
    source.limits[0].resets_at = Some(latest);
    assert_eq!(
        Watch::arm(&source, Some(ACCOUNT_A), "session", latest - 1, 60),
        Err(ArmError::StaleReading)
    );
    let mut armed =
        Watch::arm(&source, Some(ACCOUNT_A), "session", earliest, 60).expect("valid early clock");
    source.fetched_at = earliest + 1;
    source.limits[0].used_fraction = Some(0.5);
    assert!(
        armed
            .observe(&source, Some(ACCOUNT_A), earliest + 1, 60)
            .is_some()
    );
}
