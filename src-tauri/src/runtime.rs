use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use crate::{
    credentials,
    model::{LimitKind, Provenance, ProviderId, ProviderSnapshot},
    platform::{self, Activity},
    providers::{self, ClaudeProvider, CodexProvider, Issue, IssueKind, Provider, ProviderError},
    settings::{self, PercentageMode, Settings},
    tray,
};

const PROVIDERS: [ProviderId; 2] = [ProviderId::Claude, ProviderId::Codex];

#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStage {
    Renewing,
    SigningIn,
    Checking,
    Cancelling,
}

#[derive(Clone, PartialEq, Serialize)]
pub struct ProviderState {
    pub id: ProviderId,
    pub snapshot: Option<ProviderSnapshot>,
    pub error: Option<Issue>,
    pub recovery: Option<RecoveryStage>,
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
    generation: u64,
    poll_generation: Option<u64>,
    next_due: i64,
    blocked_until: i64,
    last_started: i64,
    failures: u32,
    cadence: i64,
    waiting_for_sign_in: bool,
    cooldown_issue: Option<Issue>,
    recovery_cancel: Option<Arc<AtomicBool>>,
    last_recovery_started: Option<i64>,
}

struct Inner {
    view: AppState,
    schedules: [Schedule; 2],
}

pub struct Runtime {
    inner: Mutex<Inner>,
    client: reqwest::Client,
    wake: Notify,
    quitting: AtomicBool,
}

