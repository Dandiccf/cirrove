//! Bounded revalidation of recently used directories. Filesystem activity is a
//! short lease, not a claim that a desktop window remains visible indefinitely.
use cirrove_core::{CancellationToken, ProviderError, Scope};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tokio::{sync::Notify, time::Instant};

const CAPACITY: usize = 32;
const LEASE: Duration = Duration::from_secs(60);
const INTERVAL: Duration = Duration::from_secs(5);
const SPACING: Duration = Duration::from_secs(2);

type Key = (String, String);
struct Entry {
    scope: Scope,
    used: Instant,
    due: Instant,
    running: bool,
    failures: u32,
}
struct State {
    entries: HashMap<Key, Entry>,
    next_start: Instant,
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryFreshness {
    pub active: usize,
    pub refreshing: usize,
    pub delayed: usize,
}
#[derive(Clone)]
pub(crate) struct DirectoryJob {
    pub scope: Scope,
    pub parent: String,
}
impl State {
    fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| entry.running || now < entry.used + LEASE);
    }
    fn touch(&mut self, scope: &Scope, parent: &str, now: Instant) {
        self.prune(now);
        let key = (scope.collection.clone(), parent.to_owned());
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.used = now;
            return;
        }
        if self.entries.len() >= CAPACITY {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, e)| !e.running)
                .min_by_key(|(_, e)| e.used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                return;
            };
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            key,
            Entry {
                scope: scope.clone(),
                used: now,
                due: now,
                running: false,
                failures: 0,
            },
        );
    }
    fn take(&mut self, now: Instant) -> (Option<DirectoryJob>, Option<Instant>) {
        self.prune(now);
        let entry = self
            .entries
            .iter()
            .filter(|(_, e)| !e.running)
            .min_by_key(|(_, e)| e.due)
            .map(|(key, e)| (key.clone(), e.due.max(self.next_start)));
        let Some((key, due)) = entry else {
            return (None, None);
        };
        if now < due {
            return (None, Some(due));
        }
        let Some(entry) = self.entries.get_mut(&key) else {
            return (None, None);
        };
        entry.running = true;
        self.next_start = now + SPACING;
        (
            Some(DirectoryJob {
                scope: entry.scope.clone(),
                parent: key.1,
            }),
            None,
        )
    }
    fn finish(
        &mut self,
        job: &DirectoryJob,
        outcome: Option<&Result<(), ProviderError>>,
        now: Instant,
    ) {
        // A running entry cannot be evicted; callers finish exactly once before
        // taking another job. A cold foreground fetch can finish the same key.
        let Some(entry) = self
            .entries
            .get_mut(&(job.scope.collection.clone(), job.parent.clone()))
        else {
            return;
        };
        entry.running = false;
        match outcome {
            // A foreground completion may have set a later retry while this
            // job waited for its gate. Skipping must not shorten that deadline.
            None => entry.due = entry.due.max(now + Duration::from_secs(1)),
            Some(Ok(())) => {
                entry.failures = 0;
                entry.due = now + INTERVAL;
            }
            Some(Err(error)) => {
                entry.failures = entry.failures.saturating_add(1);
                let delay = match error {
                    ProviderError::Throttled(delay) => *delay + Duration::from_secs(1),
                    ProviderError::Authentication => Duration::from_secs(60),
                    ProviderError::Permission | ProviderError::NotFound => Duration::from_secs(300),
                    _ => Duration::from_secs((5 * 2u64.pow(entry.failures.min(5))).min(120)),
                };
                entry.due = now + delay;
                if matches!(
                    error,
                    ProviderError::Throttled(_) | ProviderError::Authentication
                ) {
                    self.next_start = self.next_start.max(entry.due);
                }
            }
        }
    }
}

