//! Work a person asked for that outlives the answer to their request.
//!
//! Every other control verb is a question the daemon can answer inside one
//! exchange, and the client half gives up after three seconds. Keeping a folder
//! offline is not one of those: it fetches every file beneath the folder, which
//! is minutes on anything real. It ran inside the request anyway, so a person
//! who kept a folder offline was told `Cirrove pin timed out` while the daemon
//! went on doing exactly what they had asked -- no way to see how far it had
//! got, no way to stop it, and a final answer that was simply wrong.
//!
//! So the request answers with a job, and this is the register a job lives in
//! while it runs: a level to read it from, and a token to stop it with.
//!
//! What is deliberately *not* in here is ordinary reading. A mount stages cache
//! on every read, so a list of transfers that included those would be a list of
//! everything -- the same reason [`mod@crate::recent`] refuses files a user merely
//! opened -- and the application doing the reading draws its own progress bar.
//! This register holds work a person asked for by name.
use cirrove_core::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How many jobs that ended badly stay visible, and for how long.
///
/// A job that finished is not kept: what it did is on the "kept offline" list,
/// which is a better receipt than a spent progress bar. One that was stopped or
/// failed leaves nothing behind, so it is retained until someone has plausibly
/// seen it -- otherwise a fetch that died on a broken connection would show as a
/// progress bar that simply vanished.
const RETAINED: usize = 4;
const RETAIN_FOR: Duration = Duration::from_secs(600);

/// What kind of work this is.
///
/// `Unknown` for the usual forward-compatibility reason: a newer daemon may run
/// a kind this client has never heard of, and the right answer to one is to keep
/// rendering the rest rather than to fail the whole reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Fetching everything a pin covers, so it is really on this computer.
    #[default]
    KeepOffline,
    #[serde(other)]
    Unknown,
}

/// Where the job has got to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    #[default]
    Running,
    /// Asked to stop. It ends at the next file boundary rather than instantly:
    /// a half-written block is worse than a second of waiting.
    Stopping,
    /// Stopped because someone asked it to.
    Stopped,
    /// Gave up, for the reason in `issue`.
    Failed,
    #[serde(other)]
    Unknown,
}

/// One piece of long work, as a client sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    #[serde(default)]
    pub kind: JobKind,
    /// What is being kept, as a mount-relative path. The provider item id is
    /// what the daemon acts on and is never what a person reads.
    pub name: String,
    #[serde(default)]
    pub files_total: u64,
    #[serde(default)]
    pub files_done: u64,
    #[serde(default)]
    pub bytes_total: u64,
    #[serde(default)]
    pub bytes_done: u64,
    /// Unix seconds. A client shows elapsed time from it and nothing else; a
    /// remaining-time estimate from a cloud transfer is a guess dressed as a
    /// fact.
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub state: JobState,
    #[serde(default)]
    pub issue: Option<String>,
}

impl Job {
    /// Whether this is still moving, so a client knows to keep asking.
    pub fn running(&self) -> bool {
        matches!(self.state, JobState::Running | JobState::Stopping)
    }
}

/// What `stop` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// It was running and has been asked to stop.
    Asked,
    /// It had already ended; the record was cleared instead.
    Dismissed,
    /// No such job. Not an error: a client acting on a list it read a second
    /// ago is racing a job that finished, which is ordinary.
    Unknown,
}

struct Running {
    job: Job,
    cancel: CancellationToken,
}

#[derive(Default)]
struct Inner {
    /// Small, and in the order a person started them. A map would order by hash,
    /// which is the one order a list of someone's own actions must not have.
    running: Vec<Running>,
    ended: VecDeque<(Instant, Job)>,
}

/// The register. One per account, because that is how everything else a client
/// draws is keyed.
#[derive(Default)]
pub struct Jobs {
    inner: Mutex<Inner>,
    /// Woken when any job ends, so a caller that must wait for one -- the
    /// command line, a validator -- can do it without polling.
    ended: tokio::sync::Notify,
}

