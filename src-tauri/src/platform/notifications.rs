use std::{
    path::Path,
    ptr::NonNull,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use block2::{DynBlock, RcBlock};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSetting, UNNotificationSettings,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use serde::Serialize;
use tauri::AppHandle;
use tauri_nspanel::{
    objc2::{
        AnyThread, DefinedClass, define_class, msg_send,
        rc::{Retained, autoreleasepool},
        runtime::{Bool, ProtocolObject},
    },
    objc2_app_kit::NSWorkspace,
    objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString, NSURL},
};
use tokio::sync::oneshot;

pub const IDENTIFIER_PREFIX: &str = "delta-v.";
const BUNDLE_ID: &str = "io.github.freonoma.delta-v";
const BUNDLE_REASON: &str = "Notifications are available when Delta-V runs from its app bundle. Open Delta-V.app and try again.";
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(60);
const SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.Notifications-Settings.extension";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    NotDetermined,
    Denied,
    Authorized,
    Provisional,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PermissionState {
    pub authorization: Authorization,
    pub alerts_enabled: bool,
    pub available: bool,
    pub reason: Option<String>,
}

impl PermissionState {
    pub fn can_deliver(&self) -> bool {
        self.available
            && self.alerts_enabled
            && matches!(
                self.authorization,
                Authorization::Authorized | Authorization::Provisional
            )
    }

    fn unavailable() -> Self {
        Self {
            authorization: Authorization::Unknown,
            alerts_enabled: false,
            available: false,
            reason: Some(BUNDLE_REASON.to_owned()),
        }
    }
}

pub struct Message {
    pub identifier: String,
    pub title: String,
    pub body: String,
}

impl Message {
    fn validate(&self) -> Result<(), Error> {
        if !owned_identifier(&self.identifier)
            || self.identifier.len() > 256
            || self.identifier.chars().any(char::is_control)
            || self.title.trim().is_empty()
            || self.title.len() > 256
            || self.title.chars().any(char::is_control)
            || self.body.trim().is_empty()
            || self.body.len() > 2048
            || self
                .body
                .chars()
                .any(|character| character.is_control() && character != '\n')
        {
            return Err(Error::InvalidMessage);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{BUNDLE_REASON}")]
    Unavailable,
    #[error("macOS did not respond to the notification request. Try again.")]
    NoResponse,
    #[error("Could not request notification permission. Try again.")]
    Authorization,
    #[error("Could not send the notification. Try again.")]
    Delivery,
    #[error("The notification could not be prepared.")]
    InvalidMessage,
    #[error(
        "Could not open notification settings. Open System Settings, choose Notifications, then Delta-V."
    )]
    Settings,
}

struct DelegateIvars {
    app: AppHandle,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "DeltaVNotificationDelegate"]
    #[ivars = DelegateIvars]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let identifier = notification.request().identifier().to_string();
            completion.call((presentation_options(&identifier),));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &DynBlock<dyn Fn()>,
        ) {
            let identifier = response.notification().request().identifier().to_string();
            let is_default =
                &*response.actionIdentifier() == unsafe { UNNotificationDefaultActionIdentifier };
            completion.call(());
            if owned_identifier(&identifier)
                && is_default
                && let Err(error) = crate::platform::show_usage_from_notification(&self.ivars().app)
            {
                eprintln!("Could not open usage from a notification: {error}");
            }
        }
    }
);

impl NotificationDelegate {
    fn new(app: AppHandle) -> Retained<Self> {
        let allocated = Self::alloc().set_ivars(DelegateIvars { app });
        // NSObject has no additional initialization requirements for this subclass.
        unsafe { msg_send![super(allocated), init] }
    }
}

// UNUserNotificationCenter holds its delegate weakly. The subclass has only
// thread-safe Rust ivars; it does not retain native UI objects across threads.
static DELEGATE: OnceLock<Retained<NotificationDelegate>> = OnceLock::new();

pub fn configure(app: &AppHandle) -> Result<(), Error> {
    autoreleasepool(|_| {
        let center = center()?;
        let delegate = DELEGATE.get_or_init(|| NotificationDelegate::new(app.clone()));
        center.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
        Ok(())
    })
}

/// Reads macOS settings without requesting authorization.
pub async fn state() -> Result<PermissionState, Error> {
    let receiver = autoreleasepool(|_| {
        let Ok(center) = center() else {
            return None;
        };
        let (sender, receiver) = oneshot::channel();
        let sender = Mutex::new(Some(sender));
        let callback = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // The framework owns this nonnull settings object for the callback.
            let settings = unsafe { settings.as_ref() };
            complete(
                &sender,
                permission_state(settings.authorizationStatus(), settings.alertSetting()),
            );
        });
        center.getNotificationSettingsWithCompletionHandler(&callback);
        Some(receiver)
    });
    match receiver {
        Some(receiver) => wait(receiver, RESPONSE_TIMEOUT).await,
        None => Ok(PermissionState::unavailable()),
    }
}

