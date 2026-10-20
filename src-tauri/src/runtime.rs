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
    availability_store::Store as AvailabilityStore,
    availability_watches::{Registry as AvailabilityWatches, WatchStatus},
    cooldown::{self, Store as CooldownStore},
    credentials,
    history::Retention,
    model::{LimitKind, Provenance, ProviderId, ProviderSnapshot},
    notifications::{
        Evaluator, LowAllowance,
        availability::{RecoveredAllowance, Watch},
    },
    platform::{self, Activity},
    providers::{self, ClaudeProvider, CodexProvider, Issue, IssueKind, Provider, ProviderError},
    recording::{HistoryState, Recorder},
    settings::{
        self, DisplayPreferences, PercentageMode, ProviderSelection, Settings, SettingsError,
    },
    tray,
};

const PROVIDERS: [ProviderId; 2] = [ProviderId::Claude, ProviderId::Codex];
const CLAUDE_MIN_INTERVAL: i64 = 300;

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
    /// Orders snapshots, including command replies and events, within this app run.
    pub revision: u64,
    pub settings: Settings,
    pub providers: Vec<ProviderState>,
    pub settings_error: Option<String>,
    pub cooldown_error: Option<String>,
    pub paused: bool,
}

#[derive(Default)]
struct Schedule {
    generation: u64,
    poll_generation: Option<u64>,
    next_due: i64,
    blocked_until: i64,
    last_started: Option<i64>,
    failures: u32,
    cadence: i64,
    waiting_for_sign_in: bool,
    cooldown_issue: Option<Issue>,
    service_wait: Option<cooldown::Wait>,
    recovery_cancel: Option<Arc<AtomicBool>>,
    last_recovery_started: Option<i64>,
}

impl Schedule {
    fn after_last_start(&self, seconds: i64) -> i64 {
        self.last_started
            .map_or(0, |started| started.saturating_add(seconds))
    }

    fn next_poll_at(&self, provider: ProviderId) -> i64 {
        let spacing = if provider == ProviderId::Claude && !self.waiting_for_sign_in {
            self.after_last_start(CLAUDE_MIN_INTERVAL)
        } else {
            0
        };
        self.next_due
            .max(self.blocked_until)
            .max(spacing)
            .max(self.service_deadline())
    }

    fn service_deadline(&self) -> i64 {
        self.service_wait.as_ref().map_or(0, |wait| wait.retry_at)
    }
}

struct Inner {
    view: AppState,
    schedules: [Schedule; 2],
    notifications: Evaluator,
    notification_generation: u64,
    notification_permission: Option<bool>,
    availability: AvailabilityWatches,
    identity_demand: [bool; 2],
}

/// An accepted reading, kept in Rust while notification permission is checked.
#[derive(Clone)]
pub struct NotificationTicket {
    pub snapshot: ProviderSnapshot,
    connection_generation: u64,
    policy_generation: u64,
}

/// A watch response remains independent of the low-allowance notification preference.
pub struct AvailabilityTicket {
    pub snapshot: ProviderSnapshot,
    connection_generation: u64,
    watch_generation: u64,
}

/// Binds an explicit arm action while macOS permission may await user input.
pub struct AvailabilityArmTicket {
    provider: ProviderId,
    account_key: String,
    limit_id: String,
    window_seconds: Option<u64>,
    connection_generation: u64,
}

impl Inner {
    fn snapshot(&mut self) -> AppState {
        self.view.revision = self.view.revision.saturating_add(1);
        self.view.clone()
    }
}

pub struct Runtime {
    inner: Mutex<Inner>,
    history: Recorder,
    cooldown: CooldownStore,
    identity: providers::IdentityCache,
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
        let cooldown = CooldownStore::new(std::env::var_os("HOME").map(|home| {
            std::path::PathBuf::from(home).join(".config/delta-v/claude-cooldown.toml")
        }));
        let mut inner = Inner {
            view: AppState {
                revision: 0,
                settings: settings.clone(),
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
                cooldown_error: None,
                paused: false,
            },
            schedules: Default::default(),
            notifications: Evaluator::default(),
            notification_generation: 0,
            notification_permission: None,
            availability: AvailabilityWatches::load(AvailabilityStore::new(
                std::env::var_os("HOME").map(|home| {
                    std::path::PathBuf::from(home).join(".config/delta-v/availability-watches.json")
                }),
            )),
            identity_demand: [false; 2],
        };
        restore_claude_wait(&mut inner, &cooldown, providers::now());
        Ok(Self {
            history: Recorder::new(
                std::env::var_os("HOME").map(|home| {
                    std::path::PathBuf::from(home).join(".local/share/delta-v/history")
                }),
                settings.history_recording,
                settings.history_retention,
            ),
            identity: providers::IdentityCache::default(),
            cooldown,
            inner: Mutex::new(inner),
            client: providers::client()?,
            wake: Notify::new(),
            quitting: AtomicBool::new(false),
        })
    }

    pub fn state(&self) -> Result<AppState, String> {
        self.inner
            .lock()
            .map(|mut inner| inner.snapshot())
            .map_err(|_| "Could not read the app state.".to_owned())
    }

    pub fn save_display_preferences(
        &self,
        preferences: DisplayPreferences,
    ) -> Result<AppState, String> {
        self.save_change(|current| preferences.apply_to(current))
    }

    pub fn set_provider_selection(&self, providers: ProviderSelection) -> Result<AppState, String> {
        self.save_change(|current| current.with_provider_selection(providers))
    }

    pub fn set_notifications_enabled(&self, enabled: bool) -> Result<AppState, String> {
        let mut inner = self.inner.lock().map_err(|_| "Could not save settings.")?;
        let view =
            save_notifications_enabled(&mut inner, enabled, providers::now(), settings::save)?;
        drop(inner);
        self.wake.notify_one();
        Ok(view)
    }

    /// Record permission reads outside polling so restoring access starts with a fresh baseline.
    pub fn update_notification_permission(&self, can_deliver: bool) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not update notification permission.".to_owned())?;
        update_notification_permission(&mut inner, can_deliver)
    }

    pub fn notification_candidates(
        &self,
        ticket: &NotificationTicket,
        can_deliver: bool,
    ) -> Result<Vec<LowAllowance>, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not evaluate usage notifications.".to_owned())?;
        if self.quitting.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        notification_candidates(&mut inner, ticket, can_deliver, providers::now())
    }

    pub fn notification_ticket_current(&self, ticket: &NotificationTicket) -> bool {
        self.inner.lock().is_ok_and(|inner| {
            !self.quitting.load(Ordering::Relaxed)
                && notification_ticket_current(&inner, ticket, providers::now())
        })
    }

    pub fn availability_state(&self) -> Result<WatchStatus, String> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| "Could not read availability watches.")?;
        self.require_running()?;
        Ok(inner.availability.status())
    }

    pub fn prepare_availability_watch(&self, provider: ProviderId) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not prepare an availability watch.")?;
        self.require_running()?;
        prepare_availability_watch(&mut inner, provider)?;
        drop(inner);
        self.wake.notify_one();
        Ok(())
    }

    pub fn clear_availability_preparation(&self, provider: ProviderId) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not cancel availability watch preparation.")?;
        self.require_running()?;
        inner.identity_demand[provider_index(provider)] = false;
        drop(inner);
        self.wake.notify_one();
        Ok(())
    }

    pub fn availability_arm_ticket(
        &self,
        provider: ProviderId,
        limit_id: &str,
    ) -> Result<AvailabilityArmTicket, String> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| "Could not prepare an availability watch.")?;
        self.require_running()?;
        availability_arm_ticket(&inner, provider, limit_id, providers::now())
    }

    pub fn arm_availability_watch(&self, ticket: &AvailabilityArmTicket) -> Result<String, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not start an availability watch.")?;
        self.require_running()?;
        let id = arm_availability_from_ticket(&mut inner, ticket, providers::now())?;
        drop(inner);
        self.wake.notify_one();
        Ok(id)
    }

    pub fn cancel_availability_watch(&self, id: &str) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not cancel an availability watch.")?;
        self.require_running()?;
        inner.availability.cancel(id)?;
        drop(inner);
        self.wake.notify_one();
        Ok(())
    }

    pub fn availability_ticket_current(&self, ticket: &AvailabilityTicket) -> bool {
        self.inner.lock().is_ok_and(|inner| {
            !self.quitting.load(Ordering::Relaxed)
                && availability_ticket_current(&inner, ticket, providers::now())
        })
    }

    pub fn availability_candidates(
        &self,
        ticket: &AvailabilityTicket,
        can_deliver: bool,
    ) -> Result<Vec<RecoveredAllowance>, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "Could not evaluate availability watches.")?;
        if self.quitting.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        availability_candidates(&mut inner, ticket, can_deliver, providers::now())
    }

    fn require_running(&self) -> Result<(), String> {
        if self.quitting.load(Ordering::Relaxed) {
            Err("Delta-V is quitting.".into())
        } else {
            Ok(())
        }
    }

    fn save_change(
        &self,
        change: impl FnOnce(&Settings) -> Result<Settings, SettingsError>,
    ) -> Result<AppState, String> {
        let mut inner = self.inner.lock().map_err(|_| "Could not save settings.")?;
        let view = save_settings_change(&mut inner, change, providers::now(), settings::save)?;
        drop(inner);
        self.wake.notify_one();
        Ok(view)
    }
}

fn save_settings_change(
    inner: &mut Inner,
    change: impl FnOnce(&Settings) -> Result<Settings, SettingsError>,
    now: i64,
    persist: impl FnOnce(&Settings) -> Result<(), SettingsError>,
) -> Result<AppState, String> {
    let settings = change(&inner.view.settings).map_err(|error| error.to_string())?;
    persist(&settings).map_err(|error| error.to_string())?;
    if settings.notifications_enabled != inner.view.settings.notifications_enabled
        || settings.threshold != inner.view.settings.threshold
    {
        reset_notification_baselines(inner);
    }
    for (index, provider) in PROVIDERS.into_iter().enumerate() {
        if inner.view.settings.providers.includes(provider) != settings.providers.includes(provider)
            || (!should_poll_for(inner, &inner.view.settings, provider)
                && should_poll_for(inner, &settings, provider))
        {
            inner.schedules[index].next_due = 0;
        } else if settings.refresh_seconds != inner.view.settings.refresh_seconds {
            inner.schedules[index].next_due = inner.schedules[index]
                .next_due
                .min(now.saturating_add(settings.refresh_seconds as i64));
        }
    }
    inner.view.settings = settings;
    inner.view.settings_error = None;
    Ok(inner.snapshot())
}

fn reset_notification_baselines(inner: &mut Inner) {
    inner.notification_generation = inner.notification_generation.wrapping_add(1);
    for provider in PROVIDERS {
        inner.notifications.reset_baseline(provider);
    }
}

