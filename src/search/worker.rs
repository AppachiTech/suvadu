//! Runs recall's queries on a thread of their own.
//!
//! A reload counts every match in the whole history and ranks the newest
//! window of them. On a large history, or a broad `fuzzy` query, that takes
//! long enough that doing it on the UI thread — as recall used to — held up
//! the keystrokes typed meanwhile: each one waited for the query before it,
//! and every one of those queries was already out of date.
//!
//! Here the UI thread only takes a [`ReloadRequest`] snapshot and hands it
//! over with a generation number. The worker answers on its own read-only
//! connection. A newer request makes the older one moot three ways:
//!
//! * the running statement is interrupted (`sqlite3_interrupt`), so
//!   `SQLite` stops scanning at once;
//! * between statements, [`ReloadRequest::run`] asks whether it has been
//!   superseded and stops if so;
//! * a result that does arrive for an old generation is dropped unseen.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

use super::data::{ReloadRequest, ReloadResult};
use crate::repository::Repository;

type Job = (u64, ReloadRequest);
type Done = (u64, Result<ReloadResult, String>);

/// Where a requested generation's result stands.
pub(super) enum Poll {
    Ready(Result<ReloadResult, String>),
    Pending,
    /// The worker thread has stopped; the caller must query itself.
    Gone,
}

pub(super) struct QueryWorker {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<Done>,
    /// The generation the UI wants. Anything older is abandoned.
    latest: Arc<AtomicU64>,
    interrupt: rusqlite::InterruptHandle,
}

impl QueryWorker {
    /// Start a worker answering from `repo`, which should be a connection of
    /// its own (see [`Repository::search_reader`]).
    pub(super) fn spawn(repo: Repository) -> std::io::Result<Self> {
        let interrupt = repo.interrupt_handle();
        let latest = Arc::new(AtomicU64::new(0));
        let (jobs, job_rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let wanted = Arc::clone(&latest);
        std::thread::Builder::new()
            .name("suv-recall-query".into())
            .spawn(move || serve(&repo, &job_rx, &done_tx, &wanted))?;
        Ok(Self {
            jobs,
            done,
            latest,
            interrupt,
        })
    }

    /// Ask for `request` as `generation`, which must be larger than any
    /// generation asked for before. Whatever an older one was doing stops.
    /// `false` means the worker has stopped and nothing was asked.
    pub(super) fn submit(&self, generation: u64, request: ReloadRequest) -> bool {
        self.latest.store(generation, Ordering::SeqCst);
        let sent = self.jobs.send((generation, request)).is_ok();
        self.interrupt.interrupt();
        sent
    }

    /// `generation`'s result, waiting at most `wait` for it. Older results
    /// are dropped on the way.
    pub(super) fn poll(&self, generation: u64, wait: std::time::Duration) -> Poll {
        let deadline = std::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.done.recv_timeout(left) {
                Ok((g, result)) if g == generation => return Poll::Ready(result),
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => return Poll::Pending,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Poll::Gone,
            }
        }
    }

    /// Block until `generation`'s result arrives, dropping older ones on the
    /// way. `None` means the worker is gone and the caller must query itself.
    pub(super) fn wait(&self, generation: u64) -> Option<Result<ReloadResult, String>> {
        loop {
            match self.done.recv() {
                Ok((g, result)) if g == generation => return Some(result),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }
}

#[cfg(test)]
impl QueryWorker {
    /// Stop the worker thread the way a crash would: its request channel
    /// closes, it exits, and its result channel closes behind it.
    pub(super) fn stop_thread_for_test(&mut self) {
        self.jobs = mpsc::channel().0;
    }
}

impl Drop for QueryWorker {
    /// Recall is closing: stop the query in flight rather than finish it.
    fn drop(&mut self) {
        self.latest.store(u64::MAX, Ordering::SeqCst);
        self.interrupt.interrupt();
    }
}

/// The worker loop: always answer the newest request, and nothing else.
fn serve(
    repo: &Repository,
    jobs: &mpsc::Receiver<Job>,
    done: &mpsc::Sender<Done>,
    latest: &AtomicU64,
) {
    while let Ok(mut job) = jobs.recv() {
        loop {
            // Requests queued behind this one have already replaced it.
            while let Ok(newer) = jobs.try_recv() {
                job = newer;
            }
            let (generation, request) = &job;
            let superseded = || latest.load(Ordering::SeqCst) != *generation;
            let result = match request.run(repo, &superseded) {
                Ok(Some(result)) => Ok(result),
                // Superseded between statements: the newer request is on its
                // way through the channel.
                Ok(None) => break,
                Err(e) if crate::db::is_interrupted(&e) => {
                    if superseded() {
                        break;
                    }
                    // An interrupt meant for the previous request landed as
                    // this one started. Nothing newer exists; run it again.
                    continue;
                }
                Err(e) => Err(e.to_string()),
            };
            if done.send((*generation, result)).is_err() {
                return; // recall has closed
            }
            break;
        }
    }
}