/// Only an explicit user action may call this function.
pub async fn request_authorization() -> Result<PermissionState, Error> {
    let receiver = autoreleasepool(|_| {
        let center = center()?;
        let (sender, receiver) = oneshot::channel();
        let sender = Mutex::new(Some(sender));
        let callback = RcBlock::new(move |_granted: Bool, error: *mut NSError| {
            complete(
                &sender,
                if error.is_null() {
                    Ok(())
                } else {
                    Err(Error::Authorization)
                },
            );
        });
        center
            .requestAuthorizationWithOptions_completionHandler(authorization_options(), &callback);
        Ok(receiver)
    })?;
    wait(receiver, AUTHORIZATION_TIMEOUT).await??;
    state().await
}

/// The caller checks permission and its current delivery policy immediately
/// before calling. Enqueue happens before the first await. Success means macOS
/// accepted the request, not that a banner was displayed.
pub async fn send(message: Message) -> Result<(), Error> {
    message.validate()?;
    let receiver = autoreleasepool(|_| {
        let center = center()?;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&message.title));
        content.setBody(&NSString::from_str(&message.body));
        // New content has no sound, badge, attachments or account metadata.
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&message.identifier),
            &content,
            None,
        );
        let (sender, receiver) = oneshot::channel();
        let sender = Mutex::new(Some(sender));
        let callback = RcBlock::new(move |error: *mut NSError| {
            complete(
                &sender,
                if error.is_null() {
                    Ok(())
                } else {
                    Err(Error::Delivery)
                },
            );
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&callback));
        Ok(receiver)
    })?;
    wait(receiver, RESPONSE_TIMEOUT).await?
}

pub fn open_settings() -> Result<(), Error> {
    autoreleasepool(|_| {
        // macOS has no public per-app notification-settings URL. This fixed
        // system pane link is best effort; the error supplies the manual route.
        let url = NSURL::URLWithString(&NSString::from_str(SETTINGS_URL)).ok_or(Error::Settings)?;
        if NSWorkspace::sharedWorkspace().openURL(&url) {
            Ok(())
        } else {
            Err(Error::Settings)
        }
    })
}

fn center() -> Result<Retained<UNUserNotificationCenter>, Error> {
    // Check before accessing the singleton: raw development executables have
    // no application identity with which macOS can associate notifications.
    if !running_in_bundle() {
        return Err(Error::Unavailable);
    }
    Ok(UNUserNotificationCenter::currentNotificationCenter())
}

fn running_in_bundle() -> bool {
    let bundle = NSBundle::mainBundle();
    let Some(executable) = bundle.executablePath() else {
        return false;
    };
    let (Ok(bundle_path), Ok(bundle_executable), Ok(running_executable)) = (
        Path::new(&bundle.bundlePath().to_string()).canonicalize(),
        Path::new(&executable.to_string()).canonicalize(),
        std::env::current_exe().and_then(|path| path.canonicalize()),
    ) else {
        return false;
    };
    let identifier = bundle.bundleIdentifier().map(|value| value.to_string());
    eligible_bundle(
        &bundle_path,
        identifier.as_deref(),
        &bundle_executable,
        &running_executable,
    )
}

fn eligible_bundle(
    bundle: &Path,
    identifier: Option<&str>,
    bundle_executable: &Path,
    running_executable: &Path,
) -> bool {
    identifier == Some(BUNDLE_ID)
        && bundle
            .extension()
            .is_some_and(|extension| extension == "app")
        && bundle_executable == bundle.join("Contents/MacOS/delta-v")
        && running_executable == bundle_executable
}

fn permission_state(
    status: UNAuthorizationStatus,
    alerts: UNNotificationSetting,
) -> PermissionState {
    let authorization = match status {
        UNAuthorizationStatus::NotDetermined => Authorization::NotDetermined,
        UNAuthorizationStatus::Denied => Authorization::Denied,
        UNAuthorizationStatus::Authorized => Authorization::Authorized,
        UNAuthorizationStatus::Provisional => Authorization::Provisional,
        _ => Authorization::Unknown,
    };
    PermissionState {
        authorization,
        alerts_enabled: alerts == UNNotificationSetting::Enabled,
        available: true,
        reason: (authorization == Authorization::Unknown).then(|| {
            "macOS returned an unrecognized notification permission. Try again.".to_owned()
        }),
    }
}

fn owned_identifier(identifier: &str) -> bool {
    identifier
        .strip_prefix(IDENTIFIER_PREFIX)
        .is_some_and(|suffix| !suffix.trim().is_empty())
}

fn authorization_options() -> UNAuthorizationOptions {
    UNAuthorizationOptions::Alert
}

fn presentation_options(identifier: &str) -> UNNotificationPresentationOptions {
    if owned_identifier(identifier) {
        UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List
    } else {
        UNNotificationPresentationOptions::empty()
    }
}

fn complete<T>(sender: &Mutex<Option<oneshot::Sender<T>>>, value: T) {
    if let Ok(mut sender) = sender.lock()
        && let Some(sender) = sender.take()
    {
        let _ = sender.send(value);
    }
}