fn save_notifications_enabled(
    inner: &mut Inner,
    enabled: bool,
    now: i64,
    persist: impl FnOnce(&Settings) -> Result<(), SettingsError>,
) -> Result<AppState, String> {
    let already_enabled = inner.view.settings.notifications_enabled;
    let view = save_settings_change(
        inner,
        |current| {
            let mut settings = current.clone();
            settings.notifications_enabled = enabled;
            Ok(settings)
        },
        now,
        persist,
    )?;
    // An explicit restore may follow a permission change that polling never observed.
    if enabled && already_enabled {
        reset_notification_baselines(inner);
    }
    Ok(view)
}

fn update_notification_permission(inner: &mut Inner, can_deliver: bool) -> Result<(), String> {
    if inner
        .notification_permission
        .is_some_and(|previous| previous != can_deliver)
    {
        reset_notification_baselines(inner);
    }
    inner.notification_permission = Some(can_deliver);
    inner
        .notifications
        .set_policy(
            inner.view.settings.notifications_enabled && can_deliver,
            inner.view.settings.threshold,
        )
        .map_err(|error| error.to_string())
}

fn notification_ticket(
    inner: &Inner,
    accepted: Option<&ProviderSnapshot>,
) -> Option<NotificationTicket> {
    let snapshot = accepted?;
    if !inner.view.settings.notifications_enabled {
        return None;
    }
    Some(NotificationTicket {
        snapshot: snapshot.clone(),
        connection_generation: inner.schedules[provider_index(snapshot.provider)].generation,
        policy_generation: inner.notification_generation,
    })
}

fn notification_max_age(inner: &Inner, provider: ProviderId) -> u64 {
    inner.schedules[provider_index(provider)]
        .cadence
        .max(base_interval(&inner.view.settings, provider))
        .saturating_add(30) as u64
}

fn notification_ticket_current(inner: &Inner, ticket: &NotificationTicket, now: i64) -> bool {
    inner.view.settings.notifications_enabled
        && inner.notification_generation == ticket.policy_generation
        && accepted_snapshot_current(inner, &ticket.snapshot, ticket.connection_generation, now)
}

fn accepted_snapshot_current(
    inner: &Inner,
    snapshot: &ProviderSnapshot,
    connection_generation: u64,
    now: i64,
) -> bool {
    let provider = snapshot.provider;
    let index = provider_index(provider);
    inner.view.settings.is_enabled(provider)
        && !inner.view.paused
        && inner.schedules[index].generation == connection_generation
        && snapshot.fetched_at <= now
        && now.abs_diff(snapshot.fetched_at) <= notification_max_age(inner, provider)
        && inner.view.providers.get(index).is_some_and(|state| {
            state.id == provider
                && state.error.is_none()
                && state.recovery.is_none()
                && state.snapshot.as_ref() == Some(snapshot)
        })
}

fn prepare_availability_watch(inner: &mut Inner, provider: ProviderId) -> Result<(), String> {
    let index = provider_index(provider);
    if !inner.view.settings.is_enabled(provider) {
        return Err("Connect this provider before preparing an availability watch.".into());
    }
    if inner
        .view
        .providers
        .get(index)
        .is_none_or(|state| state.id != provider || state.recovery.is_some())
    {
        return Err("Wait for the current sign-in action to finish.".into());
    }
    // Identity preparation joins ordinary monitoring without advancing its next check.
    inner.identity_demand[index] = true;
    Ok(())
}

fn availability_arm_snapshot(
    inner: &Inner,
    provider: ProviderId,
) -> Result<&ProviderSnapshot, String> {
    let index = provider_index(provider);
    if !inner.view.settings.is_enabled(provider) {
        return Err("Connect this provider before starting an availability watch.".into());
    }
    if inner.view.paused {
        return Err("Unlock your Mac before starting an availability watch.".into());
    }
    inner
        .view
        .providers
        .get(index)
        .filter(|state| state.id == provider && state.error.is_none() && state.recovery.is_none())
        .and_then(|state| state.snapshot.as_ref())
        .filter(|snapshot| snapshot.provider == provider)
        .ok_or_else(|| "A fresh usage reading is needed to start this watch.".into())
}

fn availability_arm_ticket(
    inner: &Inner,
    provider: ProviderId,
    limit_id: &str,
    now: i64,
) -> Result<AvailabilityArmTicket, String> {
    let snapshot = availability_arm_snapshot(inner, provider)?;
    Watch::arm(
        snapshot,
        snapshot.history_account.as_deref(),
        limit_id,
        now,
        notification_max_age(inner, provider),
    )
    .map_err(|error| error.to_string())?;
    let account_key = snapshot
        .history_account
        .clone()
        .ok_or("A verified account is needed to start this watch.")?;
    let limit = snapshot
        .limits
        .iter()
        .find(|limit| limit.id == limit_id)
        .ok_or("This quota does not have a usable reading.")?;
    Ok(AvailabilityArmTicket {
        provider,
        account_key,
        limit_id: limit.id.clone(),
        window_seconds: limit.window_seconds,
        connection_generation: inner.schedules[provider_index(provider)].generation,
    })
}

fn arm_availability_from_ticket(
    inner: &mut Inner,
    ticket: &AvailabilityArmTicket,
    now: i64,
) -> Result<String, String> {
    if inner.schedules[provider_index(ticket.provider)].generation != ticket.connection_generation {
        return Err("The connection changed. Start this availability watch again.".into());
    }
    let snapshot = availability_arm_snapshot(inner, ticket.provider)?;
    if snapshot.history_account.as_deref() != Some(ticket.account_key.as_str())
        || !snapshot.limits.iter().any(|limit| {
            limit.id == ticket.limit_id && limit.window_seconds == ticket.window_seconds
        })
    {
        return Err("The account or quota changed. Start this availability watch again.".into());
    }
    arm_availability_watch(inner, ticket.provider, &ticket.limit_id, now)
}

fn arm_availability_watch(
    inner: &mut Inner,
    provider: ProviderId,
    limit_id: &str,
    now: i64,
) -> Result<String, String> {
    let snapshot = availability_arm_snapshot(inner, provider)?.clone();
    let max_age = notification_max_age(inner, provider);
    let id = inner.availability.arm(&snapshot, limit_id, now, max_age)?;
    inner.identity_demand[provider_index(provider)] = false;
    Ok(id)
}

fn availability_ticket(
    inner: &Inner,
    accepted: Option<&ProviderSnapshot>,
) -> Option<AvailabilityTicket> {
    let snapshot = accepted?;
    if !inner.availability.has_provider(snapshot.provider) {
        return None;
    }
    Some(AvailabilityTicket {
        snapshot: snapshot.clone(),
        connection_generation: inner.schedules[provider_index(snapshot.provider)].generation,
        watch_generation: inner.availability.generation(),
    })
}

fn availability_ticket_current(inner: &Inner, ticket: &AvailabilityTicket, now: i64) -> bool {
    // The final watch may already be consumed durably before its candidate is submitted.
    inner.availability.generation() == ticket.watch_generation
        && accepted_snapshot_current(inner, &ticket.snapshot, ticket.connection_generation, now)
}

fn availability_candidates(
    inner: &mut Inner,
    ticket: &AvailabilityTicket,
    can_deliver: bool,
    now: i64,
) -> Result<Vec<RecoveredAllowance>, String> {
    if !availability_ticket_current(inner, ticket, now) {
        return Ok(Vec::new());
    }
    let max_age = notification_max_age(inner, ticket.snapshot.provider);
    inner
        .availability
        .observe(&ticket.snapshot, can_deliver, now, max_age)
}

fn notification_candidates(
    inner: &mut Inner,
    ticket: &NotificationTicket,
    can_deliver: bool,
    now: i64,
) -> Result<Vec<LowAllowance>, String> {
    let current = notification_ticket_current(inner, ticket, now);
    update_notification_permission(inner, can_deliver)?;
    if !current {
        return Ok(Vec::new());
    }
    // A permission transition invalidates queued tickets, but this accepted reading can
    // establish the new silent baseline while permission and evaluation share this lock.
    let max_age = notification_max_age(inner, ticket.snapshot.provider);
    Ok(inner.notifications.observe(
        &ticket.snapshot,
        ticket.snapshot.history_account.as_deref(),
        now,
        max_age,
    ))
}

pub fn history_state(app: &AppHandle) -> Result<HistoryState, String> {
    app.state::<Runtime>().history.state()
}

#[derive(Serialize)]
pub struct HistoryCatalog {
    accounts: Vec<crate::recording::SavedAccount>,
    current_accounts: Vec<CurrentHistoryAccount>,
}

#[derive(Serialize)]
struct CurrentHistoryAccount {
    provider: ProviderId,
    account_key: Option<String>,
    verified_at: Option<i64>,
}

fn current_history_accounts(view: &AppState) -> Vec<CurrentHistoryAccount> {
    PROVIDERS
        .into_iter()
        .map(|provider| {
            let snapshot = view
                .providers
                .iter()
                .find(|state| {
                    state.id == provider
                        && view.settings.is_enabled(provider)
                        && state.error.is_none()
                        && state.recovery.is_none()
                        && !state.stale
                })
                .and_then(|state| state.snapshot.as_ref())
                .filter(|snapshot| snapshot.history_account.is_some());
            CurrentHistoryAccount {
                provider,
                account_key: snapshot.and_then(|snapshot| snapshot.history_account.clone()),
                verified_at: snapshot.map(|snapshot| snapshot.fetched_at),
            }
        })
        .collect()
}

pub fn history_catalog(app: &AppHandle) -> Result<HistoryCatalog, String> {
    let runtime = app.state::<Runtime>();
    let accounts = runtime
        .history
        .catalog(providers::now())
        .map_err(|error| error.to_string())?;
    Ok(HistoryCatalog {
        accounts,
        current_accounts: current_history_accounts(&runtime.state()?),
    })
}

pub fn query_history(
    app: &AppHandle,
    request: crate::history_query::QueryRequest,
) -> Result<crate::history_query::QueryResult, String> {
    let runtime = app.state::<Runtime>();
    let threshold = runtime.state()?.settings.threshold;
    runtime
        .history
        .query(request, providers::now(), threshold)
        .map_err(|error| error.to_string())
}

pub fn export_history_csv(
    app: &AppHandle,
    requests: Vec<crate::history_export::ExportRequest>,
) -> Result<Vec<u8>, String> {
    app.state::<Runtime>()
        .history
        .export_csv(requests, providers::now())
        .map_err(|error| error.to_string())
}

pub fn refresh_history_state(app: &AppHandle) -> Result<HistoryState, String> {
    let runtime = app.state::<Runtime>();
    let _ = runtime.history.refresh(providers::now());
    runtime.history.state()
}

