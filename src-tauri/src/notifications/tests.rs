use super::{Evaluator, LowAllowance};
use crate::model::{Limit, LimitKind, Provenance, ProviderId, ProviderSnapshot};

const ACCOUNT_A: &str = concat!(
    "aaaaaaaaaaaaaaaa",
    "aaaaaaaaaaaaaaaa",
    "aaaaaaaaaaaaaaaa",
    "aaaaaaaaaaaaaaaa"
);
const ACCOUNT_B: &str = concat!(
    "bbbbbbbbbbbbbbbb",
    "bbbbbbbbbbbbbbbb",
    "bbbbbbbbbbbbbbbb",
    "bbbbbbbbbbbbbbbb"
);

fn evaluator() -> Evaluator {
    let mut evaluator = Evaluator::default();
    assert!(evaluator.set_policy(true, 20).is_ok());
    evaluator
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
        plan: None,
        fetched_at: at,
        warnings: Vec::new(),
        history_account: None,
        history_generation: None,
    }
}

fn observe(evaluator: &mut Evaluator, snapshot: &ProviderSnapshot) -> Vec<LowAllowance> {
    evaluator.observe(snapshot, Some(ACCOUNT_A), snapshot.fetched_at, 60)
}

fn sample(evaluator: &mut Evaluator, at: i64, used: f64) -> Vec<LowAllowance> {
    observe(evaluator, &snapshot(at, used))
}

#[test]
fn disabled_by_default_and_enabling_starts_with_a_silent_baseline() {
    let mut evaluator = Evaluator::default();
    assert!(sample(&mut evaluator, 100, 0.2).is_empty());
    assert!(sample(&mut evaluator, 101, 0.9).is_empty());
    assert!(evaluator.set_policy(true, 20).is_ok());
    assert!(sample(&mut evaluator, 102, 0.9).is_empty());
    assert!(sample(&mut evaluator, 103, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 104, 0.9).len(), 1);
}

#[test]
fn crossing_returns_only_the_quota_details_needed_for_an_alert() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    let alerts = sample(&mut evaluator, 101, 0.875);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].provider, ProviderId::Claude);
    assert_eq!(alerts[0].limit_id, "session");
    assert_eq!(alerts[0].label, "5-hour");
    assert_eq!(alerts[0].remaining_percent, 12.5);
    assert_eq!(alerts[0].resets_at, 1000);
}

#[test]
fn first_low_reading_and_repeated_low_readings_are_silent() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.9).is_empty());
    assert!(sample(&mut evaluator, 101, 0.95).is_empty());
    assert!(sample(&mut evaluator, 102, 1.0).is_empty());
    assert!(sample(&mut evaluator, 103, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 104, 0.9).len(), 1);
}

#[test]
fn threshold_is_strict_and_matches_normalized_highlight_precision() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert!(sample(&mut evaluator, 101, 0.8).is_empty());
    assert!(sample(&mut evaluator, 102, 0.800_000_000_001).is_empty());
    assert_eq!(sample(&mut evaluator, 103, 0.800_000_000_02).len(), 1);
}

#[test]
fn exact_thresholds_never_alert_for_any_valid_threshold() {
    for threshold in 0..=100 {
        let mut evaluator = evaluator();
        assert!(evaluator.set_policy(true, threshold).is_ok());
        assert!(sample(&mut evaluator, 100, 0.0).is_empty());
        assert!(sample(&mut evaluator, 101, f64::from(100 - threshold) / 100.0).is_empty());
    }
}

#[test]
fn threshold_extremes_keep_zero_remaining_and_full_allowance_distinct() {
    let mut evaluator = evaluator();
    assert!(evaluator.set_policy(true, 0).is_ok());
    assert!(sample(&mut evaluator, 100, 0.0).is_empty());
    assert!(sample(&mut evaluator, 101, 1.0).is_empty());
    assert!(evaluator.set_policy(true, 100).is_ok());
    assert!(sample(&mut evaluator, 102, 0.0).is_empty());
    assert_eq!(sample(&mut evaluator, 103, 0.01).len(), 1);
}

#[test]
fn a_period_emits_only_once_even_after_recovery_and_recrossing() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
    assert!(sample(&mut evaluator, 102, 0.95).is_empty());
    assert!(sample(&mut evaluator, 103, 0.1).is_empty());
    assert!(sample(&mut evaluator, 104, 0.9).is_empty());
}