async fn wait<T>(receiver: oneshot::Receiver<T>, timeout: Duration) -> Result<T, Error> {
    tokio::time::timeout(timeout, receiver)
        .await
        .map_err(|_| Error::NoResponse)?
        .map_err(|_| Error::NoResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_identity_requires_the_real_application_and_its_running_executable() {
        let bundle = Path::new("/Applications/Delta-V.app");
        let executable = bundle.join("Contents/MacOS/delta-v");
        assert!(eligible_bundle(
            bundle,
            Some(BUNDLE_ID),
            &executable,
            &executable
        ));
        assert!(!eligible_bundle(bundle, None, &executable, &executable));
        assert!(!eligible_bundle(
            bundle,
            Some("other.app"),
            &executable,
            &executable
        ));
        assert!(!eligible_bundle(
            bundle,
            Some(BUNDLE_ID),
            &executable,
            Path::new("/tmp/delta-v")
        ));
        let raw = Path::new("/tmp/target/debug");
        let raw_executable = raw.join("delta-v");
        assert!(!eligible_bundle(
            raw,
            Some(BUNDLE_ID),
            &raw_executable,
            &raw_executable
        ));
        let misleading = bundle.join("delta-v");
        assert!(!eligible_bundle(
            bundle,
            Some(BUNDLE_ID),
            &misleading,
            &misleading
        ));
    }

    #[test]
    fn a_source_built_app_bundle_is_eligible_outside_applications() {
        let bundle = Path::new("/tmp/build/Delta-V.app");
        let executable = bundle.join("Contents/MacOS/delta-v");
        assert!(eligible_bundle(
            bundle,
            Some(BUNDLE_ID),
            &executable,
            &executable
        ));
    }

    #[test]
    fn permission_mapping_requires_authorization_and_visible_alerts() {
        for (native, expected, authorized) in [
            (
                UNAuthorizationStatus::NotDetermined,
                Authorization::NotDetermined,
                false,
            ),
            (UNAuthorizationStatus::Denied, Authorization::Denied, false),
            (
                UNAuthorizationStatus::Authorized,
                Authorization::Authorized,
                true,
            ),
            (
                UNAuthorizationStatus::Provisional,
                Authorization::Provisional,
                true,
            ),
            (
                UNAuthorizationStatus::Ephemeral,
                Authorization::Unknown,
                false,
            ),
            (UNAuthorizationStatus(99), Authorization::Unknown, false),
        ] {
            for alerts in [
                UNNotificationSetting::NotSupported,
                UNNotificationSetting::Disabled,
                UNNotificationSetting::Enabled,
                UNNotificationSetting(99),
            ] {
                let state = permission_state(native, alerts);
                assert_eq!(state.authorization, expected);
                assert!(state.available);
                assert_eq!(
                    state.can_deliver(),
                    authorized && alerts == UNNotificationSetting::Enabled
                );
            }
        }
        assert!(!PermissionState::unavailable().can_deliver());
    }

    #[test]
    fn public_authorization_values_use_snake_case() {
        assert_eq!(
            serde_json::to_string(&Authorization::NotDetermined).unwrap(),
            "\"not_determined\""
        );
        assert_eq!(
            serde_json::to_string(&Authorization::Provisional).unwrap(),
            "\"provisional\""
        );
    }

    #[test]
    fn requests_and_foreground_presentation_never_include_sound_or_badges() {
        assert_eq!(authorization_options(), UNAuthorizationOptions::Alert);
        assert_eq!(
            presentation_options("delta-v.test"),
            UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List
        );
        assert!(presentation_options("unrelated.test").is_empty());
        assert!(presentation_options("delta-v.").is_empty());
    }

    fn message() -> Message {
        Message {
            identifier: "delta-v.test.1".to_owned(),
            title: "Low allowance".to_owned(),
            body: "Claude 5-hour: 15% remaining.\nOpen Delta-V to see usage.".to_owned(),
        }
    }

    #[test]
    fn message_content_requires_an_owned_identifier_and_bounded_plain_text() {
        assert!(message().validate().is_ok());
        for identifier in ["foreign.test", "delta-v.", "delta-v.\n"] {
            let mut invalid = message();
            invalid.identifier = identifier.to_owned();
            assert!(invalid.validate().is_err());
        }
        for title in [String::new(), "line\nbreak".to_owned(), "x".repeat(257)] {
            let mut invalid = message();
            invalid.title = title;
            assert!(invalid.validate().is_err());
        }
        for body in [" ".to_owned(), "hidden\0text".to_owned(), "x".repeat(2049)] {
            let mut invalid = message();
            invalid.body = body;
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn a_late_or_repeated_callback_safely_releases_its_owned_result() {
        let (sender, receiver) = oneshot::channel();
        let sender = Mutex::new(Some(sender));
        drop(receiver);
        complete(&sender, String::from("late"));
        complete(&sender, String::from("repeated"));
        assert!(sender.lock().unwrap().is_none());
    }
}
