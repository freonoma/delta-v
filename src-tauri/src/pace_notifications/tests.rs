use super::*;
use crate::model::{Limit, LimitKind, Provenance};

const START: i64 = 1_800_000_000;
const RESET: i64 = START + 500_000;
const ACCOUNT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn snapshot(at: i64, used: f64) -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Claude,
        plan: None,
        fetched_at: at,
        warnings: vec![],
        history_account: Some(ACCOUNT.into()),
        history_generation: None,
        limits: vec![Limit {
            id: "weekly".into(),
            label: "Weekly".into(),
            kind: LimitKind::Quota,
            used_fraction: Some(used),
            resets_at: Some(RESET),
            window_seconds: Some(604_800),
            provenance: Provenance::Official,
            enabled: true,
            amount: None,
            detail: None,
        }],
    }
}

fn evaluator() -> Evaluator {
    let mut evaluator = Evaluator::default();
    evaluator.set_policy(true, PaceNotificationScope::Weekly);
    evaluator
}

fn observe(evaluator: &mut Evaluator, at: i64, used: f64) -> Vec<PaceAlert> {
    evaluator.observe(&snapshot(at, used), Some(ACCOUNT), at, 330)
}

#[test]
fn silent_baseline_then_two_confirmations_five_minutes_apart_emit_once() {
    let mut evaluator = evaluator();
    assert!(observe(&mut evaluator, START, 0.6).is_empty());
    assert!(observe(&mut evaluator, START + 300, 0.6).is_empty());
    assert!(observe(&mut evaluator, START + 599, 0.6).is_empty());
    let events = observe(&mut evaluator, START + 600, 0.6);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].remaining_percent, 40.0);
    assert_eq!(events[0].limit_id, "weekly");
    for offset in [900, 1200, 1500] {
        assert!(observe(&mut evaluator, START + offset, 0.9).is_empty());
    }
}

#[test]
fn one_minute_checks_can_confirm_across_five_minutes_without_extra_polling() {
    let mut evaluator = evaluator();
    for offset in (0..=300).step_by(60) {
        let reading = snapshot(START + offset, 0.6);
        assert!(
            evaluator
                .observe(&reading, Some(ACCOUNT), reading.fetched_at, 90)
                .is_empty()
        );
    }
    let reading = snapshot(START + 360, 0.6);
    assert_eq!(
        evaluator
            .observe(&reading, Some(ACCOUNT), reading.fetched_at, 90)
            .len(),
        1
    );
}

#[test]
fn exact_ten_point_margin_is_quiet_and_a_lower_reading_breaks_confirmation() {
    let mut evaluator = evaluator();
    assert!(observe(&mut evaluator, START, 0.6).is_empty());
    assert!(observe(&mut evaluator, START + 300, 0.6).is_empty());
    let at = START + 600;
    let elapsed = (at - (RESET - 604_800)) as f64 / 604_800.0;
    assert!(observe(&mut evaluator, at, elapsed + 0.1).is_empty());
    assert!(observe(&mut evaluator, START + 900, 0.6).is_empty());
    assert_eq!(observe(&mut evaluator, START + 1200, 0.6).len(), 1);
}

#[test]
fn opening_window_settles_before_its_silent_baseline() {
    let mut evaluator = evaluator();
    let reset = START + 604_800;
    for offset in [0, 300, 600, 21_599, 21_600, 21_900] {
        let mut reading = snapshot(START + offset, 0.6);
        reading.limits[0].resets_at = Some(reset);
        assert!(
            evaluator
                .observe(&reading, Some(ACCOUNT), reading.fetched_at, 604_800)
                .is_empty()
        );
    }
    let mut reading = snapshot(START + 22_200, 0.6);
    reading.limits[0].resets_at = Some(reset);
    assert_eq!(
        evaluator
            .observe(&reading, Some(ACCOUNT), reading.fetched_at, 604_800)
            .len(),
        1
    );
}

#[test]
fn five_hour_windows_settle_after_ten_percent_and_require_all_scope() {
    let mut evaluator = evaluator();
    for offset in [0, 300, 1800, 2100, 2400] {
        let mut reading = snapshot(START + offset, 0.6);
        reading.limits[0].window_seconds = Some(18_000);
        reading.limits[0].resets_at = Some(START + 18_000);
        assert!(
            evaluator
                .observe(&reading, Some(ACCOUNT), reading.fetched_at, 2000)
                .is_empty()
        );
    }
    evaluator.set_policy(true, PaceNotificationScope::All);
    for offset in [2700, 3000, 3300] {
        let mut reading = snapshot(START + offset, 0.6);
        reading.limits[0].window_seconds = Some(18_000);
        reading.limits[0].resets_at = Some(START + 18_000);
        let events = evaluator.observe(&reading, Some(ACCOUNT), reading.fetched_at, 330);
        assert_eq!(events.len(), usize::from(offset == 3300));
    }
}

#[test]
fn exhausted_window_can_confirm_during_settling_but_still_needs_a_baseline() {
    let mut evaluator = evaluator();
    for offset in [0, 300, 600] {
        let mut reading = snapshot(START + offset, 1.0);
        reading.limits[0].resets_at = Some(START + 604_800);
        let events = evaluator.observe(&reading, Some(ACCOUNT), reading.fetched_at, 330);
        assert_eq!(events.len(), usize::from(offset == 600));
    }
}

