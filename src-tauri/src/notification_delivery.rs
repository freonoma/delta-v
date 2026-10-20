use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use crate::{
    availability_watches::WatchSummary,
    model::ProviderId,
    notifications::{LowAllowance, availability::RecoveredAllowance},
    platform::notifications::{self as native, Authorization, Message, PermissionState},
    providers,
    runtime::{self, AvailabilityTicket, NotificationTicket, Runtime},
};

#[derive(Default)]
pub struct Delivery {
    actions: Mutex<DeliveryState>,
    sequence: AtomicU64,
}

#[derive(Default)]
struct DeliveryState {
    error: Option<String>,
    availability_error: Option<String>,
    last_test: Option<Instant>,
}

#[derive(Serialize)]
pub struct NotificationStatus {
    enabled: bool,
    permission: Option<PermissionState>,
    error: Option<String>,
}

#[derive(Serialize)]
pub struct AvailabilityStatus {
    watches: Vec<WatchSummary>,
    permission: Option<PermissionState>,
    error: Option<String>,
}

pub async fn availability_state(app: &AppHandle) -> Result<AvailabilityStatus, String> {
    let delivery = app.state::<Delivery>();
    let saved = delivery.actions.lock().await;
    let watches = app.state::<Runtime>().availability_state()?;
    let (permission, permission_error) = match remember_permission(app, native::state().await) {
        Ok(permission) => (Some(permission), None),
        Err(error) => (None, Some(error)),
    };
    Ok(AvailabilityStatus {
        watches: watches.watches,
        permission,
        error: watches
            .error
            .or(permission_error)
            .or_else(|| saved.availability_error.clone()),
    })
}

pub async fn prepare_availability(app: &AppHandle, provider: ProviderId) -> Result<(), String> {
    let delivery = app.state::<Delivery>();
    let _saved = delivery.actions.lock().await;
    app.state::<Runtime>().prepare_availability_watch(provider)
}

pub async fn clear_availability_preparation(
    app: &AppHandle,
    provider: ProviderId,
) -> Result<(), String> {
    let delivery = app.state::<Delivery>();
    let _saved = delivery.actions.lock().await;
    app.state::<Runtime>()
        .clear_availability_preparation(provider)
}

pub async fn arm_availability(
    app: &AppHandle,
    provider: ProviderId,
    limit_id: &str,
) -> Result<String, String> {
    let delivery = app.state::<Delivery>();
    let mut saved = delivery.actions.lock().await;
    let ticket = app
        .state::<Runtime>()
        .availability_arm_ticket(provider, limit_id)?;
    let mut permission = remember_permission(app, native::state().await)?;
    if should_request_permission(&permission) {
        permission = remember_permission(app, native::request_authorization().await)?;
    }
    if let Some(error) = permission_error(&permission) {
        return Err(error);
    }
    // Revalidate the accepted usage and account after the permission prompt.
    let id = app.state::<Runtime>().arm_availability_watch(&ticket)?;
    saved.availability_error = None;
    runtime::publish(app);
    Ok(id)
}

pub async fn cancel_availability(app: &AppHandle, id: &str) -> Result<(), String> {
    let delivery = app.state::<Delivery>();
    let mut saved = delivery.actions.lock().await;
    // Cancellation does not depend on macOS being able to read notification permission.
    app.state::<Runtime>().cancel_availability_watch(id)?;
    saved.availability_error = None;
    runtime::publish(app);
    Ok(())
}

pub async fn state(app: &AppHandle) -> Result<NotificationStatus, String> {
    let delivery = app.state::<Delivery>();
    let saved = delivery.actions.lock().await;
    status(app, &saved).await
}

async fn status(app: &AppHandle, saved: &DeliveryState) -> Result<NotificationStatus, String> {
    let enabled = app
        .state::<Runtime>()
        .state()?
        .settings
        .notifications_enabled;
    let (permission, error) = match remember_permission(app, native::state().await) {
        Ok(permission) => (Some(permission), saved.error.clone()),
        Err(error) => (None, Some(error)),
    };
    Ok(NotificationStatus {
        enabled,
        permission,
        error,
    })
}

