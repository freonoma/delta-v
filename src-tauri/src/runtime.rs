use std::{sync::Mutex, time::Duration};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use crate::{
    model::{LimitKind, Provenance, ProviderId, ProviderSnapshot},
    platform::{self, Activity},
    providers::{self, ClaudeProvider, CodexProvider, Provider, ProviderError},
    settings::{self, PercentageMode, Settings},
    tray,
};

const PROVIDERS: [ProviderId; 2] = [ProviderId::Claude, ProviderId::Codex];

#[derive(Clone, PartialEq, Serialize)]
pub struct ProviderState {
    pub id: ProviderId,
    pub snapshot: Option<ProviderSnapshot>,
    pub error: Option<String>,
    pub refreshing: bool,
    pub stale: bool,
    pub next_retry_at: Option<i64>,
}

#[derive(Clone, PartialEq, Serialize)]
pub struct AppState {
    pub settings: Settings,
    pub providers: Vec<ProviderState>,
    pub settings_error: Option<String>,
    pub paused: bool,
}

#[derive(Default)]
struct Schedule {
    next_due: i64,
    blocked_until: i64,
    last_started: i64,
    failures: u32,
    cadence: i64,
    waiting_for_sign_in: bool,
}

struct Inner {
    view: AppState,
    schedules: [Schedule; 2],
}

pub struct Runtime {
    inner: Mutex<Inner>,
    client: reqwest::Client,
    wake: Notify,
}

impl Runtime {
    pub fn new() -> Result<Self, reqwest::Error> {
        let (settings, settings_error) = match settings::load() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error.to_string())),
        };
        Ok(Self {
            inner: Mutex::new(Inner {
                view: AppState {
                    settings,
                    providers: PROVIDERS
                        .into_iter()
                        .map(|id| ProviderState {
                            id,
                            snapshot: None,
                            error: None,
                            refreshing: false,
                            stale: true,
                            next_retry_at: None,
                        })
                        .collect(),
                    settings_error,
                    paused: false,
                },
                schedules: Default::default(),
            }),
            client: providers::client()?,
            wake: Notify::new(),
        })
    }

    pub fn state(&self) -> Result<AppState, String> {
        self.inner
            .lock()
            .map(|inner| inner.view.clone())
            .map_err(|_| "Could not read the app state.".to_owned())
    }

    pub fn save(&self, settings: Settings) -> Result<AppState, String> {
        let mut inner = self.inner.lock().map_err(|_| "Could not save settings.")?;
        settings::save(&settings).map_err(|error| error.to_string())?;
        for (index, provider) in PROVIDERS.into_iter().enumerate() {
            if inner.view.settings.providers.includes(provider)
                != settings.providers.includes(provider)
            {
                let schedule = &mut inner.schedules[index];
                schedule.next_due = 0;
            } else if settings.refresh_seconds != inner.view.settings.refresh_seconds {
                inner.schedules[index].next_due = inner.schedules[index]
                    .next_due
                    .min(providers::now() + settings.refresh_seconds as i64);
            }
        }
        inner.view.settings = settings;
        inner.view.settings_error = None;
        let view = inner.view.clone();
        drop(inner);
        self.wake.notify_one();
        Ok(view)
    }
}

pub fn request_refresh(app: &AppHandle) {
    let runtime = app.state::<Runtime>();
    let view = if let Ok(mut inner) = runtime.inner.lock() {
        queue_manual_refresh(&mut inner);
        Some(inner.view.clone())
    } else {
        None
    };
    if let Some(view) = view {
        publish(app, &view);
    }
    runtime.wake.notify_one();
}

fn queue_manual_refresh(inner: &mut Inner) {
    for (state, schedule) in inner.view.providers.iter_mut().zip(&mut inner.schedules) {
        schedule.next_due = schedule.last_started.saturating_add(10);
        if schedule.waiting_for_sign_in {
            // A new CLI sign-in can recover immediately; server cooldowns still apply.
            schedule.blocked_until = 0;
            state.next_retry_at = Some(schedule.next_due);
        }
    }
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tick(&app, platform::activity(), providers::now());
            let runtime = app.state::<Runtime>();
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(1)) => {},
                () = runtime.wake.notified() => {},
            }
        }
    });
}