#[test]
fn invalid_policy_does_not_change_enabled_state_threshold_or_baseline() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert!(evaluator.set_policy(false, 101).is_err());
    assert!(sample(&mut evaluator, 101, 0.75).is_empty());
    assert_eq!(sample(&mut evaluator, 102, 0.9).len(), 1);
}

#[test]
fn reapplying_the_same_policy_does_not_erase_an_observed_crossing() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert!(evaluator.set_policy(true, 20).is_ok());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
}

#[test]
fn changing_threshold_rebaselines_without_synthesizing_a_crossing() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.75).is_empty());
    assert!(evaluator.set_policy(true, 30).is_ok());
    assert!(sample(&mut evaluator, 101, 0.75).is_empty());
    assert!(sample(&mut evaluator, 102, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 103, 0.75).len(), 1);
    assert!(evaluator.set_policy(true, 40).is_ok());
    assert!(sample(&mut evaluator, 104, 0.5).is_empty());
    assert!(sample(&mut evaluator, 105, 0.9).is_empty());
}

#[test]
fn disabling_and_reenabling_keeps_emitted_periods_and_ignores_disabled_samples() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
    assert!(evaluator.set_policy(false, 20).is_ok());
    assert!(sample(&mut evaluator, 900, 0.1).is_empty());
    assert!(evaluator.set_policy(true, 20).is_ok());
    assert!(sample(&mut evaluator, 102, 0.5).is_empty());
    assert!(sample(&mut evaluator, 103, 0.9).is_empty());
}

#[test]
fn repeated_or_older_provider_responses_do_not_change_the_baseline() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert!(sample(&mut evaluator, 100, 0.9).is_empty());
    assert!(sample(&mut evaluator, 99, 0.9).is_empty());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
}

#[test]
fn stale_or_future_responses_cannot_alert_or_advance_the_watermark() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert!(
        evaluator
            .observe(&snapshot(101, 0.9), Some(ACCOUNT_A), 162, 60)
            .is_empty()
    );
    assert!(
        evaluator
            .observe(&snapshot(300, 0.9), Some(ACCOUNT_A), 200, 60)
            .is_empty()
    );
    assert_eq!(sample(&mut evaluator, 201, 0.9).len(), 1);
}

#[test]
fn a_reading_at_the_maximum_age_is_still_eligible() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert_eq!(
        evaluator
            .observe(&snapshot(101, 0.9), Some(ACCOUNT_A), 161, 60)
            .len(),
        1
    );
}

#[test]
fn stale_missing_identity_does_not_invalidate_the_fresh_baseline() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert!(
        evaluator
            .observe(&snapshot(101, 0.9), None, 200, 60)
            .is_empty()
    );
    assert_eq!(sample(&mut evaluator, 201, 0.9).len(), 1);
}

#[test]
fn explicit_verified_identity_does_not_depend_on_history_recording_fields() {
    let mut evaluator = evaluator();
    let mut first = snapshot(100, 0.5);
    first.history_account = Some(ACCOUNT_B.to_owned());
    first.history_generation = Some(42);
    assert!(observe(&mut evaluator, &first).is_empty());
    let second = snapshot(101, 0.9);
    assert_eq!(observe(&mut evaluator, &second).len(), 1);
}

#[test]
fn missing_or_invalid_identity_rebaselines_without_borrowing_history_identity() {
    let invalid_keys = [
        None,
        Some(String::new()),
        Some("abc".to_owned()),
        Some("A".repeat(64)),
        Some("g".repeat(64)),
    ];
    for invalid in invalid_keys {
        let mut evaluator = evaluator();
        assert!(sample(&mut evaluator, 100, 0.5).is_empty());
        let mut unverified = snapshot(101, 0.9);
        unverified.history_account = Some(ACCOUNT_A.to_owned());
        assert!(
            evaluator
                .observe(&unverified, invalid.as_deref(), 101, 60)
                .is_empty()
        );
        assert!(sample(&mut evaluator, 102, 0.9).is_empty());
        assert!(sample(&mut evaluator, 103, 0.5).is_empty());
        assert_eq!(sample(&mut evaluator, 104, 0.9).len(), 1);
    }
}

#[test]
fn losing_identity_does_not_erase_a_previously_emitted_period() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
    assert!(
        evaluator
            .observe(&snapshot(102, 0.5), None, 102, 60)
            .is_empty()
    );
    assert!(sample(&mut evaluator, 103, 0.5).is_empty());
    assert!(sample(&mut evaluator, 104, 0.9).is_empty());
}

