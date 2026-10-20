//! In-memory low-allowance evaluation, independent of polling, history and delivery.
//! Use accepted responses, verified account keys and the polling freshness allowance.
//! Failed checks must not replay cached data. First readings establish silent baselines.
//! Emitted crossings are candidates, not confirmation of delivery.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Limit, LimitKind, Provenance, ProviderId, ProviderSnapshot};

// Runtime integration follows the independently tested watch evaluator.
#[cfg_attr(not(test), allow(dead_code))]
pub mod availability;

#[derive(Debug, PartialEq)]
pub struct LowAllowance {
    pub provider: ProviderId,
    pub limit_id: String,
    pub label: String,
    pub remaining_percent: f64,
    pub resets_at: i64,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("The low-allowance threshold must be between 0 and 100.")]
pub struct PolicyError;

pub struct Evaluator {
    enabled: bool,
    threshold: u8,
    providers: [ProviderTracker; 2],
}

impl Default for Evaluator {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 20,
            providers: Default::default(),
        }
    }
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
        for account in self.accounts.values_mut() {
            for window in account.values_mut() {
                window.previous_low = None;
            }
        }
    }
}

struct WindowTracker {
    // Keep the reset anchor fixed so one-second jitter cannot accumulate into a false reset.
    resets_at: i64,
    window_seconds: Option<u64>,
    previous_low: Option<bool>,
    emitted: bool,
}

impl WindowTracker {
    fn observe(
        &mut self,
        resets_at: i64,
        window_seconds: Option<u64>,
        low: bool,
        observed_at: i64,
    ) -> bool {
        let reset_changed = self.resets_at.abs_diff(resets_at) > 1;
        if reset_changed || self.window_seconds != window_seconds {
            // A reset correction alone does not prove a refill.
            // Use the reading's capture time; processing may finish after the reset boundary.
            if reset_changed && observed_at >= self.resets_at {
                self.emitted = false;
            }
            self.resets_at = resets_at;
            self.window_seconds = window_seconds;
            self.previous_low = None;
        }
        let crossing = self.previous_low == Some(false) && low && !self.emitted;
        self.previous_low = Some(low);
        self.emitted |= crossing;
        crossing
    }
}

impl Evaluator {
    /// Rebaseline after policy changes, preserving this run's emitted periods.
    /// Display mode, provider visibility and history recording do not affect evaluation.
    pub fn set_policy(&mut self, enabled: bool, threshold: u8) -> Result<(), PolicyError> {
        if threshold > 100 {
            return Err(PolicyError);
        }
        if self.enabled != enabled || self.threshold != threshold {
            self.enabled = enabled;
            self.threshold = threshold;
            for provider in &mut self.providers {
                provider.reset_baseline();
            }
        }
        Ok(())
    }

    /// Rebaseline on disconnect or account recovery, preserving duplicate suppression.
    /// Pass in-flight responses to observe only after the runtime's generation checks.
    pub fn reset_baseline(&mut self, provider: ProviderId) {
        self.providers[provider_index(provider)].reset_baseline();
    }

    pub fn observe(
        &mut self,
        snapshot: &ProviderSnapshot,
        account_key: Option<&str>,
        now: i64,
        max_age_seconds: u64,
    ) -> Vec<LowAllowance> {
        if !self.enabled
            || snapshot.fetched_at > now
            || now.abs_diff(snapshot.fetched_at) > max_age_seconds
        {
            return Vec::new();
        }
        let provider = &mut self.providers[provider_index(snapshot.provider)];
        if provider
            .last_observed_at
            .is_some_and(|previous| snapshot.fetched_at <= previous)
        {
            return Vec::new();
        }
        provider.last_observed_at = Some(snapshot.fetched_at);
        let Some(account_key) = account_key.filter(|key| valid_account_key(key)) else {
            // Break comparisons across unknown identity while retaining already-emitted periods.
            provider.reset_baseline();
            return Vec::new();
        };
        if provider.active_account.as_deref() != Some(account_key) {
            provider.reset_baseline();
            provider.active_account = Some(account_key.to_owned());
        }
        let windows = provider.accounts.entry(account_key.to_owned()).or_default();
        let ambiguous = ambiguous_ids(&snapshot.limits);
        let mut events = Vec::new();
        for limit in &snapshot.limits {
            if ambiguous.contains(limit.id.as_str()) {
                continue;
            }
            let Some((remaining, resets_at)) = eligible_reading(limit, now) else {
                continue;
            };
            let low = remaining < f64::from(self.threshold);
            let window = windows.entry(limit.id.clone()).or_insert(WindowTracker {
                resets_at,
                window_seconds: limit.window_seconds,
                previous_low: None,
                emitted: false,
            });
            if window.observe(resets_at, limit.window_seconds, low, snapshot.fetched_at) {
                events.push(LowAllowance {
                    provider: snapshot.provider,
                    limit_id: limit.id.clone(),
                    label: limit.label.clone(),
                    remaining_percent: remaining,
                    resets_at,
                });
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

fn valid_account_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ambiguous_ids(limits: &[Limit]) -> BTreeSet<&str> {
    let mut seen = BTreeSet::new();
    let mut ambiguous = BTreeSet::new();
    for limit in limits {
        if !seen.insert(limit.id.as_str()) {
            ambiguous.insert(limit.id.as_str());
        }
        // A parser's duplicate-row suffix makes both that row and the original ID ambiguous.
        if let Some((base, _)) = limit.id.split_once(":row:") {
            ambiguous.insert(base);
        }
    }
    ambiguous
}

fn eligible_reading(limit: &Limit, now: i64) -> Option<(f64, i64)> {
    let used = limit.used_fraction?;
    let reset = limit.resets_at?;
    if !limit.enabled
        || limit.kind != LimitKind::Quota
        || limit.provenance != Provenance::Official
        || !used.is_finite()
        || !(0.0..=1.0).contains(&used)
        || reset <= now
        || time::OffsetDateTime::from_unix_timestamp(reset).is_err()
        || limit.window_seconds == Some(0)
        || !valid_text(&limit.id)
        || !valid_text(&limit.label)
        || !stable_limit_id(&limit.id)
    {
        return None;
    }
    // Match usage.ts highlighting, including decimal noise at exact thresholds.
    let remaining = ((100.0 - used * 100.0) * 1e9).round() / 1e9;
    Some((remaining, reset))
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn stable_limit_id(id: &str) -> bool {
    !id.split(':').any(|part| part == "row")
        && !id.strip_prefix("additional:").is_some_and(|suffix| {
            suffix
                .split(':')
                .next()
                .is_some_and(|part| part.parse::<usize>().is_ok())
        })
}

#[cfg(test)]
mod tests;
