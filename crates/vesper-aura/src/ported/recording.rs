//! `RecordingBackend` over the Vesper recorder (`recorder_*` in
//! the Loom package in vesper/). The recorder stages, checks and publishes every
//! transaction; this wrapper keeps the thread token and a copied
//! [`RecordingSnapshot`] of the published state, refreshed after each
//! change, so the accessors can lend references as before.

use super::{binding, codec};
use crate::{
    Backend, BackendError, HandlerKey, Limits, RecordingSnapshot, SlotId, SubscriptionId,
    Transaction, UiThread, Value, WidgetId, WidgetSnapshot,
};
use std::{marker::PhantomData, rc::Rc, thread::ThreadId};

/// Executing retained-state adapter with atomic staging and bounded last-batch
/// recording. It does not invoke an OS toolkit, closures or foreign functions.
///
/// ```compile_fail
/// use vesper_aura::RecordingBackend;
/// fn send<T: Send>() {}
/// send::<RecordingBackend>();
/// ```
#[derive(Debug)]
pub struct RecordingBackend {
    thread: ThreadId,
    pub(crate) rec: binding::Recorder,
    snapshot: RecordingSnapshot,
    _local: PhantomData<Rc<()>>,
}
impl RecordingBackend {
    pub(crate) fn stage_clone(&self) -> Self {
        Self {
            thread: self.thread,
            rec: self.rec.clone(),
            snapshot: self.snapshot.clone(),
            _local: PhantomData,
        }
    }
    pub fn new(thread: &UiThread, limits: Limits) -> Self {
        Self {
            thread: thread.id(),
            rec: binding::Recorder::new(&codec::limits(&limits)),
            snapshot: RecordingSnapshot::default(),
            _local: PhantomData,
        }
    }
    pub fn snapshot(&self, thread: &UiThread) -> Result<&RecordingSnapshot, BackendError> {
        self.check_thread(thread)?;
        Ok(&self.snapshot)
    }
    pub fn reject_next_at(&mut self, thread: &UiThread, patch: usize) -> Result<(), BackendError> {
        self.check_thread(thread)?;
        self.rec.reject_next_at(patch as u64);
        Ok(())
    }
    pub fn set_user_state(
        &mut self,
        thread: &UiThread,
        widget: WidgetId,
        state: u64,
    ) -> Result<(), BackendError> {
        self.check_thread(thread)?;
        if let Some(fault) = self
            .rec
            .set_user_state(widget.session, widget.serial, state)
        {
            return Err(codec::backend_error(fault));
        }
        self.refresh();
        Ok(())
    }
    pub fn widget(
        &self,
        thread: &UiThread,
        widget: WidgetId,
    ) -> Result<&WidgetSnapshot, BackendError> {
        self.check_thread(thread)?;
        if let Some(fault) = self.rec.check_widget(widget.session, widget.serial) {
            return Err(codec::backend_error(fault));
        }
        self.snapshot
            .widgets
            .get(&widget)
            .ok_or(BackendError::StaleWidget)
    }
    pub fn slot(&self, thread: &UiThread, slot: SlotId) -> Result<&Value, BackendError> {
        self.check_thread(thread)?;
        if let Some(fault) = self.rec.check_slot(slot.session, slot.serial) {
            return Err(codec::backend_error(fault));
        }
        self.snapshot
            .slots
            .get(&slot)
            .ok_or(BackendError::StaleSlot)
    }
    /// Resolve a live registration to its host callback identity. A destroyed
    /// or disconnected registration cannot deliver events through this path.
    pub fn dispatch(
        &self,
        thread: &UiThread,
        subscription: SubscriptionId,
    ) -> Result<HandlerKey, BackendError> {
        self.check_thread(thread)?;
        self.rec
            .dispatch(subscription.session, subscription.serial)
            .map(HandlerKey)
            .map_err(codec::backend_error)
    }
    pub(crate) fn check_thread(&self, thread: &UiThread) -> Result<(), BackendError> {
        thread
            .check(self.thread)
            .map_err(|_| BackendError::WrongThread)
    }
    fn refresh(&mut self) {
        self.snapshot = codec::snapshot(self.rec.as_ref().st());
    }
}
impl Backend for RecordingBackend {
    type Error = BackendError;
    fn apply(&mut self, thread: &UiThread, transaction: &Transaction) -> Result<(), BackendError> {
        self.check_thread(thread)?;
        let (words, text) = codec::transaction(&transaction.patches);
        if let Some(fault) = self.rec.apply(
            transaction.session,
            transaction.base_revision,
            transaction.next_revision,
            &words,
            &text,
        ) {
            return Err(codec::backend_error(fault));
        }
        self.refresh();
        Ok(())
    }
}