fn reset_history_connection(app: &AppHandle, provider: ProviderId) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Runtime>()
            .history
            .clear_provider_issue(provider);
        publish_history(&app);
    });
}

pub fn set_history_recording(app: &AppHandle, enabled: bool) -> Result<HistoryState, String> {
    let runtime = app.state::<Runtime>();
    {
        let mut inner = runtime
            .inner
            .lock()
            .map_err(|_| "Could not save history settings.")?;
        let mut settings = inner.view.settings.clone();
        settings.history_recording = enabled;
        settings::save(&settings).map_err(|error| error.to_string())?;
        runtime.history.set_recording(enabled)?;
        inner.view.settings = settings;
        inner.view.settings_error = None;
    }
    publish(app);
    publish_history(app);
    runtime.wake.notify_one();
    runtime.history.state()
}

pub fn set_history_retention(
    app: &AppHandle,
    retention: Retention,
) -> Result<HistoryState, String> {
    let runtime = app.state::<Runtime>();
    let result = {
        let mut inner = runtime
            .inner
            .lock()
            .map_err(|_| "Could not save history settings.")?;
        let mut settings = inner.view.settings.clone();
        settings.history_retention = retention;
        settings::save(&settings).map_err(|error| error.to_string())?;
        inner.view.settings = settings;
        inner.view.settings_error = None;
        runtime.history.set_retention(retention, providers::now())
    };
    publish(app);
    publish_history(app);
    result?;
    runtime.history.state()
}

pub fn clear_history(app: &AppHandle) -> Result<HistoryState, String> {
    let runtime = app.state::<Runtime>();
    let result = runtime.history.clear();
    publish_history(app);
    result?;
    runtime.history.state()
}

fn publish_history(app: &AppHandle) {
    // Queue events under the same lock as updates so older preferences cannot arrive last.
    let _ = app.state::<Runtime>().history.with_state(|state| {
        let _ = app.emit("history-state", state);
    });
}

fn record_history(app: &AppHandle, snapshot: Option<ProviderSnapshot>) {
    let Some(snapshot) = snapshot else {
        return;
    };
    if snapshot.history_generation.is_none() {
        return;
    }
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        let runtime = app.state::<Runtime>();
        if runtime.quitting.load(Ordering::Relaxed) {
            return;
        }
        let _ = runtime.history.record(&snapshot, providers::now());
        publish_history(&app);
    });
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
        schedule.next_due = schedule.after_last_start(10);
        if schedule.waiting_for_sign_in {
            // A new CLI sign-in can recover immediately; server cooldowns still apply.
            schedule.blocked_until = 0;
        }
        schedule.next_due = schedule.next_poll_at(state.id);
        if state.error.is_some() {
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
        runtime.history.invalidate();
        change_connection(&mut inner, index, enabled, providers::now());
        inner.view.settings_error = None;
    }
    reset_history_connection(app, provider);
    publish(app);
    runtime.wake.notify_one();
    Ok(())
}

fn change_connection(inner: &mut Inner, index: usize, enabled: bool, now: i64) {
    let state = &mut inner.view.providers[index];
    let schedule = &mut inner.schedules[index];
    inner.notifications.reset_baseline(state.id);
    inner.view.settings.set_enabled(state.id, enabled);
    if !enabled {
        inner.identity_demand[index] = false;
    }
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
        schedule.next_due = schedule.after_last_start(10);
        if schedule.waiting_for_sign_in {
            schedule.blocked_until = 0;
        }
        schedule.next_due = schedule.next_poll_at(state.id);
        if schedule.blocked_until.max(schedule.service_deadline()) > now {
            state.next_retry_at = Some(schedule.next_due);
            state.error = active_cooldown_issue(schedule, now);
        }
    }
}

fn cooldown_issue(wait: &cooldown::Wait) -> Issue {
    match wait.reason {
        cooldown::Reason::RateLimited => ProviderError::RateLimited { retry_at: None },
        cooldown::Reason::AccessDenied => ProviderError::AccessDenied { retry_at: None },
        cooldown::Reason::Service { status } => ProviderError::Service {
            status,
            retry_at: None,
        },
    }
    .issue()
}

fn active_cooldown_issue(schedule: &Schedule, now: i64) -> Option<Issue> {
    schedule
        .service_wait
        .as_ref()
        .filter(|wait| wait.retry_at > now)
        .map(cooldown_issue)
        .or_else(|| schedule.cooldown_issue.clone())
}

fn restore_claude_wait(inner: &mut Inner, store: &CooldownStore, now: i64) {
    let index = provider_index(ProviderId::Claude);
    let wait = match store.load(now) {
        Ok(wait) => wait,
        Err(error) => {
            let recovered = error.recovered_wait().cloned();
            inner.view.cooldown_error = Some(if recovered.is_some() {
                format!("{error} The Claude wait still applies while Delta-V is running.")
            } else {
                format!("{error} Claude checks are paused for five minutes.")
            });
            if recovered.is_none() {
                let schedule = &mut inner.schedules[index];
                schedule.blocked_until = now.saturating_add(CLAUDE_MIN_INTERVAL);
                schedule.next_due = schedule.blocked_until;
                schedule.cooldown_issue = Some(Issue {
                    kind: IssueKind::Service,
                    message: "Claude checks are temporarily paused because the saved wait could not be read.".to_owned(),
                });
            }
            recovered
        }
    };
    let schedule = &mut inner.schedules[index];
    if let Some(wait) = wait {
        schedule.blocked_until = wait.retry_at;
        schedule.next_due = wait.retry_at;
        schedule.failures = 1;
        schedule.cooldown_issue = Some(cooldown_issue(&wait));
        schedule.service_wait = Some(wait);
    }
    if inner.view.settings.is_enabled(ProviderId::Claude) && schedule.blocked_until > now {
        let state = &mut inner.view.providers[index];
        state.error = schedule.cooldown_issue.clone();
        state.next_retry_at = Some(schedule.blocked_until);
        state.stale = true;
    }
}

fn remember_claude_wait(schedule: &mut Schedule, error: &ProviderError, now: i64) {
    if schedule.service_deadline() <= now {
        schedule.service_wait = None;
    }
    let reason = match error {
        ProviderError::RateLimited { .. } => cooldown::Reason::RateLimited,
        ProviderError::AccessDenied { .. } => cooldown::Reason::AccessDenied,
        ProviderError::Service { status, .. } => cooldown::Reason::Service { status: *status },
        _ => return,
    };
    let retry_at = schedule
        .blocked_until
        .max(schedule.after_last_start(CLAUDE_MIN_INTERVAL));
    if retry_at > schedule.service_deadline() {
        schedule.service_wait = Some(cooldown::Wait {
            observed_at: now,
            retry_at,
            reason,
        });
    }
}

fn persist_claude_wait(inner: &mut Inner, store: &CooldownStore, now: i64) {
    let wait = inner.schedules[provider_index(ProviderId::Claude)]
        .service_wait
        .as_ref()
        .filter(|wait| wait.retry_at > now);
    // Keep writes ordered with completed responses, including responses after disconnecting.
    inner.view.cooldown_error = store.save(wait).err().map(|error| {
        let consequence = if wait.is_some() {
            "Restarting may lose the current wait or restore an older one."
        } else {
            "An older wait may return after restarting."
        };
        format!("{error} {consequence}")
    });
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
    if (!schedule.waiting_for_sign_in && now < schedule.blocked_until)
        || now < schedule.service_deadline()
    {
        return Err(
            "The provider asked us to wait. Reconnect after the countdown ends.".to_owned(),
        );
    }
    if let Some(previous) = schedule.last_recovery_started
        && now < previous.saturating_add(30)
    {
        return Err("Wait a few seconds before trying to reconnect again.".to_owned());
    }
    inner.notifications.reset_baseline(state.id);
    schedule.generation = schedule.generation.wrapping_add(1);
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
        runtime.history.invalidate();
    };
    reset_history_connection(app, provider);
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
            let permitted = schedule
                .after_last_start(10)
                .max(if schedule.waiting_for_sign_in {
                    0
                } else {
                    schedule.blocked_until
                })
                .max(schedule.service_deadline());
            if providers::now() >= permitted {
                schedule.last_started = Some(providers::now());
                true
            } else {
                false
            }
        };
        if ready {
            return Some(fetch_provider(provider, &runtime).await);
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
    let (accepted, ticket, watch_ticket) = {
        let Ok(mut inner) = runtime.inner.lock() else {
            return;
        };
        let now = providers::now();
        let accepted = complete_recovery(
            &mut inner,
            index,
            cancel.load(Ordering::Relaxed),
            result,
            issue,
            now,
        );
        if index == provider_index(ProviderId::Claude) {
            persist_claude_wait(&mut inner, &runtime.cooldown, now);
        }
        let ticket = notification_ticket(&inner, accepted.as_ref());
        let watch_ticket = availability_ticket(&inner, accepted.as_ref());
        (accepted, ticket, watch_ticket)
    };
    publish(app);
    record_history(app, accepted);
    if let Some(ticket) = ticket {
        crate::notification_delivery::observe(app, ticket);
    }
    if let Some(ticket) = watch_ticket {
        crate::notification_delivery::observe_availability(app, ticket);
    }
    runtime.wake.notify_one();
}

fn complete_recovery(
    inner: &mut Inner,
    index: usize,
    cancelled: bool,
    result: Option<Result<ProviderSnapshot, ProviderError>>,
    issue: Option<Issue>,
    now: i64,
) -> Option<ProviderSnapshot> {
    inner.schedules[index].recovery_cancel = None;
    inner.view.providers[index].recovery = None;
    if !inner
        .view
        .settings
        .is_enabled(inner.view.providers[index].id)
    {
        if let Some(Err(error)) = &result {
            preserve_service_wait(
                &mut inner.schedules[index],
                inner.view.providers[index].id,
                error,
                now,
            );
        }
        return None;
    }
    if let Some(result) = result
        && (!cancelled || result.is_err())
    {
        let cadence = inner.schedules[index].cadence.max(base_interval(
            &inner.view.settings,
            inner.view.providers[index].id,
        ));
        let accepted = result.as_ref().ok().cloned();
        apply_result(inner, index, cadence, now, result);
        return accepted;
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
    None
}

pub fn request_quit(app: &AppHandle) {
    let runtime = app.state::<Runtime>();
    if runtime.quitting.swap(true, Ordering::Relaxed) {
        return;
    }
    runtime.history.invalidate();
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
                platform::finish_panel_drag(&app).await;
                let history_app = app.clone();
                let _ = tokio::task::spawn_blocking(move || history_state(&history_app)).await;
                app.exit(0);
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut next_maintenance = 0;
        loop {
            let now = providers::now();
            if now >= next_maintenance {
                next_maintenance = now.saturating_add(3600);
                let history_app = app.clone();
                tokio::task::spawn_blocking(move || {
                    let runtime = history_app.state::<Runtime>();
                    let _ = runtime.history.maintain(providers::now());
                    publish_history(&history_app);
                });
            }
            tick(&app, platform::activity(), now);
            let runtime = app.state::<Runtime>();
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(1)) => {},
                () = runtime.wake.notified() => {},
            }
        }
    });
}