fn interval(settings: &Settings, idle_seconds: Option<f64>) -> i64 {
    let minimum = match idle_seconds {
        Some(idle) if idle < 300.0 => 0,
        Some(idle) if idle < 900.0 => 180,
        _ => 600,
    };
    (settings.refresh_seconds as i64).max(minimum)
}

fn tick(app: &AppHandle, activity: Activity, now: i64) {
    let runtime = app.state::<Runtime>();
    let mut pending = Vec::new();
    let view = {
        let Ok(mut inner) = runtime.inner.lock() else {
            return;
        };
        let before = inner.view.clone();
        let was_paused = inner.view.paused;
        inner.view.paused = activity.locked != Some(false);
        let cadence = interval(&inner.view.settings, activity.idle_seconds);
        for (index, provider) in PROVIDERS.into_iter().enumerate() {
            if !inner.view.settings.providers.includes(provider) {
                continue;
            }
            if was_paused && !inner.view.paused {
                inner.schedules[index].next_due = now;
            }
            let state = &mut inner.view.providers[index];
            state.stale = is_stale(state, now, cadence);
            if inner.schedules[index].cadence != cadence {
                inner.schedules[index].cadence = cadence;
                if inner.view.providers[index].error.is_none()
                    && let Some(snapshot) = &inner.view.providers[index].snapshot
                {
                    inner.schedules[index].next_due = next_refresh(snapshot, cadence, now);
                }
            }
            if inner.view.paused || inner.view.providers[index].refreshing {
                continue;
            }
            let schedule = &mut inner.schedules[index];
            if now < schedule.next_due.max(schedule.blocked_until) {
                continue;
            }
            schedule.last_started = now;
            inner.view.providers[index].refreshing = true;
            pending.push((index, provider));
        }
        (before != inner.view).then(|| inner.view.clone())
    };
    if let Some(view) = view {
        publish(app, &view);
    }
    for (index, provider) in pending {
        let app = app.clone();
        let client = runtime.client.clone();
        tauri::async_runtime::spawn(async move {
            let result = match provider {
                ProviderId::Claude => ClaudeProvider { client: &client }.fetch().await,
                ProviderId::Codex => CodexProvider { client: &client }.fetch().await,
            };
            finish(&app, index, result);
        });
    }
}

fn is_stale(state: &ProviderState, now: i64, cadence: i64) -> bool {
    state.error.is_some()
        || state.snapshot.as_ref().is_none_or(|snapshot| {
            now.saturating_sub(snapshot.fetched_at) > cadence + 30
                || snapshot.limits.iter().any(|limit| {
                    limit.enabled
                        && limit.kind == LimitKind::Quota
                        && limit.resets_at.is_some_and(|reset| reset <= now)
                })
        })
}

fn next_refresh(snapshot: &ProviderSnapshot, cadence: i64, now: i64) -> i64 {
    let periodic = snapshot.fetched_at.saturating_add(cadence);
    snapshot
        .limits
        .iter()
        .filter(|limit| limit.enabled && limit.kind == LimitKind::Quota)
        .filter_map(|limit| limit.resets_at)
        .filter(|reset| *reset > now)
        .min()
        .map_or(periodic, |reset| reset.saturating_add(1).min(periodic))
}

fn backoff(failures: u32, now: i64) -> i64 {
    (30_i64.saturating_mul(2_i64.saturating_pow(failures.saturating_sub(1).min(5)))).min(900)
        + now.rem_euclid(11)
}

fn finish(app: &AppHandle, index: usize, result: Result<ProviderSnapshot, ProviderError>) {
    let runtime = app.state::<Runtime>();
    let view = {
        let Ok(mut inner) = runtime.inner.lock() else {
            return;
        };
        let now = providers::now();
        let cadence = inner.schedules[index].cadence;
        apply_result(&mut inner, index, cadence, now, result);
        inner.view.clone()
    };
    publish(app, &view);
}

