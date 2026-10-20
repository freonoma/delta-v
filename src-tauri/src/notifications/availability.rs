//! Explicit one-time quota watches, independent of storage, polling and delivery.
//! Supply only accepted successful responses with separately verified account identity.
//! Runtime connection/recovery generation checks must reject obsolete responses first.

use serde::{Deserialize, Serialize};

use super::{ambiguous_ids, eligible_reading, stable_limit_id, valid_account_key, valid_text};
use crate::model::{ProviderId, ProviderSnapshot};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchState {
    Waiting,
    Completed,
    Cancelled,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ArmError {
    #[error("The current time is invalid.")]
    InvalidClock,
    #[error("A fresh usage reading is needed to start this watch.")]
    StaleReading,
    #[error("A verified account is needed to start this watch.")]
    UnverifiedAccount,
    #[error("This quota does not have a usable reading.")]
    UnavailableQuota,
    #[error("This quota is not reported as exhausted.")]
    NotExhausted,
}

#[derive(Debug, PartialEq)]
pub struct Watch {
    provider: ProviderId,
    account_key: String,
    limit_id: String,
    window_seconds: Option<u64>,
    armed_at: i64,
    last_observed_at: i64,
    state: WatchState,
}

/// Only pending state is persisted. A saved reset time cannot establish recovery.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingWatch {
    pub provider: ProviderId,
    pub account_key: String,
    pub limit_id: String,
    pub window_seconds: Option<u64>,
    pub armed_at: i64,
    pub last_observed_at: i64,
}

#[derive(Debug, PartialEq)]
pub struct ExhaustedQuota {
    pub limit_id: String,
    pub label: String,
    pub window_seconds: Option<u64>,
}

/// A measured recovery of this quota, not a provider-wide availability or delivery claim.
#[derive(Debug, PartialEq)]
pub struct RecoveredAllowance {
    pub provider: ProviderId,
    pub limit_id: String,
    pub label: String,
    pub window_seconds: Option<u64>,
    pub remaining_percent: f64,
    pub resets_at: i64,
    pub observed_at: i64,
    // Only confirmed exhausted siblings are listed; omitted windows may be unavailable.
    pub other_exhausted: Vec<ExhaustedQuota>,
}

impl Watch {
    pub fn arm(
        snapshot: &ProviderSnapshot,
        account_key: Option<&str>,
        limit_id: &str,
        now: i64,
        max_age_seconds: u64,
    ) -> Result<Self, ArmError> {
        if !valid_timestamp(now) {
            return Err(ArmError::InvalidClock);
        }
        if !fresh(snapshot, now, max_age_seconds) {
            return Err(ArmError::StaleReading);
        }
        let account_key = account_key
            .filter(|key| valid_account_key(key))
            .ok_or(ArmError::UnverifiedAccount)?;
        if ambiguous_ids(&snapshot.limits).contains(limit_id) {
            return Err(ArmError::UnavailableQuota);
        }
        let limit = snapshot
            .limits
            .iter()
            .find(|limit| limit.id == limit_id)
            .filter(|limit| eligible_reading(limit, now).is_some())
            .ok_or(ArmError::UnavailableQuota)?;
        // A rounded display percentage does not establish exhaustion.
        if limit.used_fraction != Some(1.0) {
            return Err(ArmError::NotExhausted);
        }
        Ok(Self {
            provider: snapshot.provider,
            account_key: account_key.to_owned(),
            limit_id: limit.id.clone(),
            window_seconds: limit.window_seconds,
            armed_at: now,
            last_observed_at: snapshot.fetched_at,
            state: WatchState::Waiting,
        })
    }

    pub fn state(&self) -> WatchState {
        self.state
    }

    pub fn pending(&self) -> Option<PendingWatch> {
        (self.state() == WatchState::Waiting).then(|| PendingWatch {
            provider: self.provider,
            account_key: self.account_key.clone(),
            limit_id: self.limit_id.clone(),
            window_seconds: self.window_seconds,
            armed_at: self.armed_at,
            last_observed_at: self.last_observed_at,
        })
    }

    pub fn restore(saved: &PendingWatch) -> Option<Self> {
        if !valid_account_key(&saved.account_key)
            || !valid_text(&saved.limit_id)
            || !stable_limit_id(&saved.limit_id)
            || saved.window_seconds == Some(0)
            || !valid_timestamp(saved.armed_at)
            || !valid_timestamp(saved.last_observed_at)
        {
            return None;
        }
        Some(Self {
            provider: saved.provider,
            account_key: saved.account_key.clone(),
            limit_id: saved.limit_id.clone(),
            window_seconds: saved.window_seconds,
            armed_at: saved.armed_at,
            last_observed_at: saved.last_observed_at,
            state: WatchState::Waiting,
        })
    }

    pub fn cancel(&mut self) {
        if self.state == WatchState::Waiting {
            self.state = WatchState::Cancelled;
        }
    }

    /// Completion consumes the watch before returning a candidate, regardless of delivery.
    /// Unknown or different accounts cannot retarget it. A newer verified original account can resume.
    pub fn observe(
        &mut self,
        snapshot: &ProviderSnapshot,
        account_key: Option<&str>,
        now: i64,
        max_age_seconds: u64,
    ) -> Option<RecoveredAllowance> {
        if self.state != WatchState::Waiting
            || snapshot.provider != self.provider
            || !fresh(snapshot, now, max_age_seconds)
            || snapshot.fetched_at <= self.armed_at
            || snapshot.fetched_at <= self.last_observed_at
        {
            return None;
        }
        // Even an unavailable newer response supersedes older positive readings.
        self.last_observed_at = snapshot.fetched_at;
        if account_key != Some(self.account_key.as_str()) {
            return None;
        }
        let ambiguous = ambiguous_ids(&snapshot.limits);
        if ambiguous.contains(self.limit_id.as_str()) {
            return None;
        }
        let limit = snapshot
            .limits
            .iter()
            .find(|limit| limit.id == self.limit_id)?;
        if limit.window_seconds != self.window_seconds {
            return None;
        }
        let (remaining_percent, resets_at) = eligible_reading(limit, now)?;
        if remaining_percent <= 0.0 {
            return None;
        }
        let mut other_exhausted: Vec<_> = snapshot
            .limits
            .iter()
            .filter(|other| {
                other.id != self.limit_id
                    && !ambiguous.contains(other.id.as_str())
                    && other.used_fraction == Some(1.0)
                    && eligible_reading(other, now).is_some()
            })
            .map(|other| ExhaustedQuota {
                limit_id: other.id.clone(),
                label: other.label.clone(),
                window_seconds: other.window_seconds,
            })
            .collect();
        other_exhausted.sort_by(|left, right| left.limit_id.cmp(&right.limit_id));
        self.state = WatchState::Completed;
        Some(RecoveredAllowance {
            provider: self.provider,
            limit_id: limit.id.clone(),
            label: limit.label.clone(),
            window_seconds: limit.window_seconds,
            remaining_percent,
            resets_at,
            observed_at: snapshot.fetched_at,
            other_exhausted,
        })
    }
}

fn valid_timestamp(timestamp: i64) -> bool {
    time::OffsetDateTime::from_unix_timestamp(timestamp).is_ok()
}

fn fresh(snapshot: &ProviderSnapshot, now: i64, max_age_seconds: u64) -> bool {
    valid_timestamp(now)
        && valid_timestamp(snapshot.fetched_at)
        && snapshot.fetched_at <= now
        && now.abs_diff(snapshot.fetched_at) <= max_age_seconds
}

#[cfg(test)]
mod tests;