fn base_interval(settings: &Settings, provider: ProviderId) -> i64 {
    let minimum = match provider {
        ProviderId::Claude => CLAUDE_MIN_INTERVAL,
        ProviderId::Codex => 0,
    };
    (settings.refresh_seconds as i64).max(minimum)
}

fn interval(settings: &Settings, provider: ProviderId, idle_seconds: Option<f64>) -> i64 {
    let minimum = match idle_seconds {
        Some(idle) if idle < 300.0 => 0,
        Some(idle) if idle < 900.0 => 180,
        _ => 600,
    };
    base_interval(settings, provider).max(minimum)
}

fn should_poll(settings: &Settings, provider: ProviderId) -> bool {
    settings.is_enabled(provider)
        && (settings.providers.includes(provider)
            || settings.history_recording
            || settings.notifications_enabled)
}

fn should_poll_for(inner: &Inner, settings: &Settings, provider: ProviderId) -> bool {
    should_poll(settings, provider)
        || (settings.is_enabled(provider)
            && (inner.availability.has_provider(provider)
                || inner.identity_demand[provider_index(provider)]))
}

fn needs_notification_identity(inner: &Inner, provider: ProviderId) -> bool {
    inner.view.settings.notifications_enabled
        || inner.availability.has_provider(provider)
        || inner.identity_demand[provider_index(provider)]
}

fn queue_polls(inner: &mut Inner, activity: Activity, now: i64) -> Vec<(usize, ProviderId, u64)> {
    let mut pending = Vec::new();
    let was_paused = inner.view.paused;
    inner.view.paused = activity.locked != Some(false);
    for (index, provider) in PROVIDERS.into_iter().enumerate() {
        let cadence = interval(&inner.view.settings, provider, activity.idle_seconds);
        if (inner.view.paused || !inner.view.settings.is_enabled(provider))
            && let Some(cancel) = &inner.schedules[index].recovery_cancel
        {
            cancel.store(true, Ordering::Relaxed);
            inner.view.providers[index].recovery = Some(RecoveryStage::Cancelling);
        }
        if !should_poll_for(inner, &inner.view.settings, provider) {
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
        if now < schedule.next_poll_at(provider) {
            continue;
        }
        schedule.last_started = Some(now);
        let generation = schedule.generation;
        schedule.poll_generation = Some(generation);
        inner.view.providers[index].refreshing = true;
        pending.push((index, provider, generation));
    }
    pending
}

fn tick(app: &AppHandle, activity: Activity, now: i64) {
    let runtime = app.state::<Runtime>();
    if runtime.quitting.load(Ordering::Relaxed) {
        return;
    }
    let pending;
    let changed = {
        let Ok(mut inner) = runtime.inner.lock() else {
            return;
        };
        let before = inner.view.clone();
        pending = queue_polls(&mut inner, activity, now);
        before != inner.view
    };
    if changed {
        publish(app);
    }
    for (index, provider, generation) in pending {
        let app = app.clone();
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
            let runtime = app.state::<Runtime>();
            let result = fetch_provider(provider, &runtime).await;
            finish(&app, index, generation, result);
        });
    }
}

async fn fetch_provider(
    provider: ProviderId,
    runtime: &Runtime,
) -> Result<ProviderSnapshot, ProviderError> {
    let generation = runtime.history.ticket();
    // Watch preparation obtains identity with a new usage response, never an older snapshot.
    let needs_identity = generation.is_some()
        || runtime
            .inner
            .lock()
            .is_ok_and(|inner| needs_notification_identity(&inner, provider));
    let client = &runtime.client;
    let mut snapshot = match provider {
        ProviderId::Claude => {
            ClaudeProvider {
                client,
                identity: needs_identity.then_some(&runtime.identity),
            }
            .fetch()
            .await
        }
        ProviderId::Codex => CodexProvider { client }.fetch().await,
    }?;
    snapshot.history_generation = generation;
    Ok(snapshot)
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
    let (accepted, ticket, watch_ticket) = {
        let Ok(mut inner) = runtime.inner.lock() else {
            return;
        };
        let now = providers::now();
        let accepted = finish_poll(&mut inner, index, generation, now, result);
        if index == provider_index(ProviderId::Claude) {
            persist_claude_wait(&mut inner, &runtime.cooldown, now);
        }
        let ticket = notification_ticket(&inner, accepted.as_ref());
        let watch_ticket = availability_ticket(&inner, accepted.as_ref());
        (accepted, ticket, watch_ticket)
    };
    publish(app);
    record_history(app, accepted);
    if let Some(ticket) = ticket {
        crate::notification_delivery::observe(app, ticket);
    }
    if let Some(ticket) = watch_ticket {
        crate::notification_delivery::observe_availability(app, ticket);
    }
}

fn finish_poll(
    inner: &mut Inner,
    index: usize,
    generation: u64,
    now: i64,
    result: Result<ProviderSnapshot, ProviderError>,
) -> Option<ProviderSnapshot> {
    let schedule = &mut inner.schedules[index];
    if schedule.poll_generation != Some(generation) {
        return None;
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
            preserve_service_wait(schedule, inner.view.providers[index].id, error, now);
        }
        if inner
            .view
            .settings
            .is_enabled(inner.view.providers[index].id)
            && schedule.blocked_until.max(schedule.service_deadline()) > now
        {
            inner.view.providers[index].next_retry_at =
                Some(schedule.next_poll_at(inner.view.providers[index].id));
            inner.view.providers[index].error = active_cooldown_issue(schedule, now);
        }
        return None;
    }
    let cadence = schedule.cadence;
    let accepted = result.as_ref().ok().cloned();
    apply_result(inner, index, cadence, now, result);
    accepted
}