fn apply_result(
    inner: &mut Inner,
    index: usize,
    cadence: i64,
    now: i64,
    result: Result<ProviderSnapshot, ProviderError>,
) {
    let state = &mut inner.view.providers[index];
    let schedule = &mut inner.schedules[index];
    state.refreshing = false;
    match result {
        Ok(snapshot) => {
            schedule.next_due = next_refresh(&snapshot, cadence, now);
            schedule.failures = 0;
            schedule.blocked_until = 0;
            schedule.waiting_for_sign_in = false;
            state.error = None;
            state.next_retry_at = None;
            state.snapshot = Some(snapshot);
            state.stale = is_stale(state, now, cadence);
        }
        Err(error) => {
            schedule.failures = schedule.failures.saturating_add(1);
            schedule.waiting_for_sign_in = error.requires_sign_in();
            let delay = if schedule.waiting_for_sign_in {
                300
            } else {
                backoff(schedule.failures, now)
            };
            schedule.blocked_until = error.retry_at().unwrap_or(0).max(now + delay);
            schedule.next_due = schedule.blocked_until;
            if schedule.waiting_for_sign_in {
                state.snapshot = None;
            }
            state.error = Some(error.to_string());
            state.next_retry_at = Some(schedule.blocked_until);
            state.stale = true;
        }
    }
}

struct Reading {
    remaining: f64,
    stale: bool,
    label: String,
}

impl Reading {
    fn percentage(&self, mode: PercentageMode) -> f64 {
        match mode {
            PercentageMode::Remaining => self.remaining.ceil(),
            PercentageMode::Used => (((100.0 - self.remaining) * 1e9).round() / 1e9).floor(),
        }
    }
}

fn tray_reading(view: &AppState, now: i64) -> Option<Reading> {
    view.providers
        .iter()
        .filter(|state| view.settings.providers.includes(state.id))
        .flat_map(|state| {
            state.snapshot.iter().flat_map(move |snapshot| {
                snapshot.limits.iter().filter_map(move |limit| {
                    let provider = match state.id {
                        ProviderId::Claude => "claude",
                        ProviderId::Codex => "codex",
                    };
                    if !limit.enabled
                        || limit.kind != LimitKind::Quota
                        || limit.provenance != Provenance::Official
                        || limit.resets_at.is_some_and(|reset| reset <= now)
                        || (view.settings.tracked_limit != "auto"
                            && view.settings.tracked_limit != format!("{provider}:{}", limit.id))
                    {
                        return None;
                    }
                    let used = limit
                        .used_fraction
                        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))?;
                    let remaining = (((1.0 - used) * 100.0) * 1e9).round() / 1e9;
                    Some(Reading {
                        remaining,
                        stale: state.stale,
                        label: format!(
                            "{} · {}",
                            match state.id {
                                ProviderId::Claude => "Claude",
                                ProviderId::Codex => "Codex",
                            },
                            limit.label
                        ),
                    })
                })
            })
        })
        .min_by(|a, b| a.remaining.total_cmp(&b.remaining))
}