impl Runtime {
    pub fn new() -> Result<Self, reqwest::Error> {
        let (settings, settings_error) = match settings::load() {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings {
                    launch_at_login_prompt_dismissed: true,
                    ..Settings::default()
                },
                Some(error.to_string()),
            ),
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
                            recovery: None,
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
            quitting: AtomicBool::new(false),
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
        let settings = merge_settings_draft(settings, &inner.view.settings);
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

fn merge_settings_draft(mut draft: Settings, current: &Settings) -> Settings {
    // Immediate actions must survive saving an older settings draft.
    draft.claude_enabled = current.claude_enabled;
    draft.codex_enabled = current.codex_enabled;
    draft.launch_at_login_prompt_dismissed = current.launch_at_login_prompt_dismissed;
    draft
}

pub fn dismiss_launch_at_login_prompt(app: &AppHandle) -> Result<(), String> {
    let runtime = app.state::<Runtime>();
    {
        let mut inner = runtime
            .inner
            .lock()
            .map_err(|_| "Could not save the launch-at-login choice.")?;
        if inner.view.settings.launch_at_login_prompt_dismissed {
            return Ok(());
        }
        let mut settings = inner.view.settings.clone();
        settings.launch_at_login_prompt_dismissed = true;
        settings::save(&settings).map_err(|error| error.to_string())?;
        inner.view.settings = settings;
        inner.view.settings_error = None;
    }
    publish(app);
    Ok(())
}

pub fn request_refresh(app: &AppHandle) {
    let runtime = app.state::<Runtime>();
    let changed = if let Ok(mut inner) = runtime.inner.lock() {
        queue_manual_refresh(&mut inner);
        true
    } else {
        false
    };
    if changed {
        publish(app);
    }
    runtime.wake.notify_one();
}

fn queue_manual_refresh(inner: &mut Inner) {
    for (state, schedule) in inner.view.providers.iter_mut().zip(&mut inner.schedules) {
        if state.recovery.is_some() || !inner.view.settings.is_enabled(state.id) {
            continue;
        }
        schedule.next_due = schedule.last_started.saturating_add(10);
        if schedule.waiting_for_sign_in {
            // A new CLI sign-in can recover immediately; server cooldowns still apply.
            schedule.blocked_until = 0;
            state.next_retry_at = Some(schedule.next_due);
        }
    }
}

pub fn set_provider_enabled(
    app: &AppHandle,
    provider: ProviderId,
    enabled: bool,
) -> Result<(), String> {
    let runtime = app.state::<Runtime>();
    if runtime.quitting.load(Ordering::Relaxed) {
        return Err("Delta-V is quitting.".to_owned());
    }
    {
        let mut inner = runtime
            .inner
            .lock()
            .map_err(|_| "Could not update this connection.")?;
        let index = provider_index(provider);
        if enabled && inner.schedules[index].recovery_cancel.is_some() {
            return Err("Wait for the current sign-in action to finish.".to_owned());
        }
        if inner.view.settings.is_enabled(provider) == enabled {
            return Ok(());
        }
        let mut settings = inner.view.settings.clone();
        settings.set_enabled(provider, enabled);
        settings::save(&settings).map_err(|error| error.to_string())?;
        change_connection(&mut inner, index, enabled, providers::now());
        inner.view.settings_error = None;
    }
    publish(app);
    runtime.wake.notify_one();
    Ok(())
}

fn change_connection(inner: &mut Inner, index: usize, enabled: bool, now: i64) {
    let state = &mut inner.view.providers[index];
    let schedule = &mut inner.schedules[index];
    inner.view.settings.set_enabled(state.id, enabled);
    schedule.generation = schedule.generation.wrapping_add(1);
    state.snapshot = None;
    state.error = None;
    state.stale = true;
    state.next_retry_at = None;
    state.refreshing = enabled && schedule.poll_generation.is_some();
    if let Some(cancel) = &schedule.recovery_cancel {
        cancel.store(true, Ordering::Relaxed);
        state.recovery = Some(RecoveryStage::Cancelling);
    }
    if enabled {
        schedule.next_due = schedule.last_started.saturating_add(10);
        if schedule.waiting_for_sign_in {
            schedule.blocked_until = 0;
        }
        if schedule.blocked_until > now {
            state.next_retry_at = Some(schedule.blocked_until);
            state.error = schedule.cooldown_issue.clone();
        }
    }
}

fn provider_index(provider: ProviderId) -> usize {
    match provider {
        ProviderId::Claude => 0,
        ProviderId::Codex => 1,
    }
}

fn begin_recovery(
    inner: &mut Inner,
    index: usize,
    action: platform::auth::Action,
    cancel: Arc<AtomicBool>,
    now: i64,
) -> Result<(), String> {
    let state = &mut inner.view.providers[index];
    let schedule = &mut inner.schedules[index];
    if inner.view.paused {
        return Err("Unlock your Mac before reconnecting.".to_owned());
    }
    if !inner.view.settings.is_enabled(state.id) {
        return Err("Connect this provider in Settings before signing in.".to_owned());
    }
    if schedule.poll_generation.is_some() || state.refreshing || state.recovery.is_some() {
        return Err("A check or sign-in is already in progress.".to_owned());
    }
    let eligible = state
        .error
        .as_ref()
        .map_or(action == platform::auth::Action::SignIn, |error| {
            matches!(
                error.kind,
                IssueKind::SignIn
                    | IssueKind::Authentication
                    | IssueKind::Recovery
                    | IssueKind::ClientMissing
            )
        });
    if !eligible {
        return Err(
            "This problem does not require a new sign-in. Check the usage message first."
                .to_owned(),
        );
    }
    if !schedule.waiting_for_sign_in && now < schedule.blocked_until {
        return Err(
            "The provider asked us to wait. Reconnect after the countdown ends.".to_owned(),
        );
    }
    if let Some(previous) = schedule.last_recovery_started
        && now < previous.saturating_add(30)
    {
        return Err("Wait a few seconds before trying to reconnect again.".to_owned());
    }
    schedule.last_recovery_started = Some(now);
    schedule.recovery_cancel = Some(cancel);
    state.recovery = Some(match action {
        platform::auth::Action::Renew => RecoveryStage::Checking,
        platform::auth::Action::SignIn => RecoveryStage::SigningIn,
    });
    // Sign-in can select another account, so the previous account's reading must disappear.
    state.snapshot = None;
    state.error = None;
    state.next_retry_at = None;
    state.stale = true;
    Ok(())
}

pub fn request_reconnect(
    app: &AppHandle,
    provider: ProviderId,
    action: platform::auth::Action,
) -> Result<(), String> {
    let runtime = app.state::<Runtime>();
    if runtime.quitting.load(Ordering::Relaxed) {
        return Err("Delta-V is quitting.".to_owned());
    }
    let index = provider_index(provider);
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut inner = runtime
            .inner
            .lock()
            .map_err(|_| "Could not start account recovery.")?;
        begin_recovery(&mut inner, index, action, cancel.clone(), providers::now())?;
    };
    publish(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        recover(&app, provider, action, cancel).await;
    });
    Ok(())
}

