use serde::Serialize;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginItemStatus {
    Disabled,
    Enabled,
    ApprovalRequired,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
pub struct LoginItemState {
    pub status: LoginItemStatus,
    pub reason: Option<String>,
}

impl LoginItemState {
    fn unavailable(reason: &str) -> Self {
        Self {
            status: LoginItemStatus::Unavailable,
            reason: Some(reason.to_owned()),
        }
    }

    fn require_available(&self) -> Result<(), LoginItemError> {
        if self.status == LoginItemStatus::Unavailable {
            return Err(LoginItemError::Unavailable(
                self.reason.clone().unwrap_or_else(|| {
                    "Launch at login is unavailable. Quit and reopen Delta-V, then try again."
                        .to_owned()
                }),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum LoginItemError {
    #[error("Could not check launch at login. Quit and reopen Delta-V, then try again.")]
    StateUnavailable,
    #[error("{0}")]
    Unavailable(String),
    #[error("Could not enable launch at login: {0}")]
    Register(String),
    #[error("Could not disable launch at login: {0}")]
    Unregister(String),
}

#[cfg(target_os = "macos")]
pub use macos::{open_settings, set_enabled, state};

#[cfg(not(target_os = "macos"))]
pub fn state() -> Result<LoginItemState, LoginItemError> {
    Ok(LoginItemState::unavailable(
        "Launch at login is only available on macOS.",
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn set_enabled(_: bool) -> Result<LoginItemState, LoginItemError> {
    let state = state()?;
    state.require_available()?;
    Ok(state)
}

#[cfg(not(target_os = "macos"))]
pub fn open_settings() -> Result<(), LoginItemError> {
    Err(LoginItemError::StateUnavailable)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };

    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    use tauri_nspanel::{objc2::rc::autoreleasepool, objc2_foundation::NSBundle};

    use super::{LoginItemError, LoginItemState, LoginItemStatus};

    const BUNDLE_ID: &str = "io.github.freonoma.delta-v";
    const INSTALL_REASON: &str =
        "Move Delta-V to Applications and open it from there to use launch at login.";
    static SERVICE_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Debug, PartialEq, Eq)]
    enum Change {
        None,
        Register,
        Unregister,
    }

    fn change_for(status: LoginItemStatus, enabled: bool) -> Change {
        match (status, enabled) {
            (LoginItemStatus::Disabled, true) => Change::Register,
            (LoginItemStatus::Enabled | LoginItemStatus::ApprovalRequired, false) => {
                Change::Unregister
            }
            _ => Change::None,
        }
    }

    fn confirm_change(
        after: LoginItemState,
        enabled: bool,
        error: Option<String>,
    ) -> Result<LoginItemState, LoginItemError> {
        after.require_available()?;
        let satisfied = if enabled {
            matches!(
                after.status,
                LoginItemStatus::Enabled | LoginItemStatus::ApprovalRequired
            )
        } else {
            after.status == LoginItemStatus::Disabled
        };
        if satisfied {
            return Ok(after);
        }
        let message = error.unwrap_or_else(|| {
            "macOS has not confirmed the change. Check Login Items in System Settings and try again."
                .to_owned()
        });
        Err(if enabled {
            LoginItemError::Register(message)
        } else {
            LoginItemError::Unregister(message)
        })
    }

    fn eligible_bundle(
        bundle: &Path,
        identifier: Option<&str>,
        bundle_executable: &Path,
        running_executable: &Path,
        application_folders: &[PathBuf],
    ) -> bool {
        identifier == Some(BUNDLE_ID)
            && bundle
                .extension()
                .is_some_and(|extension| extension == "app")
            && application_folders
                .iter()
                .any(|folder| bundle.parent() == Some(folder.as_path()))
            && bundle_executable == bundle.join("Contents/MacOS/delta-v")
            && running_executable == bundle_executable
    }

    fn installed_bundle() -> bool {
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
        let mut folders = vec![PathBuf::from("/Applications")];
        if let Some(home) = std::env::var_os("HOME") {
            folders.push(PathBuf::from(home).join("Applications"));
        }
        let folders: Vec<_> = folders
            .into_iter()
            .filter_map(|path| path.canonicalize().ok())
            .collect();
        let identifier = bundle.bundleIdentifier().map(|value| value.to_string());
        eligible_bundle(
            &bundle_path,
            identifier.as_deref(),
            &bundle_executable,
            &running_executable,
            &folders,
        )
    }

    fn main_app_state(status: SMAppServiceStatus) -> LoginItemState {
        let status = match status {
            // A validated main app can have no login-item record before its first registration.
            SMAppServiceStatus::NotRegistered | SMAppServiceStatus::NotFound => {
                LoginItemStatus::Disabled
            }
            SMAppServiceStatus::Enabled => LoginItemStatus::Enabled,
            SMAppServiceStatus::RequiresApproval => LoginItemStatus::ApprovalRequired,
            _ => {
                return LoginItemState::unavailable(
                    "macOS returned an unrecognized login-item status. Quit and reopen Delta-V, then try again.",
                );
            }
        };
        LoginItemState {
            status,
            reason: None,
        }
    }

    fn service_state(service: &SMAppService) -> LoginItemState {
        // The app's minimum macOS version is newer than this API's introduction.
        main_app_state(unsafe { service.status() })
    }

    pub fn state() -> Result<LoginItemState, LoginItemError> {
        let _guard = SERVICE_LOCK
            .lock()
            .map_err(|_| LoginItemError::StateUnavailable)?;
        autoreleasepool(|_| {
            if !installed_bundle() {
                return Ok(LoginItemState::unavailable(INSTALL_REASON));
            }
            let service = unsafe { SMAppService::mainAppService() };
            Ok(service_state(&service))
        })
    }

    pub fn set_enabled(enabled: bool) -> Result<LoginItemState, LoginItemError> {
        let _guard = SERVICE_LOCK
            .lock()
            .map_err(|_| LoginItemError::StateUnavailable)?;
        autoreleasepool(|_| {
            if !installed_bundle() {
                return Err(LoginItemError::Unavailable(INSTALL_REASON.to_owned()));
            }
            let service = unsafe { SMAppService::mainAppService() };
            let before = service_state(&service);
            before.require_available()?;
            let change = change_for(before.status, enabled);
            let result = match change {
                Change::None => return Ok(before),
                Change::Register => unsafe { service.registerAndReturnError() },
                Change::Unregister => unsafe { service.unregisterAndReturnError() },
            };
            let after = service_state(&service);
            // System Settings can change the service between our read and mutation.
            confirm_change(
                after,
                enabled,
                result
                    .err()
                    .map(|error| error.localizedDescription().to_string()),
            )
        })
    }

    pub fn open_settings() -> Result<(), LoginItemError> {
        autoreleasepool(|_| unsafe { SMAppService::openSystemSettingsLoginItems() });
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn is_eligible(bundle: &str, identifier: Option<&str>, executable: &str) -> bool {
            eligible_bundle(
                Path::new(bundle),
                identifier,
                &Path::new(bundle).join("Contents/MacOS/delta-v"),
                Path::new(executable),
                &[
                    PathBuf::from("/Applications"),
                    PathBuf::from("/Users/test/Applications"),
                ],
            )
        }

        #[test]
        fn accepts_installed_bundle_in_either_applications_folder() {
            for bundle in [
                "/Applications/Delta-V.app",
                "/Users/test/Applications/Delta-V.app",
            ] {
                assert!(is_eligible(
                    bundle,
                    Some(BUNDLE_ID),
                    &format!("{bundle}/Contents/MacOS/delta-v"),
                ));
            }
        }

        #[test]
        fn rejects_temporary_and_development_bundles() {
            for bundle in [
                "/tmp/Delta-V.app",
                "/Users/test/Downloads/Delta-V.app",
                "/Applications/dev/target/release/bundle/macos/Delta-V.app",
                "/Applications/Delta-V.app.backup",
                "/Applications/Outer.app/Contents/Delta-V.app",
            ] {
                assert!(!is_eligible(
                    bundle,
                    Some(BUNDLE_ID),
                    &format!("{bundle}/Contents/MacOS/delta-v"),
                ));
            }
        }

        #[test]
        fn rejects_wrong_bundle_identity_or_executable() {
            let bundle = "/Applications/Delta-V.app";
            let executable = "/Applications/Delta-V.app/Contents/MacOS/delta-v";
            assert!(!is_eligible(bundle, None, executable));
            assert!(!is_eligible(bundle, Some("com.example.other"), executable));
            assert!(!is_eligible(
                bundle,
                Some(BUNDLE_ID),
                "/tmp/target/debug/delta-v"
            ));
            assert!(!eligible_bundle(
                Path::new(bundle),
                Some(BUNDLE_ID),
                Path::new("/tmp/delta-v"),
                Path::new("/tmp/delta-v"),
                &[PathBuf::from("/Applications")],
            ));
        }

        #[test]
        fn enabling_preserves_os_approval_and_existing_registration() {
            assert_eq!(
                change_for(LoginItemStatus::Disabled, true),
                Change::Register
            );
            for status in [
                LoginItemStatus::Enabled,
                LoginItemStatus::ApprovalRequired,
                LoginItemStatus::Unavailable,
            ] {
                assert_eq!(change_for(status, true), Change::None);
            }
        }

        #[test]
        fn missing_main_app_record_allows_registration_but_does_not_confirm_it() {
            let state = main_app_state(SMAppServiceStatus::NotFound);
            assert_eq!(state.status, LoginItemStatus::Disabled);
            assert!(state.require_available().is_ok());
            assert_eq!(change_for(state.status, true), Change::Register);
            assert_eq!(change_for(state.status, false), Change::None);
            assert!(matches!(
                confirm_change(state, true, Some("Registration failed.".into())),
                Err(LoginItemError::Register(message)) if message == "Registration failed."
            ));
        }

        #[test]
        fn native_status_mapping_preserves_approval_and_rejects_unknown_values() {
            for (raw, expected) in [
                (SMAppServiceStatus::NotRegistered, LoginItemStatus::Disabled),
                (SMAppServiceStatus::Enabled, LoginItemStatus::Enabled),
                (
                    SMAppServiceStatus::RequiresApproval,
                    LoginItemStatus::ApprovalRequired,
                ),
                (SMAppServiceStatus(99), LoginItemStatus::Unavailable),
            ] {
                assert_eq!(main_app_state(raw).status, expected);
            }
        }

        #[test]
        fn disabling_only_unregisters_an_existing_service() {
            for status in [LoginItemStatus::Enabled, LoginItemStatus::ApprovalRequired] {
                assert_eq!(change_for(status, false), Change::Unregister);
            }
            for status in [LoginItemStatus::Disabled, LoginItemStatus::Unavailable] {
                assert_eq!(change_for(status, false), Change::None);
            }
        }

        #[test]
        fn unavailable_state_cannot_complete_a_change() {
            let state = LoginItemState::unavailable(INSTALL_REASON);
            assert_eq!(
                state.require_available().unwrap_err().to_string(),
                INSTALL_REASON
            );
            for enabled in [true, false] {
                assert!(matches!(
                    confirm_change(state.clone(), enabled, None),
                    Err(LoginItemError::Unavailable(_))
                ));
            }
        }

        #[test]
        fn successful_native_call_still_requires_the_requested_state() {
            for (status, enabled) in [
                (LoginItemStatus::Disabled, true),
                (LoginItemStatus::Enabled, false),
                (LoginItemStatus::ApprovalRequired, false),
            ] {
                assert!(
                    confirm_change(
                        LoginItemState {
                            status,
                            reason: None
                        },
                        enabled,
                        None
                    )
                    .is_err()
                );
            }
        }

        #[test]
        fn concurrent_system_change_can_satisfy_a_failed_native_call() {
            for (status, enabled) in [
                (LoginItemStatus::Enabled, true),
                (LoginItemStatus::ApprovalRequired, true),
                (LoginItemStatus::Disabled, false),
            ] {
                assert!(
                    confirm_change(
                        LoginItemState {
                            status,
                            reason: None
                        },
                        enabled,
                        Some("The registration changed in System Settings.".to_owned()),
                    )
                    .is_ok()
                );
            }
        }
    }
}
