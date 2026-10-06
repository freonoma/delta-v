mod github;

use std::{cmp::Ordering, future::Future, sync::Mutex};

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateError {
    #[error("Could not reach GitHub. Check your connection and try again.")]
    Network,
    #[error("GitHub returned release information Delta-V could not read. Please try again later.")]
    InvalidResponse,
    #[error("GitHub could not complete the check. Please try again later.")]
    Service { status: u16, retry_at: Option<i64> },
    #[error("GitHub's request limit was reached. Please try again later.")]
    RateLimited { retry_at: i64 },
    #[error("The update check was interrupted. Please try again.")]
    Interrupted,
    #[error("Could not read update check status. Restart Delta-V and try again.")]
    StateUnavailable,
}

impl UpdateError {
    fn retry_at(&self) -> Option<i64> {
        match self {
            Self::RateLimited { retry_at } => Some(*retry_at),
            Self::Service { retry_at, .. } => *retry_at,
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    Available,
    Current,
    Ahead,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckedRelease {
    installed_version: String,
    latest_version: String,
    status: ReleaseStatus,
    checked_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UpdateState {
    checking: bool,
    last_success: Option<CheckedRelease>,
    error: Option<UpdateError>,
    next_check_at: Option<i64>,
}

/// App-lifetime state only. Constructing or reading it never makes a request.
#[derive(Default)]
pub struct UpdateChecker {
    state: Mutex<UpdateState>,
}

impl UpdateChecker {
    pub fn state(&self) -> Result<UpdateState, UpdateError> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| UpdateError::StateUnavailable)
    }

    pub async fn check(&self, package: &tauri::PackageInfo) -> Result<UpdateState, UpdateError> {
        self.check_with(package, github::latest, || {
            time::OffsetDateTime::now_utc().unix_timestamp()
        })
        .await
    }

    async fn check_with<F, Fut, Clock>(
        &self,
        package: &tauri::PackageInfo,
        fetch: F,
        now: Clock,
    ) -> Result<UpdateState, UpdateError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = github::FetchResult>,
        Clock: Fn() -> i64,
    {
        let pending = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| UpdateError::StateUnavailable)?;
            if state.checking || state.next_check_at.is_some_and(|deadline| deadline > now()) {
                return Ok(state.clone());
            }
            state.checking = true;
            state.error = None;
            state.next_check_at = None;
            PendingCheck {
                state: &self.state,
                active: true,
            }
        };

        // No lock is held while waiting for the network. Keep the last success
        // visible, with its original timestamp, until another check succeeds.
        let response = fetch().await;
        let result = response
            .result
            .and_then(|release| compare_release(package, &release.tag_name, now()));
        pending.finish(result, response.retry_at)
    }
}

struct PendingCheck<'a> {
    state: &'a Mutex<UpdateState>,
    active: bool,
}

impl PendingCheck<'_> {
    fn finish(
        mut self,
        result: Result<CheckedRelease, UpdateError>,
        retry_at: Option<i64>,
    ) -> Result<UpdateState, UpdateError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| UpdateError::StateUnavailable)?;
        state.checking = false;
        state.next_check_at = retry_at;
        match result {
            Ok(checked) => {
                state.last_success = Some(checked);
                state.error = None;
            }
            Err(error) => {
                state.next_check_at = retry_at.max(error.retry_at());
                state.error = Some(error);
            }
        }
        // Disarm before releasing the lock so Drop cannot clear a newer check.
        self.active = false;
        Ok(state.clone())
    }
}

impl Drop for PendingCheck<'_> {
    fn drop(&mut self) {
        if self.active
            && let Ok(mut state) = self.state.lock()
        {
            state.checking = false;
            state.error = Some(UpdateError::Interrupted);
        }
    }
}