pub fn cancel_reconnect(app: &AppHandle, provider: ProviderId) -> Result<(), String> {
    let runtime = app.state::<Runtime>();
    let index = provider_index(provider);
    {
        let mut inner = runtime
            .inner
            .lock()
            .map_err(|_| "Could not cancel account recovery.")?;
        if let Some(cancel) = &inner.schedules[index].recovery_cancel {
            cancel.store(true, Ordering::Relaxed);
            inner.view.providers[index].recovery = Some(RecoveryStage::Cancelling);
        }
    };
    publish(app);
    Ok(())
}

fn recovery_stage(app: &AppHandle, index: usize, stage: RecoveryStage, cancel: &AtomicBool) {
    let runtime = app.state::<Runtime>();
    if let Ok(mut inner) = runtime.inner.lock() {
        inner.view.providers[index].recovery = Some(if cancel.load(Ordering::Relaxed) {
            RecoveryStage::Cancelling
        } else {
            stage
        });
        drop(inner);
        publish(app);
    }
}

async fn recovery_fetch(
    app: &AppHandle,
    index: usize,
    provider: ProviderId,
    cancel: &AtomicBool,
) -> Option<Result<ProviderSnapshot, ProviderError>> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let runtime = app.state::<Runtime>();
        let ready = {
            let mut inner = runtime.inner.lock().ok()?;
            if inner.view.paused || !inner.view.settings.is_enabled(provider) {
                cancel.store(true, Ordering::Relaxed);
                return None;
            }
            let schedule = &mut inner.schedules[index];
            let permitted =
                schedule
                    .last_started
                    .saturating_add(10)
                    .max(if schedule.waiting_for_sign_in {
                        0
                    } else {
                        schedule.blocked_until
                    });
            if providers::now() >= permitted {
                schedule.last_started = providers::now();
                true
            } else {
                false
            }
        };
        if ready {
            return Some(fetch_provider(provider, &runtime.client).await);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn recover(
    app: &AppHandle,
    provider: ProviderId,
    action: platform::auth::Action,
    cancel: Arc<AtomicBool>,
) {
    let index = provider_index(provider);
    if matches!(action, platform::auth::Action::Renew) {
        // Another client may have renewed the sign-in since the failed poll.
        match recovery_fetch(app, index, provider, &cancel).await {
            Some(Err(ProviderError::Authentication)) => {}
            Some(result) => {
                finish_recovery(app, index, &cancel, Some(result), None);
                return;
            }
            None => {
                finish_recovery(app, index, &cancel, None, None);
                return;
            }
        }
    }
    let before = match credentials::load(provider).await {
        Ok(credential) => Some(credential),
        Err(error) if action == platform::auth::Action::SignIn && error.requires_sign_in() => None,
        Err(error) => {
            finish_recovery(
                app,
                index,
                &cancel,
                Some(Err(ProviderError::Credential(error))),
                None,
            );
            return;
        }
    };
    if cancel.load(Ordering::Relaxed) {
        finish_recovery(app, index, &cancel, None, None);
        return;
    }
    recovery_stage(
        app,
        index,
        match action {
            platform::auth::Action::Renew => RecoveryStage::Renewing,
            platform::auth::Action::SignIn => RecoveryStage::SigningIn,
        },
        &cancel,
    );
    let helper_cancel = cancel.clone();
    let result = tokio::task::spawn_blocking(move || {
        platform::auth::recover(provider, action, helper_cancel)
    })
    .await;
    let result = match result {
        Ok(result) => result,
        Err(_) => Err(platform::auth::Error::Failed),
    };
    if let Err(error) = result {
        let kind = match error {
            platform::auth::Error::ClientMissing(_) => IssueKind::ClientMissing,
            platform::auth::Error::UnsupportedAccount => IssueKind::SignIn,
            platform::auth::Error::ManualSignInRequired => IssueKind::Configuration,
            _ => IssueKind::Recovery,
        };
        finish_recovery(
            app,
            index,
            &cancel,
            None,
            Some(Issue {
                kind,
                message: error.to_string(),
            }),
        );
        return;
    }
    if cancel.load(Ordering::Relaxed) {
        finish_recovery(app, index, &cancel, None, None);
        return;
    }
    let after = match credentials::load(provider).await {
        Ok(credential) => credential,
        Err(error) => {
            finish_recovery(
                app,
                index,
                &cancel,
                Some(Err(ProviderError::Credential(error))),
                None,
            );
            return;
        }
    };
    if matches!(action, platform::auth::Action::Renew) && before.as_ref() == Some(&after) {
        finish_recovery(app, index, &cancel, None, Some(Issue {
            kind: IssueKind::Recovery,
            message: "The provider did not update the saved sign-in. Choose Sign in again to reconnect.".to_owned(),
        }));
        return;
    }
    recovery_stage(app, index, RecoveryStage::Checking, &cancel);
    let result = recovery_fetch(app, index, provider, &cancel).await;
    finish_recovery(app, index, &cancel, result, None);
}

fn finish_recovery(
    app: &AppHandle,
    index: usize,
    cancel: &AtomicBool,
    result: Option<Result<ProviderSnapshot, ProviderError>>,
    issue: Option<Issue>,
) {
    let runtime = app.state::<Runtime>();
    {
        let Ok(mut inner) = runtime.inner.lock() else {
            return;
        };
        complete_recovery(
            &mut inner,
            index,
            cancel.load(Ordering::Relaxed),
            result,
            issue,
            providers::now(),
        );
    };
    publish(app);
    runtime.wake.notify_one();
}

fn complete_recovery(
    inner: &mut Inner,
    index: usize,
    cancelled: bool,
    result: Option<Result<ProviderSnapshot, ProviderError>>,
    issue: Option<Issue>,
    now: i64,
) {
    inner.schedules[index].recovery_cancel = None;
    inner.view.providers[index].recovery = None;
    if !inner
        .view
        .settings
        .is_enabled(inner.view.providers[index].id)
    {
        if let Some(Err(error)) = &result {
            preserve_service_wait(&mut inner.schedules[index], error, now);
        }
        return;
    }
    if let Some(result) = result
        && (!cancelled || result.is_err())
    {
        let cadence = inner.schedules[index]
            .cadence
            .max(inner.view.settings.refresh_seconds as i64);
        apply_result(inner, index, cadence, now, result);
        return;
    }
    let schedule = &mut inner.schedules[index];
    // This wait is local. It must not replace an outstanding service cooldown.
    schedule.next_due = schedule.blocked_until.max(now.saturating_add(300));
    schedule.waiting_for_sign_in = schedule.waiting_for_sign_in || schedule.blocked_until <= now;
    let state = &mut inner.view.providers[index];
    state.error = Some(if cancelled {
        Issue { kind: IssueKind::Recovery, message: "Reconnection cancelled. Any sign-in already completed stays saved in the provider's app.".to_owned() }
    } else {
        issue.unwrap_or(Issue {
            kind: IssueKind::Recovery,
            message:
                "Could not finish reconnecting. Try again or sign in through the provider's app."
                    .to_owned(),
        })
    });
    state.snapshot = None;
    state.stale = true;
    state.next_retry_at = Some(schedule.next_due);
}

pub fn request_quit(app: &AppHandle) {
    let runtime = app.state::<Runtime>();
    if runtime.quitting.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Ok(mut inner) = runtime.inner.lock() {
        for index in 0..PROVIDERS.len() {
            if let Some(cancel) = &inner.schedules[index].recovery_cancel {
                cancel.store(true, Ordering::Relaxed);
                inner.view.providers[index].recovery = Some(RecoveryStage::Cancelling);
            }
        }
        drop(inner);
        publish(app);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let active = app
                .state::<Runtime>()
                .inner
                .lock()
                .map(|inner| {
                    inner
                        .schedules
                        .iter()
                        .any(|schedule| schedule.recovery_cancel.is_some())
                })
                .unwrap_or(false);
            if !active {
                app.exit(0);
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
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
    if runtime.quitting.load(Ordering::Relaxed) {
        return;
    }
    let mut pending = Vec::new();
    let changed = {
        let Ok(mut inner) = runtime.inner.lock() else {
            return;
        };
        let before = inner.view.clone();
        let was_paused = inner.view.paused;
        inner.view.paused = activity.locked != Some(false);
        let cadence = interval(&inner.view.settings, activity.idle_seconds);
        for (index, provider) in PROVIDERS.into_iter().enumerate() {
            if (inner.view.paused || !inner.view.settings.is_enabled(provider))
                && let Some(cancel) = &inner.schedules[index].recovery_cancel
            {
                cancel.store(true, Ordering::Relaxed);
                inner.view.providers[index].recovery = Some(RecoveryStage::Cancelling);
            }
            if !inner.view.settings.providers.includes(provider)
                || !inner.view.settings.is_enabled(provider)
            {
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
            if inner.view.paused
                || inner.schedules[index].poll_generation.is_some()
                || inner.view.providers[index].refreshing
                || inner.view.providers[index].recovery.is_some()
            {
                continue;
            }
            let schedule = &mut inner.schedules[index];
            if now < schedule.next_due.max(schedule.blocked_until) {
                continue;
            }
            schedule.last_started = now;
            let generation = schedule.generation;
            schedule.poll_generation = Some(generation);
            inner.view.providers[index].refreshing = true;
            pending.push((index, provider, generation));
        }
        before != inner.view
    };
    if changed {
        publish(app);
    }
    for (index, provider, generation) in pending {
        let app = app.clone();
        let client = runtime.client.clone();
        tauri::async_runtime::spawn(async move {
            let current = {
                let runtime = app.state::<Runtime>();
                let Ok(mut inner) = runtime.inner.lock() else {
                    return;
                };
                if inner.schedules[index].generation == generation
                    && inner.view.settings.is_enabled(provider)
                {
                    true
                } else {
                    if inner.schedules[index].poll_generation == Some(generation) {
                        inner.schedules[index].poll_generation = None;
                        inner.view.providers[index].refreshing = false;
                    }
                    false
                }
            };
            if !current {
                publish(&app);
                return;
            }
            let result = fetch_provider(provider, &client).await;
            finish(&app, index, generation, result);
        });
    }
}

async fn fetch_provider(
    provider: ProviderId,
    client: &reqwest::Client,
) -> Result<ProviderSnapshot, ProviderError> {
    match provider {
        ProviderId::Claude => ClaudeProvider { client }.fetch().await,
        ProviderId::Codex => CodexProvider { client }.fetch().await,
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

fn finish(
    app: &AppHandle,
    index: usize,
    generation: u64,
    result: Result<ProviderSnapshot, ProviderError>,
) {
    let runtime = app.state::<Runtime>();
    {
        let Ok(mut inner) = runtime.inner.lock() else {
            return;
        };
        let now = providers::now();
        finish_poll(&mut inner, index, generation, now, result);
    };
    publish(app);
}

fn finish_poll(
    inner: &mut Inner,
    index: usize,
    generation: u64,
    now: i64,
    result: Result<ProviderSnapshot, ProviderError>,
) {
    let schedule = &mut inner.schedules[index];
    if schedule.poll_generation != Some(generation) {
        return;
    }
    schedule.poll_generation = None;
    inner.view.providers[index].refreshing = false;
    if generation != schedule.generation
        || !inner
            .view
            .settings
            .is_enabled(inner.view.providers[index].id)
    {
        if let Err(error) = &result {
            preserve_service_wait(schedule, error, now);
        }
        if inner
            .view
            .settings
            .is_enabled(inner.view.providers[index].id)
            && schedule.blocked_until > now
        {
            inner.view.providers[index].next_retry_at = Some(schedule.blocked_until);
            inner.view.providers[index].error = schedule.cooldown_issue.clone();
        }
        return;
    }
    let cadence = schedule.cadence;
    apply_result(inner, index, cadence, now, result);
}

fn preserve_service_wait(schedule: &mut Schedule, error: &ProviderError, now: i64) {
    if matches!(
        error,
        ProviderError::RateLimited { .. }
            | ProviderError::AccessDenied { .. }
            | ProviderError::Service { .. }
    ) {
        schedule.failures = schedule.failures.saturating_add(1);
        schedule.blocked_until = schedule
            .blocked_until
            .max(error.retry_at().unwrap_or(0))
            .max(now.saturating_add(backoff(schedule.failures, now)));
        schedule.next_due = schedule.next_due.max(schedule.blocked_until);
        schedule.waiting_for_sign_in = false;
        schedule.cooldown_issue = Some(error.issue());
    }
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
            schedule.cooldown_issue = None;
            state.error = None;
            state.next_retry_at = None;
            state.snapshot = Some(snapshot);
            state.stale = is_stale(state, now, cadence);
        }
        Err(error) => {
            schedule.failures = schedule.failures.saturating_add(1);
            schedule.waiting_for_sign_in = error.requires_sign_in();
            schedule.cooldown_issue = (!schedule.waiting_for_sign_in).then(|| error.issue());
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
            state.error = Some(error.issue());
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
        .filter(|state| {
            view.settings.providers.includes(state.id) && view.settings.is_enabled(state.id)
        })
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

pub fn publish(app: &AppHandle) {
    let handle = app.clone();
    // Read state at delivery time so a delayed poll cannot restore an older sign-in screen.
    if let Err(error) = app.run_on_main_thread(move || {
        if let Ok(view) = handle.state::<Runtime>().state() {
            publish_view(&handle, &view);
        }
    }) {
        eprintln!("Could not update the usage panel: {error}");
    }
}

fn publish_view(app: &AppHandle, view: &AppState) {
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

    #[test]
    fn saving_an_open_draft_preserves_immediate_choices() {
        let draft = Settings {
            threshold: 15,
            refresh_seconds: 120,
            ..Settings::default()
        };
        let current = Settings {
            claude_enabled: false,
            codex_enabled: false,
            launch_at_login_prompt_dismissed: true,
            ..Settings::default()
        };

        let saved = merge_settings_draft(draft, &current);
        assert!(!saved.claude_enabled);
        assert!(!saved.codex_enabled);
        assert!(saved.launch_at_login_prompt_dismissed);
        assert_eq!(saved.threshold, 15);
        assert_eq!(saved.refresh_seconds, 120);

        let stale = Settings {
            claude_enabled: false,
            codex_enabled: false,
            launch_at_login_prompt_dismissed: true,
            ..Settings::default()
        };
        let saved = merge_settings_draft(stale, &Settings::default());
        assert!(saved.claude_enabled);
        assert!(saved.codex_enabled);
        assert!(!saved.launch_at_login_prompt_dismissed);
    }

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
                    recovery: None,
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
    fn recovery_clears_account_data_and_blocks_duplicate_actions_until_cleanup() {
        let mut inner = inner();
        inner.view.providers[0].error = Some(ProviderError::Authentication.issue());
        inner.schedules[0].waiting_for_sign_in = true;
        let cancel = Arc::new(AtomicBool::new(false));
        begin_recovery(
            &mut inner,
            0,
            platform::auth::Action::Renew,
            cancel.clone(),
            150,
        )
        .unwrap();
        assert!(inner.view.providers[0].snapshot.is_none());
        assert_eq!(
            inner.view.providers[0].recovery,
            Some(RecoveryStage::Checking)
        );
        let due = inner.schedules[0].next_due;
        queue_manual_refresh(&mut inner);
        assert_eq!(inner.schedules[0].next_due, due);

        cancel.store(true, Ordering::Relaxed);
        inner.view.providers[0].recovery = Some(RecoveryStage::Cancelling);
        assert!(
            begin_recovery(&mut inner, 0, platform::auth::Action::SignIn, cancel, 181).is_err()
        );
        complete_recovery(&mut inner, 0, true, None, None, 182);
        assert!(inner.schedules[0].recovery_cancel.is_none());
        assert!(inner.view.providers[0].recovery.is_none());
        assert!(inner.view.providers[0].snapshot.is_none());
    }

    #[test]
    fn recovery_requires_an_auth_issue_and_respects_service_waits() {
        for error in [
            ProviderError::Network,
            ProviderError::AccessDenied {
                retry_at: Some(5000),
            },
            ProviderError::RateLimited {
                retry_at: Some(5000),
            },
            ProviderError::Credential(credentials::CredentialError::Unreadable),
        ] {
            let mut inner = inner();
            apply_result(&mut inner, 0, 60, 150, Err(error));
            let cancel = Arc::new(AtomicBool::new(false));
            assert!(
                begin_recovery(&mut inner, 0, platform::auth::Action::SignIn, cancel, 151).is_err()
            );
            assert!(inner.schedules[0].recovery_cancel.is_none());
        }
        let mut inner = inner();
        inner.view.providers[0].error = Some(Issue {
            kind: IssueKind::Recovery,
            message: "Try again".into(),
        });
        inner.schedules[0].blocked_until = 5000;
        assert!(
            begin_recovery(
                &mut inner,
                0,
                platform::auth::Action::Renew,
                Arc::new(AtomicBool::new(false)),
                151
            )
            .is_err()
        );
    }

    #[test]
    fn cancelled_recovery_keeps_retry_after_received_during_a_check() {
        let mut inner = inner();
        apply_result(&mut inner, 0, 60, 100, Err(ProviderError::Authentication));
        begin_recovery(
            &mut inner,
            0,
            platform::auth::Action::Renew,
            Arc::new(AtomicBool::new(false)),
            150,
        )
        .unwrap();
        complete_recovery(
            &mut inner,
            0,
            true,
            Some(Err(ProviderError::RateLimited {
                retry_at: Some(5000),
            })),
            None,
            151,
        );
        queue_manual_refresh(&mut inner);
        assert_eq!(inner.schedules[0].blocked_until, 5000);
        assert!(!inner.schedules[0].waiting_for_sign_in);
        assert_eq!(
            inner.view.providers[0].error.as_ref().unwrap().kind,
            IssueKind::RateLimited
        );
        assert!(inner.view.providers[0].snapshot.is_none());
    }

    #[test]
    fn cancelled_recovery_does_not_publish_a_completed_account_reading() {
        let mut inner = inner();
        let snapshot = inner.view.providers[0].snapshot.clone().unwrap();
        apply_result(&mut inner, 0, 60, 100, Err(ProviderError::Authentication));
        begin_recovery(
            &mut inner,
            0,
            platform::auth::Action::SignIn,
            Arc::new(AtomicBool::new(false)),
            150,
        )
        .unwrap();
        complete_recovery(&mut inner, 0, true, Some(Ok(snapshot)), None, 151);
        assert!(inner.view.providers[0].snapshot.is_none());
        assert_eq!(
            inner.view.providers[0].error.as_ref().unwrap().kind,
            IssueKind::Recovery
        );
        assert!(
            begin_recovery(
                &mut inner,
                0,
                platform::auth::Action::SignIn,
                Arc::new(AtomicBool::new(false)),
                152
            )
            .is_err()
        );
        assert!(
            begin_recovery(
                &mut inner,
                0,
                platform::auth::Action::SignIn,
                Arc::new(AtomicBool::new(false)),
                180
            )
            .is_ok()
        );
    }

    #[test]
    fn recovery_waits_for_unlock_and_an_enabled_provider() {
        let mut inner = inner();
        apply_result(&mut inner, 0, 60, 100, Err(ProviderError::Authentication));
        inner.view.paused = true;
        assert!(
            begin_recovery(
                &mut inner,
                0,
                platform::auth::Action::Renew,
                Arc::new(AtomicBool::new(false)),
                150
            )
            .is_err()
        );
        inner.view.paused = false;
        inner.view.settings.set_enabled(ProviderId::Claude, false);
        assert!(
            begin_recovery(
                &mut inner,
                0,
                platform::auth::Action::Renew,
                Arc::new(AtomicBool::new(false)),
                150
            )
            .is_err()
        );
    }

    #[test]
    fn disconnect_clears_readings_and_discards_an_inflight_poll() {
        let mut inner = inner();
        let snapshot = inner.view.providers[0].snapshot.clone().unwrap();
        inner.schedules[0].poll_generation = Some(0);
        inner.view.providers[0].refreshing = true;
        change_connection(&mut inner, 0, false, 150);
        assert!(!inner.view.settings.is_enabled(ProviderId::Claude));
        assert!(inner.view.providers[0].snapshot.is_none());
        assert!(!inner.view.providers[0].refreshing);
        assert!(tray_reading(&inner.view, 150).is_none());
        let due = inner.schedules[0].next_due;
        queue_manual_refresh(&mut inner);
        assert_eq!(inner.schedules[0].next_due, due);

        finish_poll(&mut inner, 0, 0, 151, Ok(snapshot));
        assert!(inner.view.providers[0].snapshot.is_none());
        assert!(inner.view.providers[0].error.is_none());
        assert!(inner.schedules[0].poll_generation.is_none());
        assert!(
            begin_recovery(
                &mut inner,
                0,
                platform::auth::Action::SignIn,
                Arc::new(AtomicBool::new(false)),
                160
            )
            .is_err()
        );
    }

    #[test]
    fn connecting_again_waits_for_a_new_reading() {
        let mut inner = inner();
        let mut snapshot = inner.view.providers[0].snapshot.clone().unwrap();
        inner.schedules[0].poll_generation = Some(0);
        inner.schedules[0].last_started = 100;
        change_connection(&mut inner, 0, false, 101);
        change_connection(&mut inner, 0, true, 102);
        assert_eq!(inner.schedules[0].next_due, 110);
        assert!(inner.view.providers[0].refreshing);
        finish_poll(&mut inner, 0, 0, 103, Ok(snapshot.clone()));
        assert!(inner.view.providers[0].snapshot.is_none());
        assert!(!inner.view.providers[0].refreshing);

        let generation = inner.schedules[0].generation;
        inner.schedules[0].poll_generation = Some(generation);
        inner.view.providers[0].refreshing = true;
        finish_poll(&mut inner, 0, 0, 111, Ok(snapshot.clone()));
        assert!(inner.view.providers[0].refreshing);
        snapshot.fetched_at = 112;
        finish_poll(&mut inner, 0, generation, 112, Ok(snapshot));
        assert_eq!(
            inner.view.providers[0]
                .snapshot
                .as_ref()
                .unwrap()
                .fetched_at,
            112
        );
    }

    #[test]
    fn disconnect_and_connect_cannot_clear_a_service_cooldown() {
        let mut inner = inner();
        inner.schedules[0].poll_generation = Some(0);
        change_connection(&mut inner, 0, false, 150);
        finish_poll(
            &mut inner,
            0,
            0,
            151,
            Err(ProviderError::RateLimited {
                retry_at: Some(5000),
            }),
        );
        assert!(inner.view.providers[0].error.is_none());
        assert!(inner.view.providers[0].next_retry_at.is_none());
        change_connection(&mut inner, 0, true, 152);
        queue_manual_refresh(&mut inner);
        assert_eq!(inner.schedules[0].blocked_until, 5000);
        assert_eq!(inner.view.providers[0].next_retry_at, Some(5000));
        assert_eq!(
            inner.view.providers[0].error.as_ref().unwrap().kind,
            IssueKind::RateLimited
        );
        assert!(
            begin_recovery(
                &mut inner,
                0,
                platform::auth::Action::SignIn,
                Arc::new(AtomicBool::new(false)),
                160
            )
            .is_err()
        );
    }

    #[test]
    fn disconnect_cancels_auth_without_republishing_its_result() {
        let mut inner = inner();
        let snapshot = inner.view.providers[0].snapshot.clone().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        begin_recovery(
            &mut inner,
            0,
            platform::auth::Action::SignIn,
            cancel.clone(),
            150,
        )
        .unwrap();
        change_connection(&mut inner, 0, false, 151);
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(
            inner.view.providers[0].recovery,
            Some(RecoveryStage::Cancelling)
        );
        complete_recovery(&mut inner, 0, true, Some(Ok(snapshot)), None, 152);
        assert!(inner.view.providers[0].snapshot.is_none());
        assert!(inner.view.providers[0].error.is_none());
        assert!(inner.view.providers[0].recovery.is_none());
        assert!(inner.schedules[0].recovery_cancel.is_none());
        assert!(!inner.view.settings.is_enabled(ProviderId::Claude));
    }

    #[test]
    fn explicit_sign_in_can_change_a_healthy_hidden_provider() {
        let mut inner = inner();
        inner.view.settings.providers = settings::ProviderSelection::Codex;
        begin_recovery(
            &mut inner,
            0,
            platform::auth::Action::SignIn,
            Arc::new(AtomicBool::new(false)),
            150,
        )
        .unwrap();
        assert!(inner.view.providers[0].snapshot.is_none());
        assert_eq!(
            inner.view.providers[0].recovery,
            Some(RecoveryStage::SigningIn)
        );
    }

    #[test]
    fn hidden_provider_can_be_reconnected_from_settings() {
        let mut inner = inner();
        inner.view.settings.providers = settings::ProviderSelection::Codex;
        apply_result(&mut inner, 0, 60, 100, Err(ProviderError::Authentication));
        begin_recovery(
            &mut inner,
            0,
            platform::auth::Action::Renew,
            Arc::new(AtomicBool::new(false)),
            150,
        )
        .unwrap();
        assert_eq!(
            inner.view.providers[0].recovery,
            Some(RecoveryStage::Checking)
        );
    }

    #[test]
    fn disconnect_during_auth_verification_retains_a_server_wait() {
        let mut inner = inner();
        begin_recovery(
            &mut inner,
            0,
            platform::auth::Action::SignIn,
            Arc::new(AtomicBool::new(false)),
            150,
        )
        .unwrap();
        change_connection(&mut inner, 0, false, 151);
        complete_recovery(
            &mut inner,
            0,
            true,
            Some(Err(ProviderError::RateLimited {
                retry_at: Some(5000),
            })),
            None,
            152,
        );
        assert!(inner.view.providers[0].error.is_none());
        change_connection(&mut inner, 0, true, 153);
        assert_eq!(inner.view.providers[0].next_retry_at, Some(5000));
        assert_eq!(
            inner.view.providers[0].error.as_ref().unwrap().kind,
            IssueKind::RateLimited
        );
        assert_eq!(inner.schedules[0].blocked_until, 5000);
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