impl Jobs {
    /// Take the register, recovering from a poisoned lock rather than refusing.
    ///
    /// Nothing in here has an invariant a panic could break: it is a list of
    /// what is running. Refusing to report progress because some unrelated task
    /// panicked while holding this would turn a small fault into a window that
    /// never draws again.
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            self.inner.clear_poison();
            poisoned.into_inner()
        })
    }

    /// Register a new job and hand back the handle that drives it.
    ///
    /// The token is a child of the caller's, so stopping the account or the
    /// daemon stops the job too, and stopping the job leaves them alone.
    pub fn start(
        self: &Arc<Self>,
        kind: JobKind,
        name: String,
        files_total: u64,
        bytes_total: u64,
        parent: &CancellationToken,
    ) -> JobHandle {
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = parent.child_token();
        let job = Job {
            id: id.clone(),
            kind,
            name,
            files_total,
            files_done: 0,
            bytes_total,
            bytes_done: 0,
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or(0),
            state: JobState::Running,
            issue: None,
        };
        self.inner().running.push(Running {
            job,
            cancel: cancel.clone(),
        });
        JobHandle {
            jobs: self.clone(),
            id,
            cancel,
        }
    }

    /// Everything worth showing: what is running, then what ended badly and has
    /// not been seen off yet.
    pub fn list(&self) -> Vec<Job> {
        let mut inner = self.inner();
        let now = Instant::now();
        inner
            .ended
            .retain(|(at, _)| now.duration_since(*at) < RETAIN_FOR);
        let mut out: Vec<Job> = inner.running.iter().map(|r| r.job.clone()).collect();
        out.extend(inner.ended.iter().map(|(_, job)| job.clone()));
        out
    }

    /// Stop a running job, or clear the record of one that already ended.
    ///
    /// Both meanings on one verb because they are one intention: a person
    /// pressing the button beside a progress bar wants it gone, and whether the
    /// work stopped a moment before they pressed it is not something they can
    /// see or should have to care about.
    pub fn stop(&self, id: &str) -> Stopped {
        let mut inner = self.inner();
        if let Some(entry) = inner.running.iter_mut().find(|r| r.job.id == id) {
            entry.job.state = JobState::Stopping;
            entry.cancel.cancel();
            return Stopped::Asked;
        }
        let before = inner.ended.len();
        inner.ended.retain(|(_, job)| job.id != id);
        if inner.ended.len() != before {
            Stopped::Dismissed
        } else {
            Stopped::Unknown
        }
    }

    /// Wait until the named job is no longer running. Returns its final record
    /// when it ended badly, `None` when it finished or was never here.
    ///
    /// For callers whose own contract is synchronous -- `cirrove pin`, which
    /// must not print "kept offline" before anything is kept, and the live-account
    /// validators, which check what is resident the moment the call returns.
    pub async fn wait(&self, id: &str) -> Option<Job> {
        loop {
            let waiting = self.ended.notified();
            tokio::pin!(waiting);
            waiting.as_mut().enable();
            match self.find(id) {
                Some(job) if job.running() => {}
                other => return other,
            }
            waiting.await;
        }
    }

    /// One job by id, running or retained.
    pub fn find(&self, id: &str) -> Option<Job> {
        let inner = self.inner();
        inner
            .running
            .iter()
            .map(|r| &r.job)
            .chain(inner.ended.iter().map(|(_, job)| job))
            .find(|job| job.id == id)
            .cloned()
    }

    /// How many are still moving. The count a tray or a banner shows.
    pub fn running(&self) -> usize {
        self.inner().running.len()
    }

    fn advance(&self, id: &str, files_done: u64, bytes_done: u64) {
        let mut inner = self.inner();
        if let Some(entry) = inner.running.iter_mut().find(|r| r.job.id == id) {
            entry.job.files_done = files_done;
            entry.job.bytes_done = bytes_done;
        }
    }

    fn end(&self, id: &str, outcome: Option<Ended>) {
        {
            let mut inner = self.inner();
            let Some(index) = inner.running.iter().position(|r| r.job.id == id) else {
                return;
            };
            let mut job = inner.running.remove(index).job;
            match outcome {
                // A job that did what it was asked leaves the register: the list
                // of what is kept offline is a better receipt than a full bar.
                None => {}
                Some(ended) => {
                    job.state = ended.state;
                    job.issue = ended.issue;
                    if inner.ended.len() == RETAINED {
                        inner.ended.pop_front();
                    }
                    inner.ended.push_back((Instant::now(), job));
                }
            }
        }
        self.ended.notify_waiters();
    }
}

struct Ended {
    state: JobState,
    issue: Option<String>,
}

/// The running job's half: where progress is written and cancellation is read.
///
/// Dropping one without calling [`JobHandle::finished`] or
/// [`JobHandle::failed`] removes the job silently, which is what an interrupted
/// daemon should do: it is not a failure anybody can act on, and a stopped
/// service has no one to tell.
pub struct JobHandle {
    jobs: Arc<Jobs>,
    id: String,
    /// Cancelled when someone stops this job, or when the account stops.
    pub cancel: CancellationToken,
}

impl JobHandle {
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Record where the work has got to.
    pub fn advance(&self, files_done: u64, bytes_done: u64) {
        self.jobs.advance(&self.id, files_done, bytes_done);
    }
    /// Whether this job's token has been cancelled, for any reason.
    pub fn stopping(&self) -> bool {
        self.cancel.is_cancelled()
    }
    /// Whether the stop was somebody's decision, rather than the account or the
    /// daemon shutting down.
    ///
    /// Those two must not be confused. Stopping a job undoes what it was doing;
    /// a daemon stopping must undo nothing, or every shutdown would throw away
    /// what a user had chosen to keep.
    pub fn asked_to_stop(&self) -> bool {
        self.jobs
            .find(&self.id)
            .is_some_and(|job| job.state == JobState::Stopping)
    }
    /// It did what it was asked. The record goes away.
    pub fn finished(self) {
        self.jobs.end(&self.id, None);
    }
    /// It ended without finishing. The record stays a while, carrying why.
    pub fn failed(self, state: JobState, issue: Option<String>) {
        self.jobs.end(&self.id, Some(Ended { state, issue }));
    }
}