#[test]
fn accounts_are_separate_and_returning_to_one_requires_a_silent_baseline() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
    assert!(
        evaluator
            .observe(&snapshot(102, 0.9), Some(ACCOUNT_B), 102, 60)
            .is_empty()
    );
    assert!(
        evaluator
            .observe(&snapshot(103, 0.5), Some(ACCOUNT_B), 103, 60)
            .is_empty()
    );
    assert_eq!(
        evaluator
            .observe(&snapshot(104, 0.9), Some(ACCOUNT_B), 104, 60)
            .len(),
        1
    );
    assert!(sample(&mut evaluator, 105, 0.5).is_empty());
    assert!(sample(&mut evaluator, 106, 0.9).is_empty());
}

#[test]
fn account_round_trip_does_not_compare_against_the_old_accounts_high_reading() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert!(
        evaluator
            .observe(&snapshot(101, 0.5), Some(ACCOUNT_B), 101, 60)
            .is_empty()
    );
    assert!(sample(&mut evaluator, 102, 0.9).is_empty());
    assert!(sample(&mut evaluator, 103, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 104, 0.9).len(), 1);
}

#[test]
fn older_response_from_another_account_cannot_switch_the_active_account() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert!(
        evaluator
            .observe(&snapshot(99, 0.9), Some(ACCOUNT_B), 100, 60)
            .is_empty()
    );
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
}

#[test]
fn provider_baseline_resets_are_scoped_and_preserve_timestamp_ordering() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    let mut codex = snapshot(100, 0.5);
    codex.provider = ProviderId::Codex;
    assert!(observe(&mut evaluator, &codex).is_empty());
    evaluator.reset_baseline(ProviderId::Claude);
    assert!(sample(&mut evaluator, 99, 0.5).is_empty());
    assert!(sample(&mut evaluator, 101, 0.9).is_empty());
    codex.fetched_at = 101;
    codex.limits[0].used_fraction = Some(0.9);
    assert_eq!(observe(&mut evaluator, &codex).len(), 1);
    assert!(sample(&mut evaluator, 102, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 103, 0.9).len(), 1);
    evaluator.reset_baseline(ProviderId::Claude);
    assert!(sample(&mut evaluator, 104, 0.5).is_empty());
    assert!(sample(&mut evaluator, 105, 0.9).is_empty());
}

#[test]
fn each_valid_quota_crosses_independently_in_the_same_response() {
    let mut evaluator = evaluator();
    let mut first = snapshot(100, 0.5);
    let mut weekly = first.limits[0].clone();
    weekly.id = "weekly".to_owned();
    weekly.label = "Weekly".to_owned();
    weekly.window_seconds = Some(604_800);
    first.limits.push(weekly);
    assert!(observe(&mut evaluator, &first).is_empty());
    first.fetched_at = 101;
    for limit in &mut first.limits {
        limit.used_fraction = Some(0.9);
    }
    let alerts = observe(&mut evaluator, &first);
    let mut ids: Vec<_> = alerts.iter().map(|alert| alert.limit_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["session", "weekly"]);
}

#[test]
fn invalid_limit_samples_do_not_alert_or_destroy_the_last_valid_comparison() {
    let mut invalid_limits = Vec::new();
    for fraction in [
        None,
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(f64::NEG_INFINITY),
        Some(-0.1),
        Some(1.1),
    ] {
        let mut limit = snapshot(101, 0.9).limits.remove(0);
        limit.used_fraction = fraction;
        invalid_limits.push(limit);
    }
    for kind in [LimitKind::Spend, LimitKind::Credits, LimitKind::Unknown] {
        let mut limit = snapshot(101, 0.9).limits.remove(0);
        limit.kind = kind;
        invalid_limits.push(limit);
    }
    for provenance in [Provenance::LocalEstimate, Provenance::Unknown] {
        let mut limit = snapshot(101, 0.9).limits.remove(0);
        limit.provenance = provenance;
        invalid_limits.push(limit);
    }
    for reset in [None, Some(101), Some(100), Some(i64::MAX)] {
        let mut limit = snapshot(101, 0.9).limits.remove(0);
        limit.resets_at = reset;
        invalid_limits.push(limit);
    }
    let mut disabled = snapshot(101, 0.9).limits.remove(0);
    disabled.enabled = false;
    invalid_limits.push(disabled);
    let mut zero_window = snapshot(101, 0.9).limits.remove(0);
    zero_window.window_seconds = Some(0);
    invalid_limits.push(zero_window);
    for label in [
        "".to_owned(),
        " ".to_owned(),
        "line\nbreak".to_owned(),
        "x".repeat(257),
    ] {
        let mut limit = snapshot(101, 0.9).limits.remove(0);
        limit.label = label;
        invalid_limits.push(limit);
    }
    for invalid in invalid_limits {
        let mut evaluator = evaluator();
        assert!(sample(&mut evaluator, 100, 0.5).is_empty());
        let mut bad = snapshot(101, 0.9);
        bad.limits = vec![invalid];
        assert!(observe(&mut evaluator, &bad).is_empty());
        assert_eq!(sample(&mut evaluator, 102, 0.9).len(), 1);
    }
}