pub async fn set_enabled(app: &AppHandle, enabled: bool) -> Result<NotificationStatus, String> {
    let delivery = app.state::<Delivery>();
    let mut saved = delivery.actions.lock().await;
    if enabled {
        let mut permission = remember_permission(app, native::state().await)?;
        if should_request_permission(&permission) {
            permission = remember_permission(app, native::request_authorization().await)?;
        }
        if let Some(error) = permission_error(&permission) {
            return Err(error);
        }
    }
    // Disabling always saves, including when macOS cannot report its permission state.
    app.state::<Runtime>().set_notifications_enabled(enabled)?;
    saved.error = None;
    runtime::publish(app);
    status(app, &saved).await
}

pub async fn send_test(app: &AppHandle) -> Result<(), String> {
    let delivery = app.state::<Delivery>();
    let mut saved = delivery.actions.lock().await;
    if !app
        .state::<Runtime>()
        .state()?
        .settings
        .notifications_enabled
    {
        return Err("Enable low-allowance notifications before sending a test.".into());
    }
    let permission = remember_permission(app, native::state().await)?;
    if let Some(error) = permission_error(&permission) {
        return Err(error);
    }
    if saved
        .last_test
        .is_some_and(|last| last.elapsed() < Duration::from_secs(10))
    {
        return Err("Wait a few seconds before sending another test.".into());
    }
    saved.last_test = Some(Instant::now());
    let result = native::send(Message {
        identifier: identifier(&delivery, "test"),
        title: "Delta-V test notification".into(),
        body: "Low-allowance notifications are enabled. This is a test, not a usage alert.".into(),
    })
    .await
    .map_err(|error| error.to_string());
    saved.error = result.as_ref().err().cloned();
    result
}

pub fn observe(app: &AppHandle, ticket: NotificationTicket) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let delivery = app.state::<Delivery>();
        let mut saved = delivery.actions.lock().await;
        let runtime = app.state::<Runtime>();
        if !runtime.notification_ticket_current(&ticket) {
            return;
        }
        let permission = native::state().await;
        let can_deliver = permission.as_ref().is_ok_and(PermissionState::can_deliver);
        let mut candidates = match runtime.notification_candidates(&ticket, can_deliver) {
            Ok(candidates) => candidates,
            Err(error) => {
                saved.error = Some(error);
                return;
            }
        };
        if let Err(error) = permission {
            saved.error = Some(error.to_string());
        }
        if candidates.is_empty() || !runtime.notification_ticket_current(&ticket) {
            return;
        }
        candidates.retain(|candidate| candidate.resets_at > providers::now());
        let Ok(view) = runtime.state() else { return };
        let Some(body) = allowance_body(&candidates, view.settings.threshold) else {
            return;
        };
        let provider = match ticket.snapshot.provider {
            ProviderId::Claude => "Claude",
            ProviderId::Codex => "Codex",
        };
        if !runtime.notification_ticket_current(&ticket) {
            return;
        }
        // Candidates are consumed once, even if macOS rejects delivery. Never replay old alerts.
        let result = native::send(Message {
            identifier: identifier(&delivery, "allowance"),
            title: format!("{provider} allowance is low"),
            body,
        })
        .await;
        saved.error = result.err().map(|error| error.to_string());
    });
}