fn compare_release(
    package: &tauri::PackageInfo,
    tag: &str,
    checked_at: i64,
) -> Result<CheckedRelease, UpdateError> {
    let latest = tag
        .strip_prefix('v')
        .unwrap_or(tag)
        .parse()
        .map_err(|_| UpdateError::InvalidResponse)?;
    // Tauri's package version already uses SemVer. Compare precedence so build
    // metadata does not create a false update, and 0.10.0 sorts after 0.9.0.
    let status = match package.version.cmp_precedence(&latest) {
        Ordering::Less => ReleaseStatus::Available,
        Ordering::Equal => ReleaseStatus::Current,
        Ordering::Greater => ReleaseStatus::Ahead,
    };
    if !latest.pre.is_empty() {
        return Err(UpdateError::InvalidResponse);
    }
    Ok(CheckedRelease {
        installed_version: package.version.to_string(),
        latest_version: latest.to_string(),
        status,
        checked_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(version: &str) -> tauri::PackageInfo {
        tauri::PackageInfo {
            name: "Delta-V".to_owned(),
            version: version.parse().unwrap(),
            authors: "",
            description: "",
            crate_name: "delta-v",
        }
    }

    fn release(tag: &str) -> github::FetchResult {
        github::FetchResult {
            result: Ok(github::Release {
                tag_name: tag.to_owned(),
            }),
            retry_at: None,
        }
    }

    fn failed(error: UpdateError) -> github::FetchResult {
        github::FetchResult {
            result: Err(error),
            retry_at: None,
        }
    }

    #[test]
    fn compares_semver_precedence_instead_of_text_or_build_metadata() {
        for (installed, tag, status) in [
            ("0.9.0", "v0.10.0", ReleaseStatus::Available),
            ("0.3.0", "0.3.0", ReleaseStatus::Current),
            ("0.4.0", "v0.3.0", ReleaseStatus::Ahead),
            ("0.4.0-beta.1", "v0.4.0", ReleaseStatus::Available),
            ("0.4.0-beta.1", "v0.3.0", ReleaseStatus::Ahead),
            ("0.3.0+local", "v0.3.0+release", ReleaseStatus::Current),
        ] {
            let checked = compare_release(&package(installed), tag, 123).unwrap();
            assert_eq!(checked.status, status, "{installed} compared to {tag}");
            assert_eq!(checked.installed_version, installed);
            assert_eq!(checked.latest_version, tag.trim_start_matches('v'));
            assert_eq!(checked.checked_at, 123);
        }
    }

    #[test]
    fn rejects_incomplete_malformed_or_prerelease_tags() {
        for tag in [
            "",
            "v0.3",
            "01.3.0",
            "release-0.3.0",
            "vv0.3.0",
            "v0.4.0-rc.1",
            " v0.3.0",
        ] {
            assert_eq!(
                compare_release(&package("0.3.0"), tag, 123),
                Err(UpdateError::InvalidResponse),
                "{tag}"
            );
        }
    }

    #[tokio::test]
    async fn starts_unchecked_and_keeps_results_only_for_this_instance() {
        let checker = UpdateChecker::default();
        assert_eq!(checker.state().unwrap(), UpdateState::default());
        let state = checker
            .check_with(&package("0.3.0"), || async { release("v0.4.0") }, || 100)
            .await
            .unwrap();
        assert_eq!(state.last_success.unwrap().status, ReleaseStatus::Available);
        assert_eq!(
            UpdateChecker::default().state().unwrap(),
            UpdateState::default()
        );
    }

    #[tokio::test]
    async fn failed_checks_keep_the_previous_result_and_timestamp() {
        let checker = UpdateChecker::default();
        let package = package("0.3.0");
        let success = checker
            .check_with(&package, || async { release("v0.3.0") }, || 100)
            .await
            .unwrap();
        for failure in [
            UpdateError::Network,
            UpdateError::InvalidResponse,
            UpdateError::Service {
                status: 503,
                retry_at: None,
            },
        ] {
            let state = checker
                .check_with(&package, || async { failed(failure.clone()) }, || 200)
                .await
                .unwrap();
            assert_eq!(state.last_success, success.last_success);
            assert_eq!(state.error, Some(failure));
            assert!(!state.checking);
        }
        let recovered = checker
            .check_with(&package, || async { release("v0.4.0") }, || 300)
            .await
            .unwrap();
        assert_eq!(recovered.last_success.unwrap().checked_at, 300);
        assert_eq!(recovered.error, None);
    }

    #[tokio::test]
    async fn suppresses_overlapping_checks_without_losing_the_result() {
        let checker = UpdateChecker::default();
        let package = package("0.3.0");
        let (send, receive) = tokio::sync::oneshot::channel();
        let check = checker.check_with(&package, || async { receive.await.unwrap() }, || 100);
        tokio::pin!(check);
        tokio::select! {
            _ = &mut check => panic!("check must wait for the response"),
            _ = tokio::task::yield_now() => {}
        }
        let duplicate = checker
            .check_with(&package, || async { panic!("duplicate request") }, || 100)
            .await
            .unwrap();
        assert!(duplicate.checking);
        send.send(release("v0.4.0")).unwrap();
        assert_eq!(
            check.await.unwrap().last_success.unwrap().status,
            ReleaseStatus::Available
        );
    }

    #[tokio::test]
    async fn server_cooldowns_block_requests_until_the_deadline() {
        for response in [
            failed(UpdateError::RateLimited { retry_at: 200 }),
            failed(UpdateError::Service {
                status: 503,
                retry_at: Some(200),
            }),
            github::FetchResult {
                retry_at: Some(200),
                ..release("v0.3.0")
            },
            github::FetchResult {
                retry_at: Some(200),
                ..release("v0.3")
            },
            github::FetchResult {
                retry_at: Some(200),
                ..failed(UpdateError::InvalidResponse)
            },
            github::FetchResult {
                retry_at: Some(200),
                ..failed(UpdateError::Network)
            },
        ] {
            let checker = UpdateChecker::default();
            let package = package("0.3.0");
            let limited = checker
                .check_with(&package, || async { response }, || 100)
                .await
                .unwrap();
            let blocked = checker
                .check_with(
                    &package,
                    || async { panic!("request during cooldown") },
                    || 199,
                )
                .await
                .unwrap();
            assert_eq!(blocked, limited);
            let retried = checker
                .check_with(&package, || async { release("v0.4.0") }, || 200)
                .await
                .unwrap();
            assert_eq!(retried.error, None);
            assert_eq!(retried.next_check_at, None);
            assert_eq!(retried.last_success.unwrap().checked_at, 200);
        }
    }

    #[tokio::test]
    async fn cancellation_allows_a_later_check_and_retains_last_success() {
        let checker = UpdateChecker::default();
        let package = package("0.3.0");
        let initial = checker
            .check_with(&package, || async { release("v0.3.0") }, || 100)
            .await
            .unwrap();
        let mut check = Box::pin(checker.check_with(&package, std::future::pending, || 200));
        tokio::select! {
            _ = &mut check => panic!("check must wait for the response"),
            _ = tokio::task::yield_now() => {}
        }
        assert!(checker.state().unwrap().checking);
        drop(check);
        let interrupted = checker.state().unwrap();
        assert!(!interrupted.checking);
        assert_eq!(interrupted.error, Some(UpdateError::Interrupted));
        assert_eq!(interrupted.last_success, initial.last_success);
        let retried = checker
            .check_with(&package, || async { release("v0.4.0") }, || 300)
            .await
            .unwrap();
        assert_eq!(retried.error, None);
    }

    #[test]
    fn serializes_errors_and_empty_state_for_the_frontend() {
        assert_eq!(
            serde_json::to_value(UpdateState::default()).unwrap(),
            serde_json::json!({
                "checking": false, "last_success": null, "error": null, "next_check_at": null
            })
        );
        assert_eq!(
            serde_json::to_value(UpdateError::RateLimited { retry_at: 200 }).unwrap(),
            serde_json::json!({
                "kind": "rate_limited", "retry_at": 200
            })
        );
    }
}
