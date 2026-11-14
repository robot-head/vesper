//! Group commit across threads.
use super::{Commit, Store, StoreError, StoreErrorKind, StoreResult, Txn};
use crate::device::Device;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

struct Inner<D: Device> {
    store: Store<D>,
    /// A leader is collecting commits for its log sync.
    flushing: bool,
    commits: u64,
}

/// A [`Store`] shared by threads, with leader/follower group commit: the
/// first committer that finds no sync in progress becomes the leader,
/// releases the lock for `window` so that other threads can append their
/// commit groups, then syncs the log once for all of them. Followers wait
/// until the log is durable through their commit. Every `commit` returns
/// only once its commit is durable.
pub struct SharedStore<D: Device> {
    inner: Mutex<Inner<D>>,
    durable: Condvar,
    window: Duration,
}
impl<D: Device> SharedStore<D> {
    pub fn new(store: Store<D>, window: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner {
                store,
                flushing: false,
                commits: 0,
            }),
            durable: Condvar::new(),
            window,
        }
    }
    fn lock(&self) -> MutexGuard<'_, Inner<D>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Runs `f` with exclusive access to the store (begin, reads, writes).
    pub fn with<R>(&self, f: impl FnOnce(&mut Store<D>) -> R) -> R {
        f(&mut self.lock().store)
    }
    /// Commits `txn` and returns once it is durable, sharing the log sync
    /// with every commit appended during the leader's window.
    pub fn commit(&self, txn: Txn) -> StoreResult<Commit> {
        let mut g = self.lock();
        let mut c = g.store.commit_deferred(txn)?;
        g.commits += 1;
        if c.durable {
            return Ok(c);
        }
        loop {
            if g.store.durable_lsn() >= c.lsn {
                c.durable = true;
                return Ok(c);
            }
            if g.store.poisoned() {
                return Err(StoreError::new(
                    StoreErrorKind::State,
                    "store poisoned before this commit became durable; outcome unknown",
                ));
            }
            if g.flushing {
                g = self.durable.wait(g).unwrap_or_else(|e| e.into_inner());
                continue;
            }
            g.flushing = true;
            drop(g);
            if self.window.is_zero() {
                std::thread::yield_now();
            } else {
                std::thread::sleep(self.window);
            }
            g = self.lock();
            let r = g.store.flush();
            g.flushing = false;
            self.durable.notify_all();
            r?;
        }
    }
    /// (commits, log syncs) so far.
    pub fn counts(&self) -> (u64, u64) {
        let g = self.lock();
        (g.commits, g.store.stats().log_flushes)
    }
    pub fn into_inner(self) -> Store<D> {
        self.inner
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .store
    }
}
