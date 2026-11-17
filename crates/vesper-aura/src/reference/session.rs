//! Rust build of the session (frozen, #910): the Vesper build is
//! `reconciler_*` in the Loom package in vesper/.
use super::{
    identity::Ids,
    plan::{self, Mounted},
    view,
};
use crate::{
    Backend, CloseAction, CloseError, CloseFailure, CloseOutcome, CloseRequestError, CommitError,
    CommitFailure, CommitReport, InitError, InitFailure, Key, Limits, PrepareError, PrepareFailure,
    ShutdownError, ShutdownFailure, SlotId, SubscriptionId, Transaction, UiThread, ViewError,
    ViewTree, WidgetId, WrongThread, identity::new_session,
};
use std::{marker::PhantomData, rc::Rc, thread::ThreadId};

/// A prepared update owns its intended tree and all reserved identities. A
/// rejection returns this exact owner for retry/inspection. Dropping a plan
/// never changes mounted widgets or disconnects their subscriptions.
///
/// ```compile_fail
/// use vesper_aura::PreparedUpdate;
/// fn send<T: Send>() {}
/// send::<PreparedUpdate>();
/// ```
#[derive(Debug)]
#[must_use = "a prepared update must be committed, retained, or explicitly discarded"]
pub struct PreparedUpdate {
    pub(crate) transaction: Transaction,
    next: Option<Mounted>,
    tree: Option<ViewTree>,
    _local: PhantomData<Rc<()>>,
}
impl PreparedUpdate {
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }
    pub fn tree(&self) -> Option<&ViewTree> {
        self.tree.as_ref()
    }
    /// Explicitly discard a failed/stale plan while recovering its input tree.
    /// Reserved identities are never reused.
    pub fn into_tree(self) -> Option<ViewTree> {
        self.tree
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseState {
    Open,
    Pending(u64),
    Closed,
}

/// Owns all mounted backend widgets and registrations. It is !Send and !Sync,
/// independent of whether B itself can move between threads.
///
/// ```compile_fail
/// use vesper_aura::{Session, RecordingBackend};
/// fn send<T: Send>() {}
/// send::<Session<RecordingBackend>>();
/// ```
/// ```compile_fail
/// use vesper_aura::{Session, RecordingBackend};
/// fn sync<T: Sync>() {}
/// sync::<Session<RecordingBackend>>();
/// ```
#[derive(Debug)]
#[must_use = "explicitly shut down the session to disconnect mounted subscriptions"]
pub struct Session<B> {
    backend: B,
    thread: ThreadId,
    limits: Limits,
    ids: Ids,
    revision: u64,
    mounted: Option<Mounted>,
    close: CloseState,
    next_close: u64,
    _local: PhantomData<Rc<()>>,
}
impl<B: Backend> Session<B> {
    pub fn new(thread: &UiThread, backend: B, limits: Limits) -> Result<Self, InitFailure<B>> {
        if thread.check(thread.id()).is_err() {
            return Err(InitFailure {
                error: InitError::WrongThread,
                backend,
            });
        }
        if let Err(e) = limits.check() {
            return Err(InitFailure {
                error: InitError::View(e),
                backend,
            });
        }
        let Some(session) = new_session() else {
            return Err(InitFailure {
                error: InitError::IdentityExhausted,
                backend,
            });
        };
        Ok(Self {
            backend,
            thread: thread.id(),
            limits,
            ids: Ids { session, serial: 1 },
            revision: 0,
            mounted: None,
            close: CloseState::Open,
            next_close: 1,
            _local: PhantomData,
        })
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    pub fn widget(&self, key: Key) -> Option<WidgetId> {
        Some(self.mounted.as_ref()?.nodes.get(&key)?.id)
    }
    pub fn slot(&self, key: Key, property: crate::Property) -> Option<SlotId> {
        Some(
            self.mounted
                .as_ref()?
                .nodes
                .get(&key)?
                .properties
                .get(&property)?
                .id,
        )
    }
    pub fn subscription(&self, key: Key, event: crate::Event) -> Option<SubscriptionId> {
        Some(
            self.mounted
                .as_ref()?
                .nodes
                .get(&key)?
                .subscriptions
                .get(&event)?
                .id,
        )
    }
    pub fn backend(&self, thread: &UiThread) -> Result<&B, WrongThread> {
        thread.check(self.thread)?;
        Ok(&self.backend)
    }
    /// For backend-specific event input, failure injection and local widget
    /// state only. Trusted embedders must not mutate structural state behind
    /// the Session's snapshot or violate the Backend transaction contract.
    pub fn backend_mut(&mut self, thread: &UiThread) -> Result<&mut B, WrongThread> {
        thread.check(self.thread)?;
        Ok(&mut self.backend)
    }

    pub fn prepare(
        &mut self,
        thread: &UiThread,
        tree: Option<ViewTree>,
    ) -> Result<PreparedUpdate, PrepareFailure> {
        let fail = |error, tree| PrepareFailure { error, tree };
        if thread.check(self.thread).is_err() {
            return Err(fail(PrepareError::WrongThread, tree));
        }
        if self.close == CloseState::Closed {
            return Err(fail(PrepareError::View(ViewError::Closed), tree));
        }
        let next_revision = match self.revision.checked_add(1) {
            Some(r) => r,
            None => return Err(fail(PrepareError::View(ViewError::IdentityExhausted), tree)),
        };
        let validated = match tree
            .as_ref()
            .map(|t| view::validate(t, &self.limits))
            .transpose()
        {
            Ok(v) => v,
            Err(e) => return Err(fail(PrepareError::View(e), tree)),
        };
        let next = validated
            .as_ref()
            .zip(tree.as_ref())
            .map(|(v, t)| (v, t.root));
        let (patches, next) =
            match plan::build(self.mounted.as_ref(), next, &mut self.ids, &self.limits) {
                Ok(b) => b,
                Err(e) => return Err(fail(PrepareError::View(e), tree)),
            };
        Ok(PreparedUpdate {
            transaction: Transaction {
                session: self.ids.session,
                base_revision: self.revision,
                next_revision,
                patches,
                _local: PhantomData,
            },
            next,
            tree,
            _local: PhantomData,
        })
    }

    pub fn commit(
        &mut self,
        thread: &UiThread,
        update: PreparedUpdate,
    ) -> Result<CommitReport, Box<CommitFailure<B::Error>>> {
        let fail = |error, update| Box::new(CommitFailure { error, update });
        if thread.check(self.thread).is_err() {
            return Err(fail(CommitError::WrongThread, update));
        }
        if update.transaction.session != self.ids.session {
            return Err(fail(CommitError::ForeignSession, update));
        }
        if update.transaction.base_revision != self.revision {
            return Err(fail(CommitError::StaleRevision, update));
        }
        if self.close == CloseState::Closed {
            return Err(fail(CommitError::Closed, update));
        }
        if let Err(e) = self.backend.apply(thread, &update.transaction) {
            return Err(fail(CommitError::Backend(e), update));
        }
        let report = CommitReport {
            revision: update.transaction.next_revision,
            patches: update.transaction.patches.len(),
        };
        self.mounted = update.next;
        self.revision = report.revision;
        Ok(report)
    }

    pub fn request_close(&mut self, thread: &UiThread) -> Result<CloseDecision, CloseRequestError> {
        thread
            .check(self.thread)
            .map_err(|_| CloseRequestError::WrongThread)?;
        match self.close {
            CloseState::Pending(_) => return Err(CloseRequestError::AlreadyPending),
            CloseState::Closed => return Err(CloseRequestError::Closed),
            CloseState::Open => {}
        }
        let id = self.next_close;
        self.next_close = id.checked_add(1).ok_or(CloseRequestError::Exhausted)?;
        self.close = CloseState::Pending(id);
        Ok(CloseDecision {
            session: self.ids.session,
            id,
            _local: PhantomData,
        })
    }

    pub fn resolve_close(
        &mut self,
        thread: &UiThread,
        decision: CloseDecision,
        action: CloseAction,
    ) -> Result<CloseOutcome, Box<CloseFailure<B::Error>>> {
        if thread.check(self.thread).is_err() {
            return Err(Box::new(CloseFailure {
                decision,
                error: CloseError::WrongThread,
            }));
        }
        if decision.session != self.ids.session {
            return Err(Box::new(CloseFailure {
                decision,
                error: CloseError::ForeignSession,
            }));
        }
        if self.close != CloseState::Pending(decision.id) {
            return Err(Box::new(CloseFailure {
                decision,
                error: CloseError::StaleDecision,
            }));
        }
        if action == CloseAction::Veto {
            self.close = CloseState::Open;
            return Ok(CloseOutcome::Vetoed);
        }
        let update = match self.prepare(thread, None) {
            Ok(update) => update,
            Err(failure) => {
                return Err(Box::new(CloseFailure {
                    decision,
                    error: CloseError::Prepare(failure),
                }));
            }
        };
        match self.commit(thread, update) {
            Ok(report) => {
                self.close = CloseState::Closed;
                Ok(CloseOutcome::Closed(report))
            }
            Err(failure) => Err(Box::new(CloseFailure {
                decision,
                error: CloseError::Commit(failure),
            })),
        }
    }

    /// Explicitly disconnect and destroy all mounted nodes, returning the
    /// backend owner. Rejection returns the complete still-live Session and,
    /// for backend rejection, its failed plan. Pending close decisions must be
    /// resolved first. Dropping Rust owners remains possible and is not a
    /// substitute for this protocol.
    pub fn shutdown(mut self, thread: &UiThread) -> Result<B, Box<ShutdownFailure<B>>> {
        if thread.check(self.thread).is_err() {
            return Err(Box::new(ShutdownFailure {
                session: self,
                error: ShutdownError::WrongThread,
                update: None,
            }));
        }
        if matches!(self.close, CloseState::Pending(_)) {
            return Err(Box::new(ShutdownFailure {
                session: self,
                error: ShutdownError::PendingClose,
                update: None,
            }));
        }
        if self.close != CloseState::Closed {
            let update = match self.prepare(thread, None) {
                Ok(u) => u,
                Err(e) => {
                    return Err(Box::new(ShutdownFailure {
                        session: self,
                        error: ShutdownError::Prepare(e.error),
                        update: None,
                    }));
                }
            };
            if let Err(e) = self.commit(thread, update) {
                return Err(Box::new(ShutdownFailure {
                    session: self,
                    error: ShutdownError::Commit(e.error),
                    update: Some(e.update),
                }));
            }
        }
        Ok(self.backend)
    }
}

/// Consuming one-shot close owner. Dropping this value leaves the Session in
/// PendingClose; it does not silently allow/veto. Host code must retain it
/// while awaiting asynchronous work, then explicitly resolve it. Rust cannot
/// prohibit Drop and this is not a compiler linearity guarantee.
///
/// ```compile_fail
/// use vesper_aura::CloseDecision;
/// fn sync<T: Sync>() {}
/// sync::<CloseDecision>();
/// ```
#[derive(Debug)]
#[must_use = "explicitly veto or allow this pending close decision"]
pub struct CloseDecision {
    session: u64,
    id: u64,
    _local: PhantomData<Rc<()>>,
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, Node, RecordingBackend};

    #[test]
    fn exhausted_ids_and_revision_fail_without_mounted_changes() {
        let ui = UiThread::claim_current();
        let limits = Limits::default();
        let mut app =
            Session::new(&ui, RecordingBackend::new(&ui, limits.clone()), limits).unwrap();
        let tree = ViewTree::new(Key(1), vec![Node::new(Key(1), Kind::Text)]);
        app.ids.serial = u64::MAX;
        let failure = app.prepare(&ui, Some(tree.clone())).unwrap_err();
        assert_eq!(
            failure.error,
            PrepareError::View(ViewError::IdentityExhausted)
        );
        assert_eq!(failure.tree, Some(tree.clone()));
        assert!(
            app.backend(&ui)
                .unwrap()
                .snapshot(&ui)
                .unwrap()
                .widgets
                .is_empty()
        );
        app.revision = u64::MAX;
        assert_eq!(
            app.prepare(&ui, Some(tree)).unwrap_err().error,
            PrepareError::View(ViewError::IdentityExhausted)
        );
        // Explicitly recover this test-only exhausted empty session; normal
        // callers cannot forge counters or reset a session revision.
        app.revision = 0;
        app.shutdown(&ui).unwrap();
    }
}
