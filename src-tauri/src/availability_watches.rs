//! Transactional pending watches. A recovery is removed from disk before delivery.

use serde::Serialize;

use crate::{
    availability_store::{Error as StoreError, SavedEntry, SavedWatches, Store},
    model::{ProviderId, ProviderSnapshot},
    notifications::availability::{RecoveredAllowance, Watch},
};

pub struct Registry {
    store: Store,
    saved: SavedWatches,
    loaded: bool,
    error: Option<String>,
    generation: u64,
    failed_observation: [Option<i64>; 2],
}

#[derive(Serialize)]
pub struct WatchStatus {
    pub watches: Vec<WatchSummary>,
    pub error: Option<String>,
}

/// Account bindings stay in the backend. IDs are strings to avoid JavaScript rounding.
#[derive(Serialize)]
pub struct WatchSummary {
    pub id: String,
    pub provider: ProviderId,
    pub limit_id: String,
    pub label: String,
    pub window_seconds: Option<u64>,
    pub armed_at: i64,
}

impl Registry {
    pub fn load(store: Store) -> Self {
        let (saved, loaded, error) = match store.load() {
            Ok(saved) => (saved, true, None),
            Err(error) => (SavedWatches::default(), false, Some(error.to_string())),
        };
        Self {
            store,
            saved,
            loaded,
            error,
            generation: 0,
            failed_observation: [None; 2],
        }
    }

    pub fn status(&self) -> WatchStatus {
        WatchStatus {
            watches: self
                .saved
                .entries
                .iter()
                .map(|entry| WatchSummary {
                    id: entry.id.to_string(),
                    provider: entry.watch.provider,
                    limit_id: entry.watch.limit_id.clone(),
                    label: entry.label.clone(),
                    window_seconds: entry.watch.window_seconds,
                    armed_at: entry.watch.armed_at,
                })
                .collect(),
            error: self.error.clone(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn has_provider(&self, provider: ProviderId) -> bool {
        self.saved
            .entries
            .iter()
            .any(|entry| entry.watch.provider == provider)
    }

    pub fn arm(
        &mut self,
        snapshot: &ProviderSnapshot,
        limit_id: &str,
        now: i64,
        max_age: u64,
    ) -> Result<String, String> {
        self.require_loaded()?;
        let watch = Watch::arm(
            snapshot,
            snapshot.history_account.as_deref(),
            limit_id,
            now,
            max_age,
        )
        .map_err(|error| error.to_string())?
        .pending()
        .ok_or("Could not start the availability watch.")?;
        if let Some(existing) = self.saved.entries.iter().find(|entry| {
            entry.watch.provider == watch.provider
                && entry.watch.account_key == watch.account_key
                && entry.watch.limit_id == watch.limit_id
                && entry.watch.window_seconds == watch.window_seconds
        }) {
            let id = existing.id.to_string();
            self.commit(self.saved.clone(), false)?;
            return Ok(id);
        }
        let mut next = self.saved.clone();
        let id = next.next_id;
        next.next_id = id
            .checked_add(1)
            .ok_or("The availability watch identifier limit has been reached.")?;
        let label = snapshot
            .limits
            .iter()
            .find(|limit| limit.id == limit_id)
            .ok_or("The quota is no longer available.")?
            .label
            .clone();
        next.entries.push(SavedEntry { id, label, watch });
        self.commit(next, true)?;
        Ok(id.to_string())
    }

    pub fn cancel(&mut self, id: &str) -> Result<(), String> {
        self.require_loaded()?;
        let id: u64 = id
            .parse()
            .ok()
            .filter(|id| *id > 0)
            .ok_or("The availability watch identifier is invalid.")?;
        let mut next = self.saved.clone();
        let Some(index) = next.entries.iter().position(|entry| entry.id == id) else {
            return self.commit(next, false);
        };
        let mut watch = Watch::restore(&next.entries[index].watch)
            .ok_or("The saved availability watch is invalid.")?;
        watch.cancel();
        if watch.pending().is_none() {
            next.entries.remove(index);
        }
        self.commit(next, true)
    }

    pub fn observe(
        &mut self,
        snapshot: &ProviderSnapshot,
        can_deliver: bool,
        now: i64,
        max_age: u64,
    ) -> Result<Vec<RecoveredAllowance>, String> {
        self.observe_with(snapshot, can_deliver, now, max_age, Store::save)
    }

    fn observe_with(
        &mut self,
        snapshot: &ProviderSnapshot,
        can_deliver: bool,
        now: i64,
        max_age: u64,
        persist: impl FnOnce(&Store, &SavedWatches, &SavedWatches) -> Result<(), StoreError>,
    ) -> Result<Vec<RecoveredAllowance>, String> {
        self.require_loaded()?;
        // Permission never prompts here. A later accepted reading must confirm recovery.
        if !can_deliver {
            return Ok(Vec::new());
        }
        let index = match snapshot.provider {
            ProviderId::Claude => 0,
            ProviderId::Codex => 1,
        };
        if self.failed_observation[index].is_some_and(|at| snapshot.fetched_at <= at) {
            return Ok(Vec::new());
        }
        let mut next = self.saved.clone();
        let mut candidates = Vec::new();
        for entry in &mut next.entries {
            let mut watch =
                Watch::restore(&entry.watch).ok_or("The saved availability watch is invalid.")?;
            if let Some(candidate) =
                watch.observe(snapshot, snapshot.history_account.as_deref(), now, max_age)
            {
                candidates.push((entry.id, candidate));
            } else if let Some(pending) = watch.pending() {
                entry.watch = pending;
            }
        }
        next.entries
            .retain(|entry| !candidates.iter().any(|(id, _)| *id == entry.id));
        if next != self.saved {
            if let Err(error) = self.commit_with(next, false, persist) {
                // A failed write must not let a superseded capture complete this watch later.
                self.failed_observation[index] = Some(snapshot.fetched_at);
                return Err(error);
            }
            self.failed_observation[index] = None;
        }
        Ok(candidates
            .into_iter()
            .map(|(_, candidate)| candidate)
            .collect())
    }

    fn require_loaded(&self) -> Result<(), String> {
        if self.loaded {
            Ok(())
        } else {
            Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Could not read availability watches.".into()))
        }
    }

    fn commit(&mut self, next: SavedWatches, invalidate: bool) -> Result<(), String> {
        self.commit_with(next, invalidate, Store::save)
    }

    fn commit_with(
        &mut self,
        next: SavedWatches,
        invalidate: bool,
        persist: impl FnOnce(&Store, &SavedWatches, &SavedWatches) -> Result<(), StoreError>,
    ) -> Result<(), String> {
        let result = persist(&self.store, &self.saved, &next);
        // Directory sync can fail after replacement. Adopt the committed file, but do not send.
        if result.is_ok() || result.as_ref().is_err_and(|error| error.committed()) {
            self.saved = next;
            if invalidate {
                self.generation = self.generation.wrapping_add(1);
            }
        }
        self.error = result.as_ref().err().map(ToString::to_string);
        result.map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests;