pub(crate) struct DirectoryActivity {
    state: Mutex<State>,
    changed: Notify,
}
impl Default for DirectoryActivity {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                entries: HashMap::new(),
                next_start: Instant::now(),
            }),
            changed: Notify::new(),
        }
    }
}
impl DirectoryActivity {
    pub fn touch(&self, scope: &Scope, parent: &str) {
        if parent.is_empty() || parent.len() > 4096 || scope.collection.len() > 4096 {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            state.touch(scope, parent, Instant::now());
        }
        self.changed.notify_one();
    }
    pub fn finish(&self, job: &DirectoryJob, outcome: Option<&Result<(), ProviderError>>) {
        if let Ok(mut state) = self.state.lock() {
            state.finish(job, outcome, Instant::now());
        }
        self.changed.notify_one();
    }
    pub fn observed(&self, job: &DirectoryJob, outcome: &Result<(), ProviderError>) {
        if let Ok(mut state) = self.state.lock() {
            let key = (job.scope.collection.clone(), job.parent.clone());
            let running = state.entries.get(&key).is_some_and(|e| e.running);
            state.finish(job, Some(outcome), Instant::now());
            if let Some(entry) = state.entries.get_mut(&key) {
                entry.running = running;
            }
        }
    }
    pub async fn next(&self, cancel: &CancellationToken) -> Option<DirectoryJob> {
        loop {
            if cancel.is_cancelled() {
                return None;
            }
            let (job, deadline) = self.state.lock().ok()?.take(Instant::now());
            if let Some(job) = job {
                return Some(job);
            }
            tokio::select! {biased;
                _=cancel.cancelled()=>return None,
                _=self.changed.notified()=>(),
                _=async { match deadline { Some(due)=>tokio::time::sleep_until(due).await, None=>std::future::pending::<()>().await } }=>(),
            }
        }
    }
    pub fn status(&self) -> DirectoryFreshness {
        let Ok(state) = self.state.lock() else {
            return DirectoryFreshness::default();
        };
        let now = Instant::now();
        let mut status = DirectoryFreshness::default();
        for entry in state
            .entries
            .values()
            .filter(|e| e.running || now < e.used + LEASE)
        {
            status.active += 1;
            status.refreshing += usize::from(entry.running);
            status.delayed += usize::from(entry.failures > 0);
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            account: "fixture".into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        }
    }
    fn state(now: Instant) -> State {
        State {
            entries: HashMap::new(),
            next_start: now,
        }
    }
    #[test]
    fn activity_is_bounded_expires_and_never_evicts_running_work() {
        let now = Instant::now();
        let mut s = state(now);
        s.touch(&scope(), "busy", now);
        let job = s.take(now).0.expect("first job");
        for i in 0..10000 {
            s.touch(
                &scope(),
                &format!("folder-{i}"),
                now + Duration::from_millis(i),
            );
        }
        assert_eq!(s.entries.len(), CAPACITY);
        assert!(s.entries.contains_key(&("drive".into(), "busy".into())));
        s.prune(now + LEASE + Duration::from_secs(20));
        assert_eq!(s.entries.len(), 1);
        s.finish(&job, Some(&Ok(())), now + LEASE + Duration::from_secs(20));
        assert!(s.take(now + LEASE + Duration::from_secs(21)).0.is_none());
        assert!(s.entries.is_empty());
    }
    #[test]
    fn account_budget_fairness_and_error_cooldown_survive_repeated_activity() {
        let now = Instant::now();
        let mut s = state(now);
        s.touch(&scope(), "one", now);
        let one = s.take(now).0.expect("first job");
        s.touch(&scope(), "two", now + Duration::from_millis(1));
        s.finish(&one, Some(&Ok(())), now + Duration::from_millis(2));
        assert!(s.take(now + Duration::from_secs(1)).0.is_none());
        let two = s.take(now + SPACING).0.expect("second directory");
        assert_eq!(two.parent, "two");
        s.finish(
            &two,
            Some(&Err(ProviderError::Throttled(Duration::from_secs(20)))),
            now + SPACING,
        );
        for i in 3..20 {
            s.touch(&scope(), "one", now + Duration::from_secs(i));
            assert!(s.take(now + Duration::from_secs(i)).0.is_none());
        }
        assert!(s.take(now + Duration::from_secs(24)).0.is_some());
    }
    #[test]
    fn failed_directory_does_not_block_other_folders_or_renew_its_own_lease() {
        let now = Instant::now();
        let mut s = state(now);
        s.touch(&scope(), "denied", now);
        let job = s.take(now).0.expect("job");
        s.finish(&job, Some(&Err(ProviderError::Permission)), now);
        let retry = s.entries[&("drive".into(), "denied".into())].due;
        s.finish(&job, None, now + Duration::from_millis(1));
        assert_eq!(s.entries[&("drive".into(), "denied".into())].due, retry);
        s.touch(&scope(), "other", now + SPACING);
        assert_eq!(s.take(now + SPACING).0.expect("other job").parent, "other");
        s.prune(now + LEASE + SPACING);
        assert!(!s.entries.contains_key(&("drive".into(), "denied".into())));
    }
}