#[test]
fn missing_limits_and_reset_times_cannot_erase_an_emitted_period() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
    let mut missing = snapshot(102, 0.5);
    missing.limits.clear();
    assert!(observe(&mut evaluator, &missing).is_empty());
    let mut resetless = snapshot(103, 0.5);
    resetless.limits[0].resets_at = None;
    assert!(observe(&mut evaluator, &resetless).is_empty());
    assert!(sample(&mut evaluator, 104, 0.5).is_empty());
    assert!(sample(&mut evaluator, 105, 0.9).is_empty());
}

#[test]
fn missing_limits_preserve_the_last_valid_reading_but_advance_response_ordering() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    let mut missing = snapshot(200, 0.9);
    missing.limits.clear();
    assert!(observe(&mut evaluator, &missing).is_empty());
    assert!(
        evaluator
            .observe(&snapshot(150, 0.9), Some(ACCOUNT_A), 200, 60)
            .is_empty()
    );
    assert_eq!(sample(&mut evaluator, 201, 0.9).len(), 1);
}

#[test]
fn unstable_or_invalid_ids_never_become_alert_streams() {
    for id in [
        "".to_owned(),
        " ".to_owned(),
        "session\n".to_owned(),
        "x".repeat(257),
        "weekly:row:0".to_owned(),
        "session:row:1".to_owned(),
        "additional:0:primary_window".to_owned(),
    ] {
        let mut evaluator = evaluator();
        let mut reading = snapshot(100, 0.5);
        reading.limits[0].id = id;
        assert!(observe(&mut evaluator, &reading).is_empty());
        reading.fetched_at = 101;
        reading.limits[0].used_fraction = Some(0.9);
        assert!(observe(&mut evaluator, &reading).is_empty());
    }
}

#[test]
fn duplicate_ids_and_position_suffixed_duplicates_also_suppress_the_base_limit() {
    for suffix in ["session", "session:row:1"] {
        let mut evaluator = evaluator();
        assert!(sample(&mut evaluator, 100, 0.5).is_empty());
        let mut duplicated = snapshot(101, 0.9);
        let mut duplicate = duplicated.limits[0].clone();
        duplicate.id = suffix.to_owned();
        duplicate.used_fraction = Some(0.1);
        duplicated.limits.push(duplicate);
        assert!(observe(&mut evaluator, &duplicated).is_empty());
        duplicated.fetched_at = 102;
        duplicated.limits.reverse();
        assert!(observe(&mut evaluator, &duplicated).is_empty());
        assert_eq!(sample(&mut evaluator, 103, 0.9).len(), 1);
    }
}

#[test]
fn labels_can_change_without_changing_quota_identity() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    let mut renamed = snapshot(101, 0.9);
    renamed.limits[0].label = "Updated session name".to_owned();
    let alerts = observe(&mut evaluator, &renamed);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].label, "Updated session name");
}

#[test]
fn one_second_reset_jitter_does_not_break_a_crossing_or_rearm_an_alert() {
    for reset in [999, 1001] {
        let mut evaluator = evaluator();
        assert!(sample(&mut evaluator, 100, 0.5).is_empty());
        let mut low = snapshot(101, 0.9);
        low.limits[0].resets_at = Some(reset);
        assert_eq!(observe(&mut evaluator, &low).len(), 1);
        assert!(sample(&mut evaluator, 102, 0.5).is_empty());
        low.fetched_at = 103;
        assert!(observe(&mut evaluator, &low).is_empty());
    }
}

