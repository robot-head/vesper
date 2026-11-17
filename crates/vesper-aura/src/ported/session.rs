//! `Session`, `PreparedUpdate` and `CloseDecision` over the Vesper
//! reconciler (`reconciler_*` in the Loom package in vesper/). The generic backend and
//! the thread token stay here: `dyn Backend` inside Vesper waits for F7b, so
//! Vesper returns a recorded plan and this wrapper applies it.

use super::{binding, codec};
use crate::{
    Backend, CloseAction, CloseError, CloseFailure, CloseOutcome, CloseRequestError, CommitError,
    CommitFailure, CommitReport, Event, InitError, InitFailure, Key, Limits, PrepareError,
    PrepareFailure, Property, ShutdownError, ShutdownFailure, SlotId, SubscriptionId, Transaction,
    UiThread, ViewError, ViewTree, WidgetId, WrongThread, identity::new_session,
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
    plan: binding::Prepared,
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
    session: u64,
    core: binding::Reconciler,
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
        let words = codec::limits(&limits);
        if !binding::limits_valid(&words) {
            return Err(InitFailure {
                error: InitError::View(ViewError::InvalidLimits),
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
            session,
            core: binding::Reconciler::new(session, &words),
            _local: PhantomData,
        })
    }
    pub fn revision(&self) -> u64 {
        self.core.revision()
    }
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    pub fn widget(&self, key: Key) -> Option<WidgetId> {
        let serial = self.core.widget(key.0)?;
        Some(WidgetId {
            session: self.session,
            serial,
        })
    }
    pub fn slot(&self, key: Key, property: Property) -> Option<SlotId> {
        let serial = self.core.slot(key.0, codec::property(property))?;
        Some(SlotId {
            session: self.session,
            serial,
        })
    }
    pub fn subscription(&self, key: Key, event: Event) -> Option<SubscriptionId> {
        let serial = self.core.subscription(key.0, codec::event(event))?;
        Some(SubscriptionId {
            session: self.session,
            serial,
        })
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
        if thread.check(self.thread).is_err() {
            return Err(PrepareFailure {
                error: PrepareError::WrongThread,
                tree,
            });
        }
        let (words, text) = tree.as_ref().map(codec::tree).unwrap_or_default();
        let plan = self.core.prepare(tree.is_some(), &words, &text);
        self.core.reserve(&plan);
        if let Some(fault) = plan.fault() {
            return Err(PrepareFailure {
                error: PrepareError::View(codec::view_error(fault)),
                tree,
            });
        }
        let (session, base_revision, next_revision) = plan.header();
        let patches = codec::patches(session, &plan.ops(), tree.as_ref());
        Ok(PreparedUpdate {
            transaction: Transaction {
                session,
                base_revision,
                next_revision,
                patches,
                _local: PhantomData,
            },
            plan,
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
        match self.core.commit_check(&update.plan) {
            0 => {}
            1 => return Err(fail(CommitError::ForeignSession, update)),
            2 => return Err(fail(CommitError::StaleRevision, update)),
            _ => return Err(fail(CommitError::Closed, update)),
        }
        if let Err(e) = self.backend.apply(thread, &update.transaction) {
            return Err(fail(CommitError::Backend(e), update));
        }
        self.core.adopt(&update.plan);
        Ok(CommitReport {
            revision: update.transaction.next_revision,
            patches: update.transaction.patches.len(),
        })
    }

    pub fn request_close(&mut self, thread: &UiThread) -> Result<CloseDecision, CloseRequestError> {
        thread
            .check(self.thread)
            .map_err(|_| CloseRequestError::WrongThread)?;
        match self.core.request_close() {
            binding::CloseRequest::Granted(id) => Ok(CloseDecision {
                session: self.session,
                id,
                _local: PhantomData,
            }),
            binding::CloseRequest::AlreadyPending => Err(CloseRequestError::AlreadyPending),
            binding::CloseRequest::Closed => Err(CloseRequestError::Closed),
            binding::CloseRequest::Exhausted => Err(CloseRequestError::Exhausted),
        }
    }

    pub fn resolve_close(
        &mut self,
        thread: &UiThread,
        decision: CloseDecision,
        action: CloseAction,
    ) -> Result<CloseOutcome, Box<CloseFailure<B::Error>>> {
        let fail = |decision, error| Box::new(CloseFailure { decision, error });
        if thread.check(self.thread).is_err() {
            return Err(fail(decision, CloseError::WrongThread));
        }
        match self.core.close_check(decision.session, decision.id) {
            0 => {}
            1 => return Err(fail(decision, CloseError::ForeignSession)),
            _ => return Err(fail(decision, CloseError::StaleDecision)),
        }
        if action == CloseAction::Veto {
            self.core.reopen();
            return Ok(CloseOutcome::Vetoed);
        }
        let update = match self.prepare(thread, None) {
            Ok(update) => update,
            Err(failure) => return Err(fail(decision, CloseError::Prepare(failure))),
        };
        match self.commit(thread, update) {
            Ok(report) => {
                self.core.mark_closed();
                Ok(CloseOutcome::Closed(report))
            }
            Err(failure) => Err(fail(decision, CloseError::Commit(failure))),
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
        let state = self.core.close_state();
        if state == 1 {
            return Err(Box::new(ShutdownFailure {
                session: self,
                error: ShutdownError::PendingClose,
                update: None,
            }));
        }
        if state != 2 {
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

    // Translated from the Rust build's unit test: the counters are forced
    // through the Vesper reconciler instead of private fields.
    #[test]
    fn exhausted_ids_and_revision_fail_without_mounted_changes() {
        let _serial = super::super::PORT_TESTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ui = UiThread::claim_current();
        let limits = Limits::default();
        let mut app =
            Session::new(&ui, RecordingBackend::new(&ui, limits.clone()), limits).unwrap();
        let tree = ViewTree::new(Key(1), vec![Node::new(Key(1), Kind::Text)]);
        app.core.force_counters(u64::MAX, 0);
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
        app.core.force_counters(u64::MAX, u64::MAX);
        assert_eq!(
            app.prepare(&ui, Some(tree)).unwrap_err().error,
            PrepareError::View(ViewError::IdentityExhausted)
        );
        // Explicitly recover this test-only exhausted empty session; normal
        // callers cannot forge counters or reset a session revision.
        app.core.force_counters(u64::MAX, 0);
        app.shutdown(&ui).unwrap();
    }
}