pub fn publish(app: &AppHandle, view: &AppState) {
    let reading = tray_reading(view, providers::now());
    let mode = view.settings.percentage_mode;
    let percentage_label = match mode {
        PercentageMode::Remaining => "remaining",
        PercentageMode::Used => "used",
    };
    let tooltip = reading.as_ref().map_or_else(
        || "Delta-V: usage unavailable".to_owned(),
        |reading| {
            format!(
                "Delta-V: {}% {}\n{}{}",
                reading.percentage(mode),
                percentage_label,
                reading.label,
                if reading.stale {
                    "\nLast reading, waiting for an update"
                } else {
                    ""
                }
            )
        },
    );
    if let Err(error) = tray::update(
        app,
        reading.as_ref().map(|reading| reading.percentage(mode)),
        reading.as_ref().is_some_and(|reading| reading.stale),
        &tooltip,
    ) {
        eprintln!("Could not update the menu bar: {error}");
    }
    if let Err(error) = app.emit("usage-updated", view) {
        eprintln!("Could not update the usage panel: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Limit;

    fn inner() -> Inner {
        Inner {
            view: AppState {
                settings: Settings::default(),
                providers: vec![ProviderState {
                    id: ProviderId::Claude,
                    snapshot: Some(ProviderSnapshot {
                        provider: ProviderId::Claude,
                        plan: None,
                        fetched_at: 100,
                        warnings: vec![],
                        limits: vec![Limit {
                            id: "session".into(),
                            label: "5-hour".into(),
                            kind: LimitKind::Quota,
                            used_fraction: Some(0.81),
                            resets_at: Some(200),
                            window_seconds: Some(18000),
                            provenance: Provenance::Official,
                            enabled: true,
                            amount: None,
                            detail: None,
                        }],
                    }),
                    error: None,
                    refreshing: false,
                    stale: false,
                    next_retry_at: None,
                }],
                settings_error: None,
                paused: false,
            },
            schedules: Default::default(),
        }
    }

    #[test]
    fn reset_expires_reading_without_inventing_a_refill() {
        let inner = inner();
        assert!(tray_reading(&inner.view, 199).is_some());
        assert!(tray_reading(&inner.view, 200).is_none());
        assert!(is_stale(&inner.view.providers[0], 200, 600));
    }

    #[test]
    fn tray_reports_remaining_and_keeps_missing_selection_unavailable() {
        let mut inner = inner();
        let reading = tray_reading(&inner.view, 150).unwrap();
        assert!((reading.remaining - 19.0).abs() < 0.00001);
        inner.view.settings.tracked_limit = "claude:missing".into();
        assert!(tray_reading(&inner.view, 150).is_none());
    }

    #[test]
    fn decimal_percentages_preserve_integer_boundaries() {
        let mut inner = inner();
        inner.view.providers[0].snapshot.as_mut().unwrap().limits[0].used_fraction = Some(0.58);
        assert_eq!(tray_reading(&inner.view, 150).unwrap().remaining, 42.0);
        inner.view.providers[0].snapshot.as_mut().unwrap().limits[0].used_fraction = Some(0.8);
        assert_eq!(tray_reading(&inner.view, 150).unwrap().remaining, 20.0);
    }

    #[test]
    fn used_and_remaining_percentages_stay_complementary_at_rounding_boundaries() {
        let mut inner = inner();
        for (used_fraction, expected_used) in [
            (0.0, 0.0),
            (0.005, 0.0),
            (0.01, 1.0),
            (0.58, 58.0),
            (0.8, 80.0),
            (0.999, 99.0),
            (1.0, 100.0),
        ] {
            inner.view.providers[0].snapshot.as_mut().unwrap().limits[0].used_fraction =
                Some(used_fraction);
            let reading = tray_reading(&inner.view, 150).unwrap();
            let used = reading.percentage(PercentageMode::Used);
            let remaining = reading.percentage(PercentageMode::Remaining);
            assert_eq!(used, expected_used);
            assert_eq!(used + remaining, 100.0);
        }
    }

    #[test]
    fn percentage_mode_does_not_change_the_tracked_quota() {
        let mut inner = inner();
        let snapshot = inner.view.providers[0].snapshot.as_mut().unwrap();
        let mut weekly = snapshot.limits[0].clone();
        weekly.id = "weekly".into();
        weekly.label = "Weekly".into();
        weekly.used_fraction = Some(0.95);
        snapshot.limits.push(weekly);

        for mode in [PercentageMode::Remaining, PercentageMode::Used] {
            inner.view.settings.percentage_mode = mode;
            let reading = tray_reading(&inner.view, 150).unwrap();
            assert_eq!(reading.label, "Claude · Weekly");
            assert_eq!(reading.remaining, 5.0);
        }

        inner.view.settings.tracked_limit = "claude:session".into();
        let reading = tray_reading(&inner.view, 150).unwrap();
        assert_eq!(reading.label, "Claude · 5-hour");
        assert_eq!(reading.percentage(PercentageMode::Used), 81.0);
        assert_eq!(inner.view.settings.threshold, 20);
    }

    #[test]
    fn returning_to_active_cadence_does_not_wait_for_idle_deadline() {
        let mut inner = inner();
        let snapshot = inner.view.providers[0].snapshot.as_mut().unwrap();
        snapshot.limits[0].resets_at = Some(1000);
        assert_eq!(next_refresh(snapshot, 600, 150), 700);
        assert_eq!(next_refresh(snapshot, 60, 150), 160);
        snapshot.limits[0].resets_at = Some(155);
        assert_eq!(next_refresh(snapshot, 60, 150), 156);
    }

    #[test]
    fn rate_limit_keeps_last_reading_and_honors_longer_retry_after() {
        let mut inner = inner();
        apply_result(
            &mut inner,
            0,
            60,
            150,
            Err(ProviderError::RateLimited {
                retry_at: Some(5000),
            }),
        );
        assert_eq!(inner.schedules[0].blocked_until, 5000);
        assert!(inner.view.providers[0].snapshot.is_some());
        assert!(inner.view.providers[0].stale);
        apply_result(&mut inner, 0, 60, 151, Err(ProviderError::Authentication));
        assert!(inner.view.providers[0].snapshot.is_none());
    }

    #[test]
    fn manual_check_can_recover_after_sign_in_without_waiting_five_minutes() {
        let mut inner = inner();
        inner.schedules[0].last_started = 100;
        apply_result(&mut inner, 0, 60, 101, Err(ProviderError::Authentication));
        assert_eq!(inner.schedules[0].blocked_until, 401);
        assert!(inner.schedules[0].waiting_for_sign_in);

        queue_manual_refresh(&mut inner);
        let schedule = &inner.schedules[0];
        assert_eq!(schedule.next_due.max(schedule.blocked_until), 110);
        assert_eq!(inner.view.providers[0].next_retry_at, Some(110));
        assert!(inner.view.providers[0].snapshot.is_none());
    }

    #[test]
    fn successful_auth_recovery_clears_failure_state_and_deadlines() {
        let mut inner = inner();
        let mut snapshot = inner.view.providers[0].snapshot.clone().unwrap();
        snapshot.fetched_at = 150;
        apply_result(&mut inner, 0, 60, 110, Err(ProviderError::Authentication));
        apply_result(&mut inner, 0, 60, 120, Err(ProviderError::Authentication));
        assert_eq!(inner.schedules[0].failures, 2);

        apply_result(&mut inner, 0, 60, 150, Ok(snapshot));
        let schedule = &inner.schedules[0];
        let state = &inner.view.providers[0];
        assert!(!schedule.waiting_for_sign_in);
        assert_eq!(schedule.failures, 0);
        assert_eq!(schedule.blocked_until, 0);
        assert_eq!(schedule.next_due, 201);
        assert!(state.error.is_none());
        assert!(state.next_retry_at.is_none());
        assert!(!state.stale);
        assert!(state.snapshot.is_some());
    }

    #[test]
    fn manual_checks_preserve_server_cooldowns_and_other_error_backoff() {
        for error in [
            ProviderError::RateLimited {
                retry_at: Some(5000),
            },
            ProviderError::AccessDenied {
                retry_at: Some(5000),
            },
            ProviderError::Service {
                status: 503,
                retry_at: Some(5000),
            },
            ProviderError::Network,
        ] {
            let mut inner = inner();
            inner.schedules[0].last_started = 100;
            apply_result(&mut inner, 0, 60, 101, Err(ProviderError::Authentication));
            apply_result(&mut inner, 0, 60, 102, Err(error));
            let blocked_until = inner.schedules[0].blocked_until;
            assert!(blocked_until > 110);
            assert!(!inner.schedules[0].waiting_for_sign_in);

            queue_manual_refresh(&mut inner);
            let schedule = &inner.schedules[0];
            assert_eq!(schedule.blocked_until, blocked_until);
            assert_eq!(schedule.next_due.max(schedule.blocked_until), blocked_until);
            assert_eq!(inner.view.providers[0].next_retry_at, Some(blocked_until));
        }
    }

    #[test]
    fn repeated_manual_checks_keep_ten_second_spacing_and_inflight_state() {
        let mut inner = inner();
        inner.schedules[0].last_started = 100;
        apply_result(&mut inner, 0, 60, 101, Err(ProviderError::Authentication));
        for _ in 0..3 {
            queue_manual_refresh(&mut inner);
            assert_eq!(inner.schedules[0].next_due, 110);
        }

        inner.schedules[0].last_started = 110;
        inner.view.providers[0].refreshing = true;
        for _ in 0..3 {
            queue_manual_refresh(&mut inner);
            assert_eq!(inner.schedules[0].next_due, 120);
            assert!(inner.view.providers[0].refreshing);
        }
    }

    #[test]
    fn idle_tiers_and_backoff_are_bounded() {
        let settings = Settings::default();
        assert_eq!(interval(&settings, Some(0.0)), 60);
        assert_eq!(interval(&settings, Some(400.0)), 180);
        assert_eq!(interval(&settings, None), 600);
        assert!(backoff(2, 0) > backoff(1, 0));
        assert!(backoff(u32::MAX, 100) <= 910);
    }
}