#[test]
fn reset_jitter_is_compared_with_the_fixed_anchor_not_the_previous_reading() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    let mut jitter = snapshot(101, 0.5);
    jitter.limits[0].resets_at = Some(1001);
    assert!(observe(&mut evaluator, &jitter).is_empty());
    let mut corrected = snapshot(102, 0.9);
    corrected.limits[0].resets_at = Some(1002);
    assert!(observe(&mut evaluator, &corrected).is_empty());
    corrected.fetched_at = 103;
    corrected.limits[0].used_fraction = Some(0.5);
    assert!(observe(&mut evaluator, &corrected).is_empty());
    corrected.fetched_at = 104;
    corrected.limits[0].used_fraction = Some(0.9);
    assert_eq!(observe(&mut evaluator, &corrected).len(), 1);
}

#[test]
fn a_reset_correction_before_expiry_rebaselines_and_preserves_emitted_state() {
    for reset in [900, 1100] {
        let mut evaluator = evaluator();
        assert!(sample(&mut evaluator, 100, 0.5).is_empty());
        assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
        let mut corrected = snapshot(102, 0.5);
        corrected.limits[0].resets_at = Some(reset);
        assert!(observe(&mut evaluator, &corrected).is_empty());
        corrected.fetched_at = 103;
        corrected.limits[0].used_fraction = Some(0.9);
        assert!(observe(&mut evaluator, &corrected).is_empty());
    }
}

#[test]
fn a_new_period_starts_silently_then_allows_one_new_crossing() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
    let mut next = snapshot(1001, 0.9);
    next.limits[0].resets_at = Some(2000);
    assert!(observe(&mut evaluator, &next).is_empty());
    next.fetched_at = 1002;
    assert!(observe(&mut evaluator, &next).is_empty());
    next.fetched_at = 1003;
    next.limits[0].used_fraction = Some(0.5);
    assert!(observe(&mut evaluator, &next).is_empty());
    next.fetched_at = 1004;
    next.limits[0].used_fraction = Some(0.9);
    assert_eq!(observe(&mut evaluator, &next).len(), 1);
}

#[test]
fn an_adopted_reset_correction_becomes_the_boundary_for_the_next_period() {
    let mut evaluator = evaluator();
    assert!(sample(&mut evaluator, 100, 0.5).is_empty());
    assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
    let mut corrected = snapshot(102, 0.5);
    corrected.limits[0].resets_at = Some(900);
    assert!(observe(&mut evaluator, &corrected).is_empty());
    corrected.fetched_at = 103;
    corrected.limits[0].used_fraction = Some(0.9);
    assert!(observe(&mut evaluator, &corrected).is_empty());
    let mut next = snapshot(901, 0.5);
    next.limits[0].resets_at = Some(2000);
    assert!(observe(&mut evaluator, &next).is_empty());
    next.fetched_at = 902;
    next.limits[0].used_fraction = Some(0.9);
    assert_eq!(observe(&mut evaluator, &next).len(), 1);
}

#[test]
fn delayed_processing_does_not_turn_a_pre_reset_correction_into_a_new_period() {
    let mut evaluator = evaluator();
    let mut reading = snapshot(10, 0.5);
    reading.limits[0].resets_at = Some(100);
    assert!(observe(&mut evaluator, &reading).is_empty());
    reading.fetched_at = 20;
    reading.limits[0].used_fraction = Some(0.9);
    assert_eq!(observe(&mut evaluator, &reading).len(), 1);
    reading.fetched_at = 95;
    reading.limits[0].used_fraction = Some(0.5);
    reading.limits[0].resets_at = Some(200);
    assert!(
        evaluator
            .observe(&reading, Some(ACCOUNT_A), 105, 30)
            .is_empty()
    );
    reading.fetched_at = 110;
    reading.limits[0].used_fraction = Some(0.9);
    assert!(observe(&mut evaluator, &reading).is_empty());
}

#[test]
fn duration_changes_require_a_new_baseline_without_rearming_the_same_period() {
    for already_emitted in [false, true] {
        let mut evaluator = evaluator();
        assert!(sample(&mut evaluator, 100, 0.5).is_empty());
        if already_emitted {
            assert_eq!(sample(&mut evaluator, 101, 0.9).len(), 1);
        }
        let mut changed = snapshot(102, 0.9);
        changed.limits[0].window_seconds = Some(3600);
        assert!(observe(&mut evaluator, &changed).is_empty());
        changed.fetched_at = 103;
        changed.limits[0].used_fraction = Some(0.5);
        assert!(observe(&mut evaluator, &changed).is_empty());
        changed.fetched_at = 104;
        changed.limits[0].used_fraction = Some(0.9);
        assert_eq!(
            observe(&mut evaluator, &changed).len(),
            usize::from(!already_emitted)
        );
    }
}