#[test]
fn gaps_failures_and_policy_restores_require_new_confirmations() {
    for action in ["gap", "reset", "off", "scope", "identity"] {
        let mut evaluator = evaluator();
        assert!(observe(&mut evaluator, START, 0.6).is_empty());
        assert!(observe(&mut evaluator, START + 300, 0.6).is_empty());
        let next = if action == "gap" {
            START + 1000
        } else {
            START + 600
        };
        match action {
            "reset" => evaluator.reset_baseline(ProviderId::Claude),
            "off" => {
                evaluator.set_policy(false, PaceNotificationScope::Weekly);
                evaluator.set_policy(true, PaceNotificationScope::Weekly);
            }
            "scope" => evaluator.set_policy(true, PaceNotificationScope::All),
            "identity" => {
                evaluator.observe(&snapshot(START + 301, 0.6), None, START + 301, 330);
            }
            _ => {}
        }
        assert!(observe(&mut evaluator, next, 0.6).is_empty(), "{action}");
        assert!(
            observe(&mut evaluator, next + 300, 0.6).is_empty(),
            "{action}"
        );
        assert_eq!(
            observe(&mut evaluator, next + 600, 0.6).len(),
            1,
            "{action}"
        );
    }
}

#[test]
fn account_changes_rebaseline_and_returning_preserves_emitted_periods() {
    let mut evaluator = evaluator();
    for offset in [0, 300, 600] {
        observe(&mut evaluator, START + offset, 0.6);
    }
    for offset in [900, 1200, 1500] {
        let reading = snapshot(START + offset, 0.6);
        assert_eq!(
            evaluator
                .observe(&reading, Some(OTHER), reading.fetched_at, 330)
                .len(),
            usize::from(offset == 1500)
        );
    }
    for offset in [1800, 2100, 2400] {
        assert!(observe(&mut evaluator, START + offset, 0.6).is_empty());
    }
}

#[test]
fn duplicate_and_out_of_order_samples_cannot_count_as_confirmations() {
    let mut evaluator = evaluator();
    observe(&mut evaluator, START, 0.6);
    observe(&mut evaluator, START + 300, 0.6);
    for offset in [300, 299, 0] {
        assert!(observe(&mut evaluator, START + offset, 0.6).is_empty());
    }
    assert_eq!(observe(&mut evaluator, START + 600, 0.6).len(), 1);
}

#[test]
fn reset_jitter_preserves_episode_and_future_corrections_do_not_rearm_it() {
    let mut evaluator = evaluator();
    for (offset, jitter) in [(0, 0), (300, 1), (600, -1)] {
        let mut reading = snapshot(START + offset, 0.6);
        reading.limits[0].resets_at = Some(RESET + jitter);
        assert_eq!(
            evaluator
                .observe(&reading, Some(ACCOUNT), reading.fetched_at, 330)
                .len(),
            usize::from(offset == 600)
        );
    }
    for offset in [900, 1200, 1500] {
        let mut reading = snapshot(START + offset, 0.6);
        reading.limits[0].resets_at = Some(RESET + 60);
        assert!(
            evaluator
                .observe(&reading, Some(ACCOUNT), reading.fetched_at, 330)
                .is_empty()
        );
    }
    for offset in [0, 300, 600] {
        let mut reading = snapshot(RESET + 60 + 21_600 + offset, 0.6);
        reading.limits[0].resets_at = Some(RESET + 60 + 604_800);
        assert_eq!(
            evaluator
                .observe(&reading, Some(ACCOUNT), reading.fetched_at, 330)
                .len(),
            usize::from(offset == 600)
        );
    }
}

#[test]
fn missing_invalid_and_ambiguous_quotas_break_pending_confirmations() {
    for action in [
        "missing",
        "duplicate",
        "invalid",
        "unofficial",
        "positional",
        "no_window",
    ] {
        let mut evaluator = evaluator();
        observe(&mut evaluator, START, 0.6);
        observe(&mut evaluator, START + 300, 0.6);
        let mut reading = snapshot(START + 600, 0.6);
        match action {
            "missing" => reading.limits.clear(),
            "duplicate" => reading.limits.push(reading.limits[0].clone()),
            "invalid" => reading.limits[0].used_fraction = Some(f64::NAN),
            "unofficial" => reading.limits[0].provenance = Provenance::LocalEstimate,
            "positional" => reading.limits[0].id = "additional:0:primary_window".into(),
            "no_window" => reading.limits[0].window_seconds = None,
            _ => unreachable!(),
        }
        assert!(
            evaluator
                .observe(&reading, Some(ACCOUNT), reading.fetched_at, 330)
                .is_empty()
        );
        assert!(
            observe(&mut evaluator, START + 900, 0.6).is_empty(),
            "{action}"
        );
        assert!(
            observe(&mut evaluator, START + 1200, 0.6).is_empty(),
            "{action}"
        );
        assert_eq!(
            observe(&mut evaluator, START + 1500, 0.6).len(),
            1,
            "{action}"
        );
    }
}

#[test]
fn stale_future_and_expired_readings_never_alert() {
    let mut evaluator = evaluator();
    observe(&mut evaluator, START, 0.6);
    observe(&mut evaluator, START + 300, 0.6);
    let stale = snapshot(START + 600, 0.6);
    assert!(
        evaluator
            .observe(&stale, Some(ACCOUNT), START + 931, 330)
            .is_empty()
    );
    assert!(observe(&mut evaluator, START + 932, 0.6).is_empty());
    let future = snapshot(START + 1300, 0.6);
    assert!(
        evaluator
            .observe(&future, Some(ACCOUNT), START + 1200, 330)
            .is_empty()
    );
    let mut expired = snapshot(START + 1500, 0.6);
    expired.limits[0].resets_at = Some(START + 1500);
    assert!(
        evaluator
            .observe(&expired, Some(ACCOUNT), expired.fetched_at, 330)
            .is_empty()
    );
}