pub fn observe_availability(app: &AppHandle, ticket: AvailabilityTicket) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let delivery = app.state::<Delivery>();
        let mut saved = delivery.actions.lock().await;
        let runtime = app.state::<Runtime>();
        if !runtime.availability_ticket_current(&ticket) {
            return;
        }
        let permission = remember_permission(&app, native::state().await);
        let can_deliver = permission.as_ref().is_ok_and(PermissionState::can_deliver);
        let candidates = match runtime.availability_candidates(&ticket, can_deliver) {
            Ok(candidates) => candidates,
            Err(_) => {
                // Registry persistence errors are exposed by availability_state.
                runtime::publish(&app);
                return;
            }
        };
        if candidates.is_empty() {
            return;
        }
        // Durable consumption has already happened. A failure or process exit below
        // can miss a delivery, but must never replay this recovery after a restart.
        runtime::publish(&app);
        let mut failed = false;
        for candidate in candidates {
            if !runtime.availability_ticket_current(&ticket) {
                return;
            }
            if candidate.resets_at <= providers::now() {
                continue;
            }
            let provider = match candidate.provider {
                ProviderId::Claude => "Claude",
                ProviderId::Codex => "Codex",
            };
            let result = native::send(Message {
                identifier: identifier(&delivery, "availability"),
                title: format!("{provider} quota has allowance"),
                body: availability_body(&candidate),
            })
            .await;
            match result {
                Err(error) => {
                    saved.availability_error = Some(error.to_string());
                    failed = true;
                }
                Ok(()) if !failed => saved.availability_error = None,
                Ok(()) => {}
            }
        }
    });
}

fn availability_body(candidate: &RecoveredAllowance) -> String {
    let label = |text: &str| {
        let mut bounded: String = text.chars().take(80).collect();
        if text.chars().count() > 80 {
            bounded.push_str("...");
        }
        bounded
    };
    let mut body = format!("{} reports remaining allowance.", label(&candidate.label));
    let exhausted = &candidate.other_exhausted;
    if let Some(first) = exhausted.first() {
        let names = match exhausted.len() {
            1 => label(&first.label),
            2 => format!("{} and {}", label(&first.label), label(&exhausted[1].label)),
            3 => format!(
                "{}, {} and 1 more window",
                label(&first.label),
                label(&exhausted[1].label),
            ),
            count => format!(
                "{}, {} and {} more windows",
                label(&first.label),
                label(&exhausted[1].label),
                count - 2
            ),
        };
        let verb = if exhausted.len() == 1 { "is" } else { "are" };
        body.push_str(&format!(" {names} {verb} still reported as exhausted."));
    }
    body
}

fn identifier(delivery: &Delivery, kind: &str) -> String {
    format!(
        "{}{kind}.{}.{}.{}",
        native::IDENTIFIER_PREFIX,
        std::process::id(),
        providers::now(),
        delivery.sequence.fetch_add(1, Ordering::Relaxed)
    )
}

fn remember_permission(
    app: &AppHandle,
    result: Result<PermissionState, native::Error>,
) -> Result<PermissionState, String> {
    app.state::<Runtime>()
        .update_notification_permission(result.as_ref().is_ok_and(PermissionState::can_deliver))?;
    result.map_err(|error| error.to_string())
}

fn should_request_permission(permission: &PermissionState) -> bool {
    permission.available && permission.authorization == Authorization::NotDetermined
}

fn permission_error(permission: &PermissionState) -> Option<String> {
    if !permission.available {
        return Some(
            permission
                .reason
                .clone()
                .unwrap_or_else(|| "Notifications are unavailable in this copy of Delta-V.".into()),
        );
    }
    if permission.can_deliver() {
        return None;
    }
    Some(match permission.authorization {
        Authorization::NotDetermined => "Notification permission has not been granted. Try enabling notifications again.",
        Authorization::Denied => "Notifications are blocked. Open System Settings > Notifications > Delta-V to allow them.",
        _ => "Alerts are unavailable. Open System Settings > Notifications > Delta-V and allow alerts.",
    }.into())
}

