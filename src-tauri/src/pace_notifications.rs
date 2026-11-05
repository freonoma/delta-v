//! In-memory pace alerts from successive accepted quota readings.
//! The first settled reading is silent. Two later over-budget observations, at
//! least five minutes apart, are required before consuming a period's alert.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    model::{ProviderId, ProviderSnapshot},
    notifications::{ambiguous_ids, eligible_reading, valid_account_key},
    settings::PaceNotificationScope,
};

#[derive(Debug, PartialEq)]
pub struct PaceAlert {
    pub provider: ProviderId,
    pub limit_id: String,
    pub label: String,
    pub remaining_percent: f64,
    pub resets_at: i64,
}

#[derive(Default)]
pub struct Evaluator {
    enabled: bool,
    scope: PaceNotificationScope,
    providers: [ProviderTracker; 2],
}

#[derive(Default)]
struct ProviderTracker {
    active_account: Option<String>,
    last_observed_at: Option<i64>,
    accounts: BTreeMap<String, BTreeMap<String, WindowTracker>>,
}

impl ProviderTracker {
    fn reset_baseline(&mut self) {
        self.active_account = None;
        for windows in self.accounts.values_mut() {
            for window in windows.values_mut() {
                window.reset_baseline();
            }
        }
    }
}

struct WindowTracker {
    resets_at: i64,
    window_seconds: u64,
    baseline: bool,
    first_over_at: Option<i64>,
    emitted: bool,
}

impl WindowTracker {
    fn reset_baseline(&mut self) {
        self.baseline = false;
        self.first_over_at = None;
    }

    fn observe(&mut self, reset: i64, duration: u64, settled: bool, over: bool, at: i64) -> bool {
        let reset_changed = self.resets_at.abs_diff(reset) > 1;
        if reset_changed || self.window_seconds != duration {
            // Correcting a future reset must not permit another alert in this period.
            if reset_changed && at >= self.resets_at {
                self.emitted = false;
            }
            self.resets_at = reset;
            self.window_seconds = duration;
            self.reset_baseline();
        }
        if !settled {
            self.reset_baseline();
            return false;
        }
        if !self.baseline {
            self.baseline = true;
            return false;
        }
        if !over {
            self.first_over_at = None;
            return false;
        }
        if self.emitted {
            return false;
        }
        match self.first_over_at {
            None => self.first_over_at = Some(at),
            Some(first) if at.saturating_sub(first) >= 300 => {
                self.emitted = true;
                self.first_over_at = None;
                return true;
            }
            Some(_) => {}
        }
        false
    }
}

impl Evaluator {
    pub fn set_policy(&mut self, enabled: bool, scope: PaceNotificationScope) {
        if self.enabled != enabled || self.scope != scope {
            self.enabled = enabled;
            self.scope = scope;
            for provider in &mut self.providers {
                provider.reset_baseline();
            }
        }
    }

    pub fn reset_baseline(&mut self, provider: ProviderId) {
        self.providers[provider_index(provider)].reset_baseline();
    }

    pub fn observe(
        &mut self,
        snapshot: &ProviderSnapshot,
        account_key: Option<&str>,
        now: i64,
        max_age_seconds: u64,
    ) -> Vec<PaceAlert> {
        if !self.enabled {
            return Vec::new();
        }
        let provider = &mut self.providers[provider_index(snapshot.provider)];
        if snapshot.fetched_at > now || now.abs_diff(snapshot.fetched_at) > max_age_seconds {
            provider.reset_baseline();
            return Vec::new();
        }
        if provider
            .last_observed_at
            .is_some_and(|last| snapshot.fetched_at <= last)
        {
            return Vec::new();
        }
        if provider
            .last_observed_at
            .is_some_and(|last| snapshot.fetched_at.abs_diff(last) > max_age_seconds)
        {
            provider.reset_baseline();
        }
        provider.last_observed_at = Some(snapshot.fetched_at);
        let Some(account) = account_key.filter(|key| valid_account_key(key)) else {
            provider.reset_baseline();
            return Vec::new();
        };
        if provider.active_account.as_deref() != Some(account) {
            provider.reset_baseline();
            provider.active_account = Some(account.to_owned());
        }
        let windows = provider.accounts.entry(account.to_owned()).or_default();
        let ambiguous = ambiguous_ids(&snapshot.limits);
        let mut observed = BTreeSet::new();
        let mut events = Vec::new();
        for limit in &snapshot.limits {
            if ambiguous.contains(limit.id.as_str()) {
                continue;
            }
            let Some((remaining_percent, reset)) = eligible_reading(limit, now) else {
                continue;
            };
            let Some(duration) = limit.window_seconds.filter(|duration| *duration > 0) else {
                continue;
            };
            if self.scope == PaceNotificationScope::Weekly && duration < 604_800 {
                continue;
            }
            let Some(start) = i64::try_from(duration)
                .ok()
                .and_then(|duration| reset.checked_sub(duration))
            else {
                continue;
            };
            if time::OffsetDateTime::from_unix_timestamp(start).is_err()
                || time::OffsetDateTime::from_unix_timestamp(snapshot.fetched_at).is_err()
                || snapshot.fetched_at < start
            {
                continue;
            }
            let elapsed = snapshot.fetched_at.saturating_sub(start) as f64;
            let Some(used) = limit.used_fraction else {
                continue;
            };
            let exhausted = used == 1.0;
            let settled = exhausted || elapsed >= (duration as f64 * 0.1).min(21_600.0);
            let over = exhausted || used - elapsed / duration as f64 > 0.10 + 1e-9;
            observed.insert(limit.id.clone());
            let window = windows.entry(limit.id.clone()).or_insert(WindowTracker {
                resets_at: reset,
                window_seconds: duration,
                baseline: false,
                first_over_at: None,
                emitted: false,
            });
            if window.observe(reset, duration, settled, over, snapshot.fetched_at) {
                events.push(PaceAlert {
                    provider: snapshot.provider,
                    limit_id: limit.id.clone(),
                    label: limit.label.clone(),
                    remaining_percent,
                    resets_at: reset,
                });
            }
        }
        // An absent or unusable quota breaks a sequence of confirmations.
        for (id, window) in windows.iter_mut() {
            if !observed.contains(id) {
                window.reset_baseline();
            }
        }
        events
    }
}

fn provider_index(provider: ProviderId) -> usize {
    match provider {
        ProviderId::Claude => 0,
        ProviderId::Codex => 1,
    }
}

#[cfg(test)]
mod tests;