impl Drop for JobHandle {
    /// An interrupted daemon reports nothing: the job is removed, not recorded
    /// as a failure somebody could act on. Harmless after `finished` or
    /// `failed`, which have already taken the job out of the running list.
    fn drop(&mut self) {
        self.jobs.end(&self.id, None);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn register() -> Arc<Jobs> {
        Arc::new(Jobs::default())
    }

    #[tokio::test]
    async fn a_running_job_reports_where_it_has_got_to() {
        let jobs = register();
        let parent = CancellationToken::new();
        let handle = jobs.start(JobKind::KeepOffline, "Photos".into(), 3, 300, &parent);
        handle.advance(1, 100);
        let listed = jobs.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Photos");
        assert_eq!((listed[0].files_done, listed[0].bytes_done), (1, 100));
        assert_eq!((listed[0].files_total, listed[0].bytes_total), (3, 300));
        handle.finished();
        assert!(
            jobs.list().is_empty(),
            "a job that finished leaves no progress bar behind"
        );
    }

    #[tokio::test]
    async fn stopping_cancels_the_token_the_work_is_watching() {
        let jobs = register();
        let parent = CancellationToken::new();
        let handle = jobs.start(JobKind::KeepOffline, "Photos".into(), 1, 1, &parent);
        assert!(!handle.stopping());
        assert_eq!(jobs.stop(handle.id()), Stopped::Asked);
        assert!(handle.stopping(), "the work is watching this token");
        assert_eq!(
            jobs.list()[0].state,
            JobState::Stopping,
            "a client sees that the stop was taken, before the work ends"
        );
        let id = handle.id().to_owned();
        handle.failed(JobState::Stopped, None);
        assert_eq!(jobs.list()[0].state, JobState::Stopped);
        // The same verb clears a record nobody needs any more.
        assert_eq!(jobs.stop(&id), Stopped::Dismissed);
        assert!(jobs.list().is_empty());
        assert_eq!(jobs.stop(&id), Stopped::Unknown);
    }

    #[tokio::test]
    async fn stopping_one_job_leaves_the_account_alone() {
        let jobs = register();
        let parent = CancellationToken::new();
        let handle = jobs.start(JobKind::KeepOffline, "Photos".into(), 1, 1, &parent);
        jobs.stop(handle.id());
        assert!(
            !parent.is_cancelled(),
            "a stopped job must not take the account's own token with it"
        );
    }

    #[tokio::test]
    async fn a_failure_stays_visible_and_says_why() {
        let jobs = register();
        let parent = CancellationToken::new();
        let handle = jobs.start(JobKind::KeepOffline, "Photos".into(), 9, 900, &parent);
        handle.advance(4, 400);
        handle.failed(JobState::Failed, Some("the cloud was unreachable".into()));
        let listed = jobs.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, JobState::Failed);
        assert_eq!(
            listed[0].issue.as_deref(),
            Some("the cloud was unreachable")
        );
        assert_eq!(
            (listed[0].files_done, listed[0].files_total),
            (4, 9),
            "how far it got is the useful part of a failure"
        );
    }

    #[tokio::test]
    async fn only_the_last_few_failures_are_kept() {
        let jobs = register();
        let parent = CancellationToken::new();
        for i in 0..RETAINED + 2 {
            let handle = jobs.start(JobKind::KeepOffline, format!("f{i}"), 1, 1, &parent);
            handle.failed(JobState::Failed, None);
        }
        let listed = jobs.list();
        assert_eq!(listed.len(), RETAINED);
        assert_eq!(listed[0].name, "f2", "the oldest is the one that goes");
    }

    #[tokio::test]
    async fn waiting_returns_when_the_job_ends() {
        let jobs = register();
        let parent = CancellationToken::new();
        let handle = jobs.start(JobKind::KeepOffline, "Photos".into(), 1, 1, &parent);
        let id = handle.id().to_owned();
        let waiter = {
            let jobs = jobs.clone();
            let id = id.clone();
            tokio::spawn(async move { jobs.wait(&id).await })
        };
        tokio::task::yield_now().await;
        handle.failed(JobState::Failed, Some("no".into()));
        let ended = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("waiting ends when the job does")
            .unwrap();
        assert_eq!(ended.map(|job| job.state), Some(JobState::Failed));
    }

    #[tokio::test]
    async fn waiting_for_a_job_that_already_finished_returns_at_once() {
        let jobs = register();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), jobs.wait("nothing"))
                .await
                .expect("an unknown job is not something to wait for")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_dropped_handle_is_not_reported_as_a_failure() {
        let jobs = register();
        let parent = CancellationToken::new();
        drop(jobs.start(JobKind::KeepOffline, "Photos".into(), 1, 1, &parent));
        assert!(
            jobs.list().is_empty(),
            "a daemon shutting down has nobody to tell"
        );
    }
}