fn allowance_body(candidates: &[LowAllowance], threshold: u8) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let labels: Vec<String> = candidates
        .iter()
        .take(2)
        .map(|candidate| {
            let mut label: String = candidate.label.chars().take(80).collect();
            if candidate.label.chars().count() > 80 {
                label.push_str("...");
            }
            label
        })
        .collect();
    let windows = match candidates.len() {
        1 => labels[0].clone(),
        2 => format!("{} and {}", labels[0], labels[1]),
        3 => format!("{}, {} and 1 more window", labels[0], labels[1]),
        count => format!(
            "{}, {} and {} more windows",
            labels[0],
            labels[1],
            count - 2
        ),
    };
    let verb = if candidates.len() == 1 { "is" } else { "are" };
    Some(format!("{windows} {verb} below {threshold}% remaining."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_available_undecided_permission_can_prompt() {
        for authorization in [
            Authorization::NotDetermined,
            Authorization::Denied,
            Authorization::Authorized,
            Authorization::Provisional,
            Authorization::Unknown,
        ] {
            for available in [false, true] {
                let permission = PermissionState {
                    authorization,
                    available,
                    alerts_enabled: true,
                    reason: None,
                };
                assert_eq!(
                    should_request_permission(&permission),
                    available && authorization == Authorization::NotDetermined
                );
                assert_eq!(
                    permission_error(&permission).is_none(),
                    permission.can_deliver()
                );
            }
        }
        let no_alerts = PermissionState {
            authorization: Authorization::Authorized,
            available: true,
            alerts_enabled: false,
            reason: None,
        };
        assert!(!should_request_permission(&no_alerts));
        assert!(
            permission_error(&no_alerts)
                .unwrap()
                .contains("System Settings")
        );
    }

    fn candidate(label: &str) -> LowAllowance {
        LowAllowance {
            provider: ProviderId::Claude,
            limit_id: "session".into(),
            label: label.into(),
            remaining_percent: 19.99,
            resets_at: 2_000,
        }
    }

    #[test]
    fn availability_wording_identifies_the_quota_without_claiming_provider_readiness() {
        use crate::notifications::availability::ExhaustedQuota;
        let mut recovered = RecoveredAllowance {
            provider: ProviderId::Claude,
            limit_id: "session".into(),
            label: "5-hour".into(),
            window_seconds: Some(18_000),
            remaining_percent: 0.000_001,
            resets_at: 2000,
            observed_at: 1000,
            other_exhausted: Vec::new(),
        };
        assert_eq!(
            availability_body(&recovered),
            "5-hour reports remaining allowance."
        );
        for label in ["Weekly", "Model", "Extra"] {
            recovered.other_exhausted.push(ExhaustedQuota {
                limit_id: label.into(),
                label: label.into(),
                window_seconds: None,
            });
        }
        assert_eq!(
            availability_body(&recovered),
            "5-hour reports remaining allowance. Weekly, Model and 1 more window are still reported as exhausted."
        );
        recovered.other_exhausted.truncate(1);
        assert_eq!(
            availability_body(&recovered),
            "5-hour reports remaining allowance. Weekly is still reported as exhausted."
        );
        recovered.label = "界".repeat(100);
        assert!(availability_body(&recovered).starts_with(&format!(
            "{}... reports remaining allowance.",
            "界".repeat(80)
        )));
    }

    #[test]
    fn combines_crossings_without_rounding_up_to_the_threshold() {
        assert_eq!(allowance_body(&[], 20), None);
        assert_eq!(
            allowance_body(&[candidate("5-hour")], 20).unwrap(),
            "5-hour is below 20% remaining."
        );
        assert_eq!(
            allowance_body(&[candidate("5-hour"), candidate("Weekly")], 20).unwrap(),
            "5-hour and Weekly are below 20% remaining."
        );
        let body = allowance_body(
            &[
                candidate(&"界".repeat(100)),
                candidate("Weekly"),
                candidate("Model"),
            ],
            20,
        )
        .unwrap();
        assert!(body.starts_with(&format!("{}...", "界".repeat(80))));
        assert!(body.ends_with("and 1 more window are below 20% remaining."));
    }
}