fn preserve_service_wait(
    schedule: &mut Schedule,
    provider: ProviderId,
    error: &ProviderError,
    now: i64,
) {
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
        if provider == ProviderId::Claude {
            remember_claude_wait(schedule, error, now);
        }
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
            schedule.service_wait = None;
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
            if state.id == ProviderId::Claude {
                remember_claude_wait(schedule, &error, now);
            }
            state.next_retry_at = Some(schedule.next_poll_at(state.id));
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

fn tray_tooltip(view: &AppState, reading: Option<&Reading>) -> String {
    let mode = view.settings.percentage_mode;
    let percentage_label = match mode {
        PercentageMode::Remaining => "remaining",
        PercentageMode::Used => "used",
    };
    let mut tooltip = reading.map_or_else(
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
    let limited: Vec<&str> = view
        .providers
        .iter()
        .filter(|state| {
            view.settings.providers.includes(state.id)
                && view.settings.is_enabled(state.id)
                && !state.refreshing
                && state.recovery.is_none()
                && state
                    .error
                    .as_ref()
                    .is_some_and(|issue| issue.kind == IssueKind::RateLimited)
        })
        .map(|state| match state.id {
            ProviderId::Claude => "Claude: usage checks temporarily limited.",
            ProviderId::Codex => "Codex: usage checks temporarily limited.",
        })
        .collect();
    if !limited.is_empty() {
        tooltip.push('\n');
        tooltip.push_str(&limited.join("\n"));
        tooltip.push_str("\nOpen Delta-V for check timing.");
    }
    tooltip
}

fn publish_view(app: &AppHandle, view: &AppState) {
    let reading = tray_reading(view, providers::now());
    let mode = view.settings.percentage_mode;
    let tooltip = tray_tooltip(view, reading.as_ref());
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
mod availability_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Limit;

    #[test]
    fn history_checks_hidden_connections_without_reenabling_disconnected_providers() {
        let mut settings = Settings {
            providers: settings::ProviderSelection::Claude,
            ..Settings::default()
        };
        assert!(should_poll(&settings, ProviderId::Claude));
        assert!(!should_poll(&settings, ProviderId::Codex));
        settings.history_recording = true;
        assert!(should_poll(&settings, ProviderId::Codex));
        settings.codex_enabled = false;
        assert!(!should_poll(&settings, ProviderId::Codex));
        settings.history_recording = false;
        assert!(should_poll(&settings, ProviderId::Claude));
    }

    #[test]
    fn only_accepted_successful_polls_are_offered_to_history() {
        let mut state = inner();
        let mut snapshot = state.view.providers[0].snapshot.clone().unwrap();
        snapshot.history_generation = Some(1);
        snapshot.history_account = Some("a".repeat(64));
        state.schedules[0].poll_generation = Some(0);
        let accepted = finish_poll(&mut state, 0, 0, 100, Ok(snapshot.clone())).unwrap();
        assert_eq!(accepted, snapshot);
        state.schedules[0].poll_generation = Some(0);
        assert!(finish_poll(&mut state, 0, 0, 101, Err(ProviderError::Network)).is_none());
        state.schedules[0].poll_generation = Some(0);
        change_connection(&mut state, 0, false, 102);
        assert!(finish_poll(&mut state, 0, 0, 103, Ok(snapshot)).is_none());
    }

    #[test]
    fn notifications_monitor_hidden_connected_providers_without_recording_history() {
        let mut settings = Settings {
            providers: ProviderSelection::Claude,
            notifications_enabled: true,
            ..Settings::default()
        };
        assert!(!settings.history_recording);
        assert!(should_poll(&settings, ProviderId::Codex));
        settings.codex_enabled = false;
        assert!(!should_poll(&settings, ProviderId::Codex));
        settings.codex_enabled = true;
        settings.notifications_enabled = false;
        assert!(!should_poll(&settings, ProviderId::Codex));
    }

    #[test]
    fn enabling_notifications_schedules_hidden_connections_without_resetting_service_waits() {
        let mut state = inner();
        state.view.settings.providers = ProviderSelection::Claude;
        state.schedules[0].next_due = 160;
        state.schedules[1].next_due = 600;
        state.schedules[1].blocked_until = 900;
        save_notification_choice(&mut state, true);
        assert_eq!(state.schedules[0].next_due, 160);
        assert_eq!(state.schedules[1].next_due, 0);
        assert_eq!(state.schedules[1].blocked_until, 900);
        assert!(!state.view.settings.history_recording);
    }

    fn notification_state() -> Inner {
        let mut state = inner();
        state.view.settings.notifications_enabled = true;
        state
    }

    fn notification_reading(at: i64, used: f64) -> ProviderSnapshot {
        let mut snapshot = inner().view.providers[0].snapshot.take().unwrap();
        snapshot.fetched_at = at;
        snapshot.limits[0].used_fraction = Some(used);
        snapshot.limits[0].resets_at = Some(1_000);
        snapshot.history_account = Some("a".repeat(64));
        snapshot
    }

    fn accept_notification_reading(state: &mut Inner, at: i64, used: f64) -> NotificationTicket {
        let generation = state.schedules[0].generation;
        state.schedules[0].poll_generation = Some(generation);
        let accepted = finish_poll(state, 0, generation, at, Ok(notification_reading(at, used)));
        notification_ticket(state, accepted.as_ref()).unwrap()
    }

    fn save_notification_choice(state: &mut Inner, enabled: bool) {
        save_notifications_enabled(state, enabled, 100, |_| Ok(())).unwrap();
    }

    #[test]
    fn notification_handoff_excludes_disabled_failed_and_cancelled_readings() {
        let mut state = inner();
        assert!(notification_ticket(&state, state.view.providers[0].snapshot.as_ref()).is_none());
        state.view.settings.notifications_enabled = true;
        state.schedules[0].poll_generation = Some(0);
        let failed = finish_poll(&mut state, 0, 0, 100, Err(ProviderError::Network));
        assert!(notification_ticket(&state, failed.as_ref()).is_none());
        let cancelled = complete_recovery(
            &mut state,
            0,
            true,
            Some(Ok(notification_reading(101, 0.5))),
            None,
            101,
        );
        assert!(notification_ticket(&state, cancelled.as_ref()).is_none());
        let accepted = complete_recovery(
            &mut state,
            0,
            false,
            Some(Ok(notification_reading(102, 0.5))),
            None,
            102,
        );
        let ticket = notification_ticket(&state, accepted.as_ref()).unwrap();
        assert!(notification_ticket_current(&state, &ticket, 102));
        assert!(ticket.snapshot.history_generation.is_none());
    }

    #[test]
    fn history_free_notification_readings_are_silent_first_then_cross_once() {
        let mut state = notification_state();
        let first_low = accept_notification_reading(&mut state, 100, 0.9);
        assert!(
            notification_candidates(&mut state, &first_low, true, 100)
                .unwrap()
                .is_empty()
        );
        let high = accept_notification_reading(&mut state, 101, 0.5);
        assert!(
            notification_candidates(&mut state, &high, true, 101)
                .unwrap()
                .is_empty()
        );
        let low = accept_notification_reading(&mut state, 102, 0.9);
        assert!(!state.view.settings.history_recording);
        assert!(low.snapshot.history_generation.is_none());
        assert_eq!(
            notification_candidates(&mut state, &low, true, 102)
                .unwrap()
                .len(),
            1
        );
        assert!(
            notification_candidates(&mut state, &low, true, 102)
                .unwrap()
                .is_empty()
        );
        assert!(!notification_ticket_current(&state, &high, 102));
    }

    #[test]
    fn notification_permission_changes_rebaseline_without_rearming_an_emitted_period() {
        let mut state = notification_state();
        for (at, used, permission, count) in [
            (100, 0.5, true, 0),
            (101, 0.9, false, 0),
            (102, 0.9, true, 0),
            (103, 0.5, true, 0),
            (104, 0.9, true, 1),
            (105, 0.5, false, 0),
            (106, 0.5, true, 0),
            (107, 0.9, true, 0),
        ] {
            let ticket = accept_notification_reading(&mut state, at, used);
            assert_eq!(
                notification_candidates(&mut state, &ticket, permission, at)
                    .unwrap()
                    .len(),
                count
            );
        }
    }

    #[test]
    fn permission_reads_without_polls_invalidate_old_work_only_when_availability_changes() {
        let mut state = notification_state();
        let high = accept_notification_reading(&mut state, 100, 0.5);
        notification_candidates(&mut state, &high, true, 100).unwrap();
        let queued = accept_notification_reading(&mut state, 101, 0.9);
        update_notification_permission(&mut state, true).unwrap();
        update_notification_permission(&mut state, true).unwrap();
        assert!(notification_ticket_current(&state, &queued, 101));
        update_notification_permission(&mut state, false).unwrap();
        let generation = state.notification_generation;
        update_notification_permission(&mut state, false).unwrap();
        assert_eq!(state.notification_generation, generation);
        update_notification_permission(&mut state, true).unwrap();
        assert!(!notification_ticket_current(&state, &queued, 101));
        for (at, used, count) in [(102, 0.9, 0), (103, 0.5, 0), (104, 0.9, 1)] {
            let ticket = accept_notification_reading(&mut state, at, used);
            assert_eq!(
                notification_candidates(&mut state, &ticket, true, at)
                    .unwrap()
                    .len(),
                count
            );
        }
    }

    #[test]
    fn explicitly_enabling_an_enabled_preference_requires_a_fresh_notification_baseline() {
        let mut state = notification_state();
        let high = accept_notification_reading(&mut state, 100, 0.5);
        notification_candidates(&mut state, &high, true, 100).unwrap();
        let queued = accept_notification_reading(&mut state, 101, 0.9);
        assert!(
            save_notifications_enabled(&mut state, true, 101, |_| Err(SettingsError::Threshold))
                .is_err()
        );
        assert!(notification_ticket_current(&state, &queued, 101));
        save_notification_choice(&mut state, true);
        assert!(!notification_ticket_current(&state, &queued, 101));
        for (at, used, count) in [(102, 0.9, 0), (103, 0.5, 0), (104, 0.9, 1)] {
            let ticket = accept_notification_reading(&mut state, at, used);
            assert_eq!(
                notification_candidates(&mut state, &ticket, true, at)
                    .unwrap()
                    .len(),
                count
            );
        }
    }

    #[test]
    fn consumed_notification_crossings_are_not_replayed_after_delivery_failure() {
        let mut state = notification_state();
        let high = accept_notification_reading(&mut state, 100, 0.5);
        notification_candidates(&mut state, &high, true, 100).unwrap();
        let low = accept_notification_reading(&mut state, 101, 0.9);
        let consumed = notification_candidates(&mut state, &low, true, 101).unwrap();
        assert_eq!(consumed.len(), 1);
        // Delivery happens outside the runtime. Dropping failed submissions cannot rearm them.
        drop(consumed);
        assert!(
            notification_candidates(&mut state, &low, true, 101)
                .unwrap()
                .is_empty()
        );
        for (at, used) in [(102, 0.9), (103, 0.5), (104, 0.9)] {
            let ticket = accept_notification_reading(&mut state, at, used);
            assert!(
                notification_candidates(&mut state, &ticket, true, at)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn notification_policy_changes_invalidate_queued_work_and_preserve_deduplication() {
        let mut state = notification_state();
        let high = accept_notification_reading(&mut state, 100, 0.5);
        notification_candidates(&mut state, &high, true, 100).unwrap();
        let queued = accept_notification_reading(&mut state, 101, 0.9);
        save_notification_choice(&mut state, false);
        save_notification_choice(&mut state, true);
        assert!(!notification_ticket_current(&state, &queued, 101));
        for (at, used, count) in [(102, 0.9, 0), (103, 0.5, 0), (104, 0.9, 1)] {
            let ticket = accept_notification_reading(&mut state, at, used);
            assert_eq!(
                notification_candidates(&mut state, &ticket, true, at)
                    .unwrap()
                    .len(),
                count
            );
        }
        let queued = accept_notification_reading(&mut state, 105, 0.5);
        save_settings_change(
            &mut state,
            |current| {
                let mut settings = current.clone();
                settings.threshold = 15;
                Ok(settings)
            },
            105,
            |_| Ok(()),
        )
        .unwrap();
        assert!(!notification_ticket_current(&state, &queued, 105));
        for (at, used) in [(106, 0.5), (107, 0.9)] {
            let ticket = accept_notification_reading(&mut state, at, used);
            assert!(
                notification_candidates(&mut state, &ticket, true, at)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn unrelated_or_failed_settings_saves_do_not_invalidate_notification_tickets() {
        let mut state = notification_state();
        let ticket = accept_notification_reading(&mut state, 100, 0.5);
        save_settings_change(
            &mut state,
            |current| {
                let mut settings = current.clone();
                settings.providers = ProviderSelection::Codex;
                settings.theme = settings::Theme::Dark;
                settings.percentage_mode = PercentageMode::Used;
                settings.history_recording = true;
                Ok(settings)
            },
            100,
            |_| Ok(()),
        )
        .unwrap();
        assert!(notification_ticket_current(&state, &ticket, 100));
        assert!(
            save_settings_change(
                &mut state,
                |current| {
                    let mut settings = current.clone();
                    settings.notifications_enabled = false;
                    Ok(settings)
                },
                100,
                |_| Err(SettingsError::Threshold)
            )
            .is_err()
        );
        assert!(notification_ticket_current(&state, &ticket, 100));
    }

    #[test]
    fn connection_and_recovery_invalidate_notification_tickets_and_rebaseline() {
        let mut state = notification_state();
        let high = accept_notification_reading(&mut state, 100, 0.5);
        notification_candidates(&mut state, &high, true, 100).unwrap();
        let queued = accept_notification_reading(&mut state, 101, 0.9);
        change_connection(&mut state, 0, false, 101);
        change_connection(&mut state, 0, true, 101);
        assert!(!notification_ticket_current(&state, &queued, 101));
        let low = accept_notification_reading(&mut state, 102, 0.9);
        assert!(
            notification_candidates(&mut state, &low, true, 102)
                .unwrap()
                .is_empty()
        );
        let high = accept_notification_reading(&mut state, 103, 0.5);
        notification_candidates(&mut state, &high, true, 103).unwrap();
        begin_recovery(
            &mut state,
            0,
            platform::auth::Action::SignIn,
            Arc::new(AtomicBool::new(false)),
            104,
        )
        .unwrap();
        assert!(!notification_ticket_current(&state, &high, 104));
        let accepted = complete_recovery(
            &mut state,
            0,
            false,
            Some(Ok(notification_reading(105, 0.9))),
            None,
            105,
        );
        let recovered = notification_ticket(&state, accepted.as_ref()).unwrap();
        assert!(
            notification_candidates(&mut state, &recovered, true, 105)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn notification_ticket_checks_current_state_and_the_polling_freshness_allowance() {
        let mut state = notification_state();
        let ticket = accept_notification_reading(&mut state, 100, 0.5);
        assert!(!notification_ticket_current(&state, &ticket, 99));
        assert!(notification_ticket_current(&state, &ticket, 430));
        assert!(!notification_ticket_current(&state, &ticket, 431));
        state.schedules[0].cadence = 600;
        assert!(notification_ticket_current(&state, &ticket, 730));
        assert!(!notification_ticket_current(&state, &ticket, 731));
        state.view.paused = true;
        assert!(!notification_ticket_current(&state, &ticket, 100));
        state.view.paused = false;
        state.view.providers[0].error = Some(ProviderError::Network.issue());
        assert!(!notification_ticket_current(&state, &ticket, 100));
        state.view.providers[0].error = None;
        state.view.providers[0].recovery = Some(RecoveryStage::Checking);
        assert!(!notification_ticket_current(&state, &ticket, 100));
    }

    #[test]
    fn an_expired_sibling_quota_does_not_hide_a_fresh_notification_crossing() {
        let mut state = notification_state();
        let high = accept_notification_reading(&mut state, 100, 0.5);
        notification_candidates(&mut state, &high, true, 100).unwrap();
        let mut reading = notification_reading(101, 0.9);
        let mut expired = reading.limits[0].clone();
        expired.id = "weekly".into();
        expired.resets_at = Some(100);
        reading.limits.push(expired);
        state.schedules[0].poll_generation = Some(0);
        let accepted = finish_poll(&mut state, 0, 0, 101, Ok(reading));
        let ticket = notification_ticket(&state, accepted.as_ref()).unwrap();
        assert!(state.view.providers[0].stale);
        let candidates = notification_candidates(&mut state, &ticket, true, 101).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].limit_id, "session");
    }

    #[test]
    fn interleaved_display_and_provider_changes_preserve_each_others_fields() {
        for display_first in [true, false] {
            let mut state = inner();
            let draft = DisplayPreferences {
                tracked_limit: "codex:weekly".into(),
                threshold: 15,
                refresh_seconds: 120,
                theme: settings::Theme::Dark,
                percentage_mode: PercentageMode::Used,
                claude_windows: vec!["session".into(), "weekly".into()],
                codex_windows: vec!["weekly".into()],
            };
            // These immediate actions occurred after the form was opened.
            state.view.settings.claude_enabled = false;
            state.view.settings.codex_enabled = false;
            state.view.settings.launch_at_login_prompt_dismissed = true;
            state.view.settings.history_recording = true;
            state.view.settings.history_retention = Retention::Days90;
            let display = |state: &mut Inner| {
                save_settings_change(
                    state,
                    |current| draft.clone().apply_to(current),
                    100,
                    |_| Ok(()),
                )
                .unwrap()
            };
            let selection = |state: &mut Inner| {
                save_settings_change(
                    state,
                    |current| current.with_provider_selection(ProviderSelection::Claude),
                    100,
                    |_| Ok(()),
                )
                .unwrap()
            };
            let (first, last) = if display_first {
                (display(&mut state), selection(&mut state))
            } else {
                (selection(&mut state), display(&mut state))
            };
            assert!(last.revision > first.revision);
            let saved = last.settings;
            assert_eq!(saved.providers, ProviderSelection::Claude);
            assert_eq!(saved.tracked_limit, "auto");
            assert_eq!(saved.threshold, draft.threshold);
            assert_eq!(saved.refresh_seconds, draft.refresh_seconds);
            assert_eq!(saved.theme, draft.theme);
            assert_eq!(saved.percentage_mode, draft.percentage_mode);
            assert_eq!(saved.claude_windows, draft.claude_windows);
            assert_eq!(saved.codex_windows, draft.codex_windows);
            assert!(!saved.claude_enabled);
            assert!(!saved.codex_enabled);
            assert!(saved.launch_at_login_prompt_dismissed);
            assert!(saved.history_recording);
            assert_eq!(saved.history_retention, Retention::Days90);
        }
    }

    #[test]
    fn narrow_settings_changes_leave_service_waits_and_inflight_polls_intact() {
        let mut state = inner();
        for (index, schedule) in state.schedules.iter_mut().enumerate() {
            schedule.generation = index as u64 + 4;
            schedule.poll_generation = Some(schedule.generation);
            schedule.next_due = if index == 0 { 500 } else { 250 };
            schedule.blocked_until = 700;
            schedule.last_started = Some(190);
            schedule.failures = 2;
            schedule.cadence = 600;
            schedule.waiting_for_sign_in = true;
        }
        let mut draft = DisplayPreferences::from(&state.view.settings);
        draft.theme = settings::Theme::Dark;
        save_settings_change(
            &mut state,
            |current| draft.clone().apply_to(current),
            200,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(state.schedules[0].next_due, 500);
        assert_eq!(state.schedules[1].next_due, 250);

        draft.refresh_seconds = 120;
        save_settings_change(
            &mut state,
            |current| draft.apply_to(current),
            200,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(state.schedules[0].next_due, 320);
        assert_eq!(state.schedules[1].next_due, 250);
        save_settings_change(
            &mut state,
            |current| current.with_provider_selection(ProviderSelection::Claude),
            200,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(state.schedules[0].next_due, 320);
        assert_eq!(state.schedules[1].next_due, 0);
        for (index, schedule) in state.schedules.iter().enumerate() {
            assert_eq!(schedule.blocked_until, 700);
            assert_eq!(schedule.next_due.max(schedule.blocked_until), 700);
            assert_eq!(schedule.generation, index as u64 + 4);
            assert_eq!(schedule.poll_generation, Some(schedule.generation));
            assert_eq!(schedule.last_started, Some(190));
            assert_eq!(schedule.failures, 2);
            assert_eq!(schedule.cadence, 600);
            assert!(schedule.waiting_for_sign_in);
        }
    }

    #[test]
    fn failed_settings_changes_do_not_publish_or_reschedule_unsaved_choices() {
        let mut state = inner();
        state.view.settings_error = Some("Previous settings error".into());
        state.schedules[0].next_due = 500;
        let original = state.view.clone();
        let mut draft = DisplayPreferences::from(&original.settings);
        draft.threshold = 101;
        assert!(
            save_settings_change(
                &mut state,
                |current| draft.clone().apply_to(current),
                200,
                |_| panic!("Invalid settings must not be written"),
            )
            .is_err()
        );
        assert!(state.view == original);
        draft.threshold = 15;
        draft.refresh_seconds = 120;
        assert!(
            save_settings_change(
                &mut state,
                |current| draft.clone().apply_to(current),
                200,
                |_| Err(SettingsError::Io(std::io::Error::other(
                    "Synthetic write failure"
                ))),
            )
            .is_err()
        );
        assert!(state.view == original);
        assert_eq!(state.schedules[0].next_due, 500);

        let saved = save_settings_change(
            &mut state,
            |current| draft.apply_to(current),
            200,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(saved.settings.threshold, 15);
        assert_eq!(saved.settings_error, None);
        assert_eq!(saved.revision, 1);
        assert_eq!(state.schedules[0].next_due, 320);
    }

    #[test]
    fn snapshot_revisions_order_reads_command_replies_and_event_snapshots() {
        let mut state = inner();
        assert_eq!(state.view.revision, 0);
        let initial_read = state.snapshot();
        let mut draft = DisplayPreferences::from(&initial_read.settings);
        draft.threshold = 15;
        let reply = save_settings_change(
            &mut state,
            |current| draft.apply_to(current),
            100,
            |_| Ok(()),
        )
        .unwrap();
        let event = state.snapshot();
        assert!(initial_read.revision < reply.revision);
        assert!(reply.revision < event.revision);
        assert_eq!(initial_read.settings.threshold, 20);
        assert_eq!(reply.settings, event.settings);
        assert_eq!(event.settings.threshold, 15);
        assert_eq!(
            serde_json::to_value(&event).unwrap()["revision"],
            event.revision
        );
    }

    fn inner() -> Inner {
        Inner {
            view: AppState {
                revision: 0,
                settings: Settings::default(),
                providers: vec![ProviderState {
                    id: ProviderId::Claude,
                    snapshot: Some(ProviderSnapshot {
                        provider: ProviderId::Claude,
                        history_account: None,
                        history_generation: None,
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
                cooldown_error: None,
                paused: false,
            },
            schedules: Default::default(),
            notifications: Evaluator::default(),
            notification_generation: 0,
            notification_permission: None,
            availability: AvailabilityWatches::load(AvailabilityStore::new(None)),
            identity_demand: [false; 2],
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
    fn history_current_account_requires_a_fresh_verified_connected_snapshot() {
        let mut view = inner().view;
        assert!(
            current_history_accounts(&view)
                .iter()
                .all(|account| account.account_key.is_none())
        );
        view.providers[0].snapshot.as_mut().unwrap().history_account = Some("a".repeat(64));
        let accounts = current_history_accounts(&view);
        assert_eq!(accounts[0].provider, ProviderId::Claude);
        assert_eq!(
            accounts[0].account_key.as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(accounts[0].verified_at, Some(100));
        assert_eq!(accounts[1].provider, ProviderId::Codex);
        assert!(accounts[1].account_key.is_none());

        for reason in 0..4 {
            let mut unavailable = view.clone();
            match reason {
                0 => unavailable.settings.claude_enabled = false,
                1 => unavailable.providers[0].stale = true,
                2 => unavailable.providers[0].recovery = Some(RecoveryStage::SigningIn),
                _ => {
                    unavailable.providers[0].error = Some(Issue {
                        kind: IssueKind::Authentication,
                        message: "Sign in again.".into(),
                    })
                }
            }
            let account = current_history_accounts(&unavailable).remove(0);
            assert!(account.account_key.is_none());
            assert!(account.verified_at.is_none());
        }
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
    fn tray_tooltip_preserves_readings_and_ordinary_errors() {
        let mut view = inner().view;
        for (mode, percentage) in [
            (PercentageMode::Remaining, "19% remaining"),
            (PercentageMode::Used, "81% used"),
        ] {
            view.settings.percentage_mode = mode;
            assert_eq!(
                tray_tooltip(&view, tray_reading(&view, 150).as_ref()),
                format!("Delta-V: {percentage}\nClaude · 5-hour")
            );
        }
        view.providers[0].stale = true;
        view.providers[0].error = Some(Issue {
            kind: IssueKind::Network,
            message: "Network unavailable.".into(),
        });
        assert_eq!(
            tray_tooltip(&view, tray_reading(&view, 150).as_ref()),
            "Delta-V: 81% used\nClaude · 5-hour\nLast reading, waiting for an update"
        );
        view.providers[0].snapshot = None;
        assert_eq!(tray_tooltip(&view, None), "Delta-V: usage unavailable");
    }

    #[test]
    fn tray_tooltip_explains_rate_limits_with_and_without_a_reading() {
        let mut view = inner().view;
        view.providers[0].error = Some(ProviderError::RateLimited { retry_at: None }.issue());
        view.providers[0].stale = true;
        let explanation =
            "\nClaude: usage checks temporarily limited.\nOpen Delta-V for check timing.";
        assert_eq!(
            tray_tooltip(&view, tray_reading(&view, 150).as_ref()),
            format!(
                "Delta-V: 19% remaining\nClaude · 5-hour\nLast reading, waiting for an update{explanation}"
            )
        );
        view.providers[0].snapshot = None;
        assert_eq!(
            tray_tooltip(&view, None),
            format!("Delta-V: usage unavailable{explanation}")
        );
    }

    #[test]
    fn tray_tooltip_names_the_limited_provider_beside_another_providers_reading() {
        let mut view = inner().view;
        let mut codex = view.providers[0].clone();
        codex.id = ProviderId::Codex;
        codex.snapshot.as_mut().unwrap().provider = ProviderId::Codex;
        view.providers.push(codex);
        view.providers[0].snapshot = None;
        view.providers[0].error = Some(ProviderError::RateLimited { retry_at: None }.issue());
        assert_eq!(
            tray_tooltip(&view, tray_reading(&view, 150).as_ref()),
            "Delta-V: 19% remaining\nCodex · 5-hour\nClaude: usage checks temporarily limited.\nOpen Delta-V for check timing."
        );
        view.providers[1].error = Some(ProviderError::RateLimited { retry_at: None }.issue());
        let tooltip = tray_tooltip(&view, tray_reading(&view, 150).as_ref());
        assert!(tooltip.contains("Claude: usage checks temporarily limited."));
        assert!(tooltip.contains("Codex: usage checks temporarily limited."));
        assert_eq!(tooltip.matches("Open Delta-V for check timing.").count(), 1);
    }

    #[test]
    fn tray_tooltip_omits_limits_for_hidden_disconnected_or_busy_providers() {
        let mut view = inner().view;
        view.providers[0].snapshot = None;
        view.providers[0].error = Some(ProviderError::RateLimited { retry_at: None }.issue());
        for reason in 0..4 {
            let mut excluded = view.clone();
            match reason {
                0 => excluded.settings.providers = ProviderSelection::Codex,
                1 => excluded.settings.claude_enabled = false,
                2 => excluded.providers[0].refreshing = true,
                _ => excluded.providers[0].recovery = Some(RecoveryStage::Checking),
            }
            assert_eq!(tray_tooltip(&excluded, None), "Delta-V: usage unavailable");
        }
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
        inner.schedules[0].last_started = Some(100);
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
            inner.schedules[0].last_started = Some(100);
            apply_result(&mut inner, 0, 60, 101, Err(ProviderError::Authentication));
            apply_result(&mut inner, 0, 60, 102, Err(error));
            let blocked_until = inner.schedules[0].blocked_until;
            assert!(blocked_until > 110);
            assert!(!inner.schedules[0].waiting_for_sign_in);

            queue_manual_refresh(&mut inner);
            let schedule = &inner.schedules[0];
            assert_eq!(schedule.blocked_until, blocked_until);
            assert_eq!(schedule.next_due, blocked_until.max(400));
            assert_eq!(
                inner.view.providers[0].next_retry_at,
                Some(blocked_until.max(400))
            );
        }
    }

    #[test]
    fn repeated_manual_checks_keep_ten_second_spacing_and_inflight_state() {
        let mut inner = inner();
        inner.schedules[0].last_started = Some(100);
        apply_result(&mut inner, 0, 60, 101, Err(ProviderError::Authentication));
        for _ in 0..3 {
            queue_manual_refresh(&mut inner);
            assert_eq!(inner.schedules[0].next_due, 110);
        }

        inner.schedules[0].last_started = Some(110);
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
        inner.schedules[0].last_started = Some(100);
        change_connection(&mut inner, 0, false, 101);
        change_connection(&mut inner, 0, true, 102);
        assert_eq!(inner.schedules[0].next_due, 400);
        assert!(inner.view.providers[0].refreshing);
        finish_poll(&mut inner, 0, 0, 103, Ok(snapshot.clone()));
        assert!(inner.view.providers[0].snapshot.is_none());
        assert!(!inner.view.providers[0].refreshing);

        let generation = inner.schedules[0].generation;
        inner.schedules[0].poll_generation = Some(generation);
        inner.view.providers[0].refreshing = true;
        finish_poll(&mut inner, 0, 0, 401, Ok(snapshot.clone()));
        assert!(inner.view.providers[0].refreshing);
        snapshot.fetched_at = 402;
        finish_poll(&mut inner, 0, generation, 402, Ok(snapshot));
        assert_eq!(
            inner.view.providers[0]
                .snapshot
                .as_ref()
                .unwrap()
                .fetched_at,
            402
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
    fn claude_poll_triggers_share_minimum_spacing() {
        for trigger in ["manual", "unlock", "selection", "reset", "interval"] {
            let mut state = polling_inner(ProviderSelection::Claude);
            match trigger {
                "manual" => {
                    for _ in 0..3 {
                        queue_manual_refresh(&mut state);
                    }
                }
                "unlock" => state.view.paused = true,
                "selection" => {
                    state.view.settings.providers = ProviderSelection::Codex;
                    save_settings_change(
                        &mut state,
                        |settings| settings.with_provider_selection(ProviderSelection::Claude),
                        101,
                        |_| Ok(()),
                    )
                    .unwrap();
                }
                "reset" => {
                    let snapshot = state.view.providers[0].snapshot.as_mut().unwrap();
                    snapshot.limits[0].resets_at = Some(200);
                    state.schedules[0].next_due = next_refresh(snapshot, 300, 101);
                }
                "interval" => {
                    let mut draft = DisplayPreferences::from(&state.view.settings);
                    draft.refresh_seconds = 30;
                    save_settings_change(
                        &mut state,
                        |settings| draft.apply_to(settings),
                        101,
                        |_| Ok(()),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                queue_polls(&mut state, active(), 399).is_empty(),
                "{trigger}"
            );
            if trigger == "reset" {
                assert!(state.view.providers[0].stale);
                assert!(tray_reading(&state.view, 399).is_none());
            }
            assert_eq!(queue_polls(&mut state, active(), 400).len(), 1, "{trigger}");
            assert!(
                queue_polls(&mut state, active(), 400).is_empty(),
                "{trigger}"
            );
        }
    }

    struct CooldownDirectory(std::path::PathBuf);

    impl CooldownDirectory {
        fn new() -> Self {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "delta-v-runtime-cooldown-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> std::path::PathBuf {
            self.0.join("claude-cooldown.toml")
        }
        fn store(&self) -> CooldownStore {
            CooldownStore::new(Some(self.path()))
        }
    }

    impl Drop for CooldownDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fresh_start() -> Inner {
        let mut state = polling_inner(ProviderSelection::Both);
        state.schedules = Default::default();
        for provider in &mut state.view.providers {
            provider.snapshot = None;
            provider.stale = true;
        }
        state
    }

    #[test]
    fn saved_claude_wait_blocks_startup_and_manual_checks_without_blocking_codex() {
        let directory = CooldownDirectory::new();
        let store = directory.store();
        let mut state = polling_inner(ProviderSelection::Both);
        apply_result(
            &mut state,
            0,
            300,
            101,
            Err(ProviderError::RateLimited {
                retry_at: Some(3700),
            }),
        );
        persist_claude_wait(&mut state, &store, 101);
        assert!(state.view.cooldown_error.is_none());

        let mut restarted = fresh_start();
        restore_claude_wait(&mut restarted, &store, 200);
        assert!(restarted.view.providers[0].snapshot.is_none());
        assert!(restarted.view.providers[0].stale);
        assert_eq!(restarted.view.providers[0].next_retry_at, Some(3700));
        assert_eq!(
            restarted.view.providers[0].error.as_ref().unwrap().kind,
            IssueKind::RateLimited
        );
        queue_manual_refresh(&mut restarted);
        assert_eq!(
            queue_polls(&mut restarted, active(), 200),
            vec![(1, ProviderId::Codex, 0)]
        );
        restarted.view.paused = true;
        assert!(queue_polls(&mut restarted, active(), 3699).is_empty());
        assert_eq!(
            queue_polls(&mut restarted, active(), 3700),
            vec![(0, ProviderId::Claude, 0)]
        );
    }

    #[test]
    fn persisted_service_errors_keep_the_floor_but_network_and_auth_waits_are_not_saved() {
        let directory = CooldownDirectory::new();
        let store = directory.store();
        for error in [
            ProviderError::RateLimited { retry_at: None },
            ProviderError::RateLimited {
                retry_at: Some(101),
            },
            ProviderError::AccessDenied {
                retry_at: Some(120),
            },
            ProviderError::Service {
                status: 503,
                retry_at: None,
            },
        ] {
            let mut state = polling_inner(ProviderSelection::Claude);
            apply_result(&mut state, 0, 300, 101, Err(error));
            persist_claude_wait(&mut state, &store, 101);
            assert_eq!(store.load(102).unwrap().unwrap().retry_at, 400);
            let mut restarted = fresh_start();
            restore_claude_wait(&mut restarted, &store, 102);
            restarted.view.settings.providers = ProviderSelection::Claude;
            queue_manual_refresh(&mut restarted);
            assert!(queue_polls(&mut restarted, active(), 399).is_empty());
            assert_eq!(queue_polls(&mut restarted, active(), 400).len(), 1);
        }
        for error in [
            ProviderError::Network,
            ProviderError::Authentication,
            ProviderError::Response("Invalid".into()),
        ] {
            let mut state = polling_inner(ProviderSelection::Claude);
            apply_result(&mut state, 0, 300, 101, Err(error));
            persist_claude_wait(&mut state, &store, 101);
            assert!(store.load(102).unwrap().is_none());
        }
        let mut state = polling_inner(ProviderSelection::Both);
        apply_result(
            &mut state,
            1,
            60,
            101,
            Err(ProviderError::RateLimited {
                retry_at: Some(3700),
            }),
        );
        assert!(state.schedules[1].service_wait.is_none());
    }

    #[test]
    fn late_service_errors_survive_disconnect_and_cancelled_recovery() {
        for recovering in [false, true] {
            let directory = CooldownDirectory::new();
            let store = directory.store();
            let mut state = polling_inner(ProviderSelection::Claude);
            if recovering {
                begin_recovery(
                    &mut state,
                    0,
                    platform::auth::Action::SignIn,
                    Arc::new(AtomicBool::new(false)),
                    101,
                )
                .unwrap();
            } else {
                state.schedules[0].poll_generation = Some(0);
            }
            change_connection(&mut state, 0, false, 102);
            let result = Err(ProviderError::RateLimited {
                retry_at: Some(3700),
            });
            if recovering {
                complete_recovery(&mut state, 0, true, Some(result), None, 103);
            } else {
                finish_poll(&mut state, 0, 0, 103, result);
            }
            persist_claude_wait(&mut state, &store, 103);
            assert_eq!(store.load(104).unwrap().unwrap().retry_at, 3700);
            let mut restarted = fresh_start();
            restarted.view.settings.claude_enabled = false;
            restore_claude_wait(&mut restarted, &store, 104);
            assert!(restarted.view.providers[0].error.is_none());
            assert!(restarted.view.providers[0].next_retry_at.is_none());
            change_connection(&mut restarted, 0, true, 105);
            assert_eq!(restarted.view.providers[0].next_retry_at, Some(3700));
            assert!(
                begin_recovery(
                    &mut restarted,
                    0,
                    platform::auth::Action::SignIn,
                    Arc::new(AtomicBool::new(false)),
                    106
                )
                .is_err()
            );
        }
    }

    #[test]
    fn obsolete_success_cannot_clear_a_wait_but_accepted_success_does() {
        let directory = CooldownDirectory::new();
        let store = directory.store();
        let mut state = polling_inner(ProviderSelection::Claude);
        let snapshot = state.view.providers[0].snapshot.clone().unwrap();
        state.schedules[0].poll_generation = Some(0);
        change_connection(&mut state, 0, false, 101);
        preserve_service_wait(
            &mut state.schedules[0],
            ProviderId::Claude,
            &ProviderError::RateLimited {
                retry_at: Some(3700),
            },
            102,
        );
        assert!(finish_poll(&mut state, 0, 0, 103, Ok(snapshot.clone())).is_none());
        persist_claude_wait(&mut state, &store, 103);
        assert_eq!(store.load(104).unwrap().unwrap().retry_at, 3700);

        change_connection(&mut state, 0, true, 3700);
        let pending = queue_polls(&mut state, active(), 3700);
        assert_eq!(pending.len(), 1);
        assert!(finish_poll(&mut state, 0, pending[0].2, 3701, Ok(snapshot)).is_some());
        persist_claude_wait(&mut state, &store, 3701);
        assert!(store.load(3701).unwrap().is_none());
        let mut restarted = fresh_start();
        restore_claude_wait(&mut restarted, &store, 3702);
        assert!(restarted.view.providers[0].snapshot.is_none());
        assert_eq!(queue_polls(&mut restarted, active(), 3702).len(), 2);
    }

    #[test]
    fn newer_shorter_error_or_auth_failure_does_not_remove_an_outstanding_wait() {
        let directory = CooldownDirectory::new();
        let store = directory.store();
        let mut state = polling_inner(ProviderSelection::Claude);
        apply_result(
            &mut state,
            0,
            300,
            101,
            Err(ProviderError::RateLimited {
                retry_at: Some(3700),
            }),
        );
        apply_result(
            &mut state,
            0,
            300,
            102,
            Err(ProviderError::Service {
                status: 503,
                retry_at: Some(200),
            }),
        );
        apply_result(&mut state, 0, 300, 103, Err(ProviderError::Authentication));
        queue_manual_refresh(&mut state);
        assert_eq!(state.view.providers[0].next_retry_at, Some(3700));
        assert!(
            begin_recovery(
                &mut state,
                0,
                platform::auth::Action::Renew,
                Arc::new(AtomicBool::new(false)),
                104
            )
            .is_err()
        );
        persist_claude_wait(&mut state, &store, 104);
        assert_eq!(store.load(105).unwrap().unwrap().retry_at, 3700);
        apply_result(&mut state, 0, 300, 3701, Err(ProviderError::Authentication));
        persist_claude_wait(&mut state, &store, 3701);
        assert!(store.load(3702).unwrap().is_none());
    }

    #[test]
    fn damaged_storage_warns_and_falls_back_without_affecting_settings_or_codex() {
        let directory = CooldownDirectory::new();
        std::fs::write(directory.path(), "invalid").unwrap();
        let store = directory.store();
        let mut state = fresh_start();
        state.view.settings_error = Some("Existing settings issue".into());
        restore_claude_wait(&mut state, &store, 100);
        assert!(state.view.cooldown_error.is_some());
        assert_eq!(
            state.view.settings_error.as_deref(),
            Some("Existing settings issue")
        );
        queue_manual_refresh(&mut state);
        change_connection(&mut state, 0, false, 101);
        change_connection(&mut state, 0, true, 102);
        assert_eq!(state.view.providers[0].next_retry_at, Some(400));
        assert_eq!(
            queue_polls(&mut state, active(), 399),
            vec![(1, ProviderId::Codex, 0)]
        );
        assert_eq!(
            queue_polls(&mut state, active(), 400),
            vec![(0, ProviderId::Claude, 2)]
        );
    }

    #[test]
    fn failed_persistence_keeps_the_current_wait_and_can_recover() {
        let directory = CooldownDirectory::new();
        std::fs::create_dir(directory.path()).unwrap();
        let store = directory.store();
        let mut state = polling_inner(ProviderSelection::Claude);
        apply_result(
            &mut state,
            0,
            300,
            101,
            Err(ProviderError::RateLimited {
                retry_at: Some(3700),
            }),
        );
        persist_claude_wait(&mut state, &store, 101);
        assert!(state.view.cooldown_error.is_some());
        assert_eq!(state.view.providers[0].next_retry_at, Some(3700));
        queue_manual_refresh(&mut state);
        assert!(queue_polls(&mut state, active(), 200).is_empty());
        std::fs::remove_dir(directory.path()).unwrap();
        persist_claude_wait(&mut state, &store, 201);
        assert!(state.view.cooldown_error.is_none());
        assert_eq!(store.load(202).unwrap().unwrap().retry_at, 3700);
    }

    fn active() -> Activity {
        Activity {
            idle_seconds: Some(0.0),
            locked: Some(false),
        }
    }

    fn polling_inner(selection: ProviderSelection) -> Inner {
        let mut state = inner();
        let mut codex = state.view.providers[0].clone();
        codex.id = ProviderId::Codex;
        codex.snapshot.as_mut().unwrap().provider = ProviderId::Codex;
        state.view.providers.push(codex);
        state.view.settings.providers = selection;
        for (provider, schedule) in state.view.providers.iter_mut().zip(&mut state.schedules) {
            let snapshot = provider.snapshot.as_mut().unwrap();
            snapshot.limits[0].resets_at = Some(2000);
            schedule.last_started = Some(100);
            schedule.cadence = base_interval(&state.view.settings, provider.id);
            schedule.next_due = next_refresh(snapshot, schedule.cadence, 100);
        }
        state
    }

    #[test]
    fn claude_first_check_is_immediate_and_subsequent_checks_are_spaced() {
        let mut state = polling_inner(ProviderSelection::Claude);
        state.view.providers[0].snapshot = None;
        state.schedules[0] = Schedule::default();
        assert_eq!(queue_polls(&mut state, active(), 0).len(), 1);
        assert_eq!(state.schedules[0].last_started, Some(0));
        finish_poll(&mut state, 0, 0, 1, Err(ProviderError::Network));
        queue_manual_refresh(&mut state);
        assert!(queue_polls(&mut state, active(), 299).is_empty());
        assert_eq!(queue_polls(&mut state, active(), 300).len(), 1);
    }

    #[test]
    fn claude_retry_countdown_matches_short_and_long_service_waits() {
        for retry_at in [None, Some(101), Some(5000)] {
            let mut state = polling_inner(ProviderSelection::Claude);
            apply_result(
                &mut state,
                0,
                300,
                101,
                Err(ProviderError::RateLimited { retry_at }),
            );
            let expected = retry_at.unwrap_or(0).max(400);
            assert_eq!(state.view.providers[0].next_retry_at, Some(expected));
            queue_manual_refresh(&mut state);
            assert_eq!(state.view.providers[0].next_retry_at, Some(expected));
            assert!(queue_polls(&mut state, active(), expected - 1).is_empty());
            assert_eq!(queue_polls(&mut state, active(), expected).len(), 1);
        }
    }

    #[test]
    fn claude_spacing_resumes_after_manual_sign_in_recovery() {
        let mut state = polling_inner(ProviderSelection::Claude);
        let mut snapshot = state.view.providers[0].snapshot.clone().unwrap();
        apply_result(&mut state, 0, 300, 101, Err(ProviderError::Authentication));
        queue_manual_refresh(&mut state);
        assert!(queue_polls(&mut state, active(), 109).is_empty());
        assert_eq!(queue_polls(&mut state, active(), 110).len(), 1);
        snapshot.fetched_at = 111;
        finish_poll(&mut state, 0, 0, 111, Ok(snapshot));
        queue_manual_refresh(&mut state);
        assert!(queue_polls(&mut state, active(), 409).is_empty());
        assert_eq!(queue_polls(&mut state, active(), 410).len(), 1);
    }

    #[test]
    fn codex_keeps_its_automatic_and_manual_cadence() {
        let mut state = polling_inner(ProviderSelection::Codex);
        assert!(queue_polls(&mut state, active(), 159).is_empty());
        assert_eq!(
            queue_polls(&mut state, active(), 160),
            vec![(1, ProviderId::Codex, 0)]
        );
        let mut state = polling_inner(ProviderSelection::Codex);
        queue_manual_refresh(&mut state);
        assert!(queue_polls(&mut state, active(), 109).is_empty());
        assert_eq!(
            queue_polls(&mut state, active(), 110),
            vec![(1, ProviderId::Codex, 0)]
        );
    }

    #[test]
    fn claude_cadence_keeps_idle_slowdown_slower_preferences_and_freshness() {
        let mut state = polling_inner(ProviderSelection::Claude);
        assert_eq!(
            interval(&state.view.settings, ProviderId::Claude, Some(0.0)),
            300
        );
        assert_eq!(
            interval(&state.view.settings, ProviderId::Claude, Some(400.0)),
            300
        );
        assert_eq!(
            interval(&state.view.settings, ProviderId::Claude, Some(900.0)),
            600
        );
        assert_eq!(
            interval(&state.view.settings, ProviderId::Claude, None),
            600
        );
        assert_eq!(notification_max_age(&state, ProviderId::Claude), 330);
        assert_eq!(notification_max_age(&state, ProviderId::Codex), 90);
        assert!(!is_stale(&state.view.providers[0], 430, 300));
        assert!(is_stale(&state.view.providers[0], 431, 300));
        state.view.settings.refresh_seconds = 900;
        assert!(queue_polls(&mut state, active(), 999).is_empty());
        assert_eq!(notification_max_age(&state, ProviderId::Claude), 930);
        assert_eq!(queue_polls(&mut state, active(), 1000).len(), 1);
    }

    #[test]
    fn idle_tiers_and_backoff_are_bounded() {
        let settings = Settings::default();
        assert_eq!(interval(&settings, ProviderId::Codex, Some(0.0)), 60);
        assert_eq!(interval(&settings, ProviderId::Codex, Some(400.0)), 180);
        assert_eq!(interval(&settings, ProviderId::Codex, None), 600);
        assert!(backoff(2, 0) > backoff(1, 0));
        assert!(backoff(u32::MAX, 100) <= 910);
    }
}
