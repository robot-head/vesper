use crate::{
    Backend, Event, HandlerKey, Kind, Limits, Patch, Property, SlotId, SubscriptionId, Transaction,
    UiThread, Value, WidgetId,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    rc::Rc,
    thread::ThreadId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    WrongThread,
    ForeignSession,
    StaleRevision,
    StaleWidget,
    StaleSlot,
    StaleSubscription,
    Invalid(&'static str),
    Budget(&'static str),
    /// Injected before patch i, or after all patches if i is at/above length.
    Injected(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordedEvent {
    RootChanged(Option<WidgetId>),
    Created(WidgetId),
    Destroyed(WidgetId),
    SlotBound(SlotId),
    /// Fine-grained slot invalidation, only for an actually changed value.
    SlotChanged(SlotId),
    SlotUnbound(SlotId),
    Subscribed(SubscriptionId),
    Disconnected(SubscriptionId),
    ChildRemoved {
        parent: WidgetId,
        child: WidgetId,
        index: usize,
    },
    ChildInserted {
        parent: WidgetId,
        child: WidgetId,
        index: usize,
    },
    ChildMoved {
        parent: WidgetId,
        child: WidgetId,
        from: usize,
        to: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetSnapshot {
    pub kind: Kind,
    pub parent: Option<WidgetId>,
    pub children: Vec<WidgetId>,
    pub properties: BTreeMap<Property, SlotId>,
    pub subscriptions: BTreeMap<Event, SubscriptionId>,
    /// Independent native-like state, never rewritten by view reconciliation.
    pub user_state: u64,
}

/// Read-only copied recording data; no native owner is contained here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordingSnapshot {
    pub revision: u64,
    pub root: Option<WidgetId>,
    pub widgets: BTreeMap<WidgetId, WidgetSnapshot>,
    pub slots: BTreeMap<SlotId, Value>,
    pub subscriptions: BTreeMap<SubscriptionId, (WidgetId, Event, HandlerKey)>,
    /// Only the latest successful transaction, never an unbounded history.
    /// Callers that want a full history must retain/copy these explicit outputs.
    pub last_events: Vec<RecordedEvent>,
    pub created_subscriptions: u64,
    pub disconnected_subscriptions: u64,
    session: Option<u64>,
    high_water: u64,
    slot_owners: BTreeMap<SlotId, (WidgetId, Property)>,
}

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
    limits: Limits,
    snapshot: RecordingSnapshot,
    reject: Option<usize>,
    _local: PhantomData<Rc<()>>,
}
impl RecordingBackend {
    pub fn new(thread: &UiThread, limits: Limits) -> Self {
        Self {
            thread: thread.id(),
            limits,
            snapshot: RecordingSnapshot::default(),
            reject: None,
            _local: PhantomData,
        }
    }
    pub fn snapshot(&self, thread: &UiThread) -> Result<&RecordingSnapshot, BackendError> {
        self.check_thread(thread)?;
        Ok(&self.snapshot)
    }
    pub fn reject_next_at(&mut self, thread: &UiThread, patch: usize) -> Result<(), BackendError> {
        self.check_thread(thread)?;
        self.reject = Some(patch);
        Ok(())
    }
    pub fn set_user_state(
        &mut self,
        thread: &UiThread,
        widget: WidgetId,
        state: u64,
    ) -> Result<(), BackendError> {
        self.check_thread(thread)?;
        self.snapshot.check_session(widget.session)?;
        self.snapshot
            .widgets
            .get_mut(&widget)
            .ok_or(BackendError::StaleWidget)?
            .user_state = state;
        Ok(())
    }
    pub fn widget(
        &self,
        thread: &UiThread,
        widget: WidgetId,
    ) -> Result<&WidgetSnapshot, BackendError> {
        self.check_thread(thread)?;
        self.snapshot.check_session(widget.session)?;
        self.snapshot
            .widgets
            .get(&widget)
            .ok_or(BackendError::StaleWidget)
    }
    pub fn slot(&self, thread: &UiThread, slot: SlotId) -> Result<&Value, BackendError> {
        self.check_thread(thread)?;
        self.snapshot.check_session(slot.session)?;
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
        self.snapshot.check_session(subscription.session)?;
        Ok(self
            .snapshot
            .subscriptions
            .get(&subscription)
            .ok_or(BackendError::StaleSubscription)?
            .2)
    }
    fn check_thread(&self, thread: &UiThread) -> Result<(), BackendError> {
        thread
            .check(self.thread)
            .map_err(|_| BackendError::WrongThread)
    }
}
impl Backend for RecordingBackend {
    type Error = BackendError;
    fn apply(&mut self, thread: &UiThread, transaction: &Transaction) -> Result<(), BackendError> {
        self.check_thread(thread)?;
        self.limits
            .check()
            .map_err(|_| BackendError::Invalid("invalid recording limits"))?;
        if self
            .snapshot
            .session
            .is_some_and(|s| s != transaction.session)
        {
            return Err(BackendError::ForeignSession);
        }
        if self.snapshot.revision != transaction.base_revision
            || transaction.next_revision
                != transaction
                    .base_revision
                    .checked_add(1)
                    .ok_or(BackendError::StaleRevision)?
        {
            return Err(BackendError::StaleRevision);
        }
        if transaction.patches.len() > self.limits.max_patches {
            return Err(BackendError::Budget("patches"));
        }
        let reject = self.reject.take();
        let mut next = self.snapshot.clone();
        next.session = Some(transaction.session);
        next.last_events.clear();
        for (index, patch) in transaction.patches.iter().enumerate() {
            if reject == Some(index) {
                return Err(BackendError::Injected(index));
            }
            next.apply(patch, &self.limits)?;
        }
        if reject.is_some_and(|index| index >= transaction.patches.len()) {
            return Err(BackendError::Injected(transaction.patches.len()));
        }
        next.validate_final(&self.limits)?;
        next.revision = transaction.next_revision;
        self.snapshot = next;
        Ok(())
    }
}
impl RecordingSnapshot {
    fn check_session(&self, session: u64) -> Result<(), BackendError> {
        if self.session == Some(session) {
            Ok(())
        } else {
            Err(BackendError::ForeignSession)
        }
    }
    fn allocate(&mut self, session: u64, serial: u64) -> Result<(), BackendError> {
        self.check_session(session)?;
        if serial <= self.high_water {
            return Err(BackendError::Invalid("reused identity"));
        }
        self.high_water = serial;
        Ok(())
    }
    fn apply(&mut self, patch: &Patch, limits: &Limits) -> Result<(), BackendError> {
        use Patch::*;
        let event = match patch {
            SetRoot(root) => {
                if let Some(root) = root {
                    self.check_session(root.session)?;
                    let w = self.widgets.get(root).ok_or(BackendError::StaleWidget)?;
                    if w.parent.is_some() {
                        return Err(BackendError::Invalid("root already has a parent"));
                    }
                }
                self.root = *root;
                RecordedEvent::RootChanged(*root)
            }
            Create { widget, kind } => {
                self.allocate(widget.session, widget.serial)?;
                if self.widgets.len() >= limits.max_nodes {
                    return Err(BackendError::Budget("nodes"));
                }
                self.widgets.insert(
                    *widget,
                    WidgetSnapshot {
                        kind: *kind,
                        parent: None,
                        children: Vec::new(),
                        properties: BTreeMap::new(),
                        subscriptions: BTreeMap::new(),
                        user_state: 0,
                    },
                );
                RecordedEvent::Created(*widget)
            }
            Destroy { widget } => {
                self.check_session(widget.session)?;
                let node = self.widgets.get(widget).ok_or(BackendError::StaleWidget)?;
                if self.root == Some(*widget)
                    || node.parent.is_some()
                    || !node.children.is_empty()
                    || !node.properties.is_empty()
                    || !node.subscriptions.is_empty()
                {
                    return Err(BackendError::Invalid(
                        "destroy requires detached unsubscribed unbound widget",
                    ));
                }
                self.widgets.remove(widget);
                RecordedEvent::Destroyed(*widget)
            }
            BindSlot {
                widget,
                property,
                slot,
                value,
            } => {
                self.check_session(widget.session)?;
                self.allocate(slot.session, slot.serial)?;
                let node = self
                    .widgets
                    .get_mut(widget)
                    .ok_or(BackendError::StaleWidget)?;
                if node.properties.contains_key(property) {
                    return Err(BackendError::Invalid("property already bound"));
                }
                if node.properties.len() >= limits.max_properties {
                    return Err(BackendError::Budget("properties"));
                }
                if !crate::view::property_valid(node.kind, *property, value) {
                    return Err(BackendError::Invalid("invalid property type/kind"));
                }
                if matches!(value, Value::Text(text) if text.len() > limits.max_text_bytes) {
                    return Err(BackendError::Budget("property text"));
                }
                node.properties.insert(*property, *slot);
                self.slot_owners.insert(*slot, (*widget, *property));
                self.slots.insert(*slot, value.clone());
                RecordedEvent::SlotBound(*slot)
            }
            SetSlot { slot, value } => {
                self.check_session(slot.session)?;
                let (widget, property) =
                    self.slot_owners.get(slot).ok_or(BackendError::StaleSlot)?;
                let kind = self
                    .widgets
                    .get(widget)
                    .ok_or(BackendError::StaleWidget)?
                    .kind;
                if !crate::view::property_valid(kind, *property, value) {
                    return Err(BackendError::Invalid("invalid property type/kind"));
                }
                if matches!(value, Value::Text(text) if text.len() > limits.max_text_bytes) {
                    return Err(BackendError::Budget("property text"));
                }
                let old = self.slots.get_mut(slot).ok_or(BackendError::StaleSlot)?;
                if *old == *value {
                    return Err(BackendError::Invalid("redundant slot notification"));
                }
                *old = value.clone();
                RecordedEvent::SlotChanged(*slot)
            }
            UnbindSlot {
                widget,
                property,
                slot,
            } => {
                self.check_session(slot.session)?;
                if self.slot_owners.get(slot) != Some(&(*widget, *property)) {
                    return Err(BackendError::StaleSlot);
                }
                let node = self
                    .widgets
                    .get_mut(widget)
                    .ok_or(BackendError::StaleWidget)?;
                if node.properties.remove(property) != Some(*slot) {
                    return Err(BackendError::Invalid("slot/property mismatch"));
                }
                self.slot_owners.remove(slot);
                self.slots.remove(slot);
                RecordedEvent::SlotUnbound(*slot)
            }
            Subscribe {
                widget,
                event,
                handler,
                subscription,
            } => {
                self.check_session(widget.session)?;
                self.allocate(subscription.session, subscription.serial)?;
                let node = self
                    .widgets
                    .get_mut(widget)
                    .ok_or(BackendError::StaleWidget)?;
                if node.subscriptions.contains_key(event) {
                    return Err(BackendError::Invalid("event already subscribed"));
                }
                if node.subscriptions.len() >= limits.max_subscriptions {
                    return Err(BackendError::Budget("subscriptions"));
                }
                if !crate::view::event_valid(node.kind, *event) {
                    return Err(BackendError::Invalid("unsupported event for kind"));
                }
                node.subscriptions.insert(*event, *subscription);
                self.subscriptions
                    .insert(*subscription, (*widget, *event, *handler));
                self.created_subscriptions = self
                    .created_subscriptions
                    .checked_add(1)
                    .ok_or(BackendError::Budget("subscription counter"))?;
                RecordedEvent::Subscribed(*subscription)
            }
            Disconnect { subscription } => {
                self.check_session(subscription.session)?;
                let (widget, event, _) = self
                    .subscriptions
                    .remove(subscription)
                    .ok_or(BackendError::StaleSubscription)?;
                if self
                    .widgets
                    .get_mut(&widget)
                    .ok_or(BackendError::StaleWidget)?
                    .subscriptions
                    .remove(&event)
                    != Some(*subscription)
                {
                    return Err(BackendError::Invalid("subscription/widget mismatch"));
                }
                self.disconnected_subscriptions = self
                    .disconnected_subscriptions
                    .checked_add(1)
                    .ok_or(BackendError::Budget("disconnect counter"))?;
                RecordedEvent::Disconnected(*subscription)
            }
            RemoveChild {
                parent,
                child,
                index,
            } => {
                self.check_session(parent.session)?;
                self.check_session(child.session)?;
                let parent_node = self
                    .widgets
                    .get_mut(parent)
                    .ok_or(BackendError::StaleWidget)?;
                if parent_node.children.get(*index) != Some(child) {
                    return Err(BackendError::Invalid("remove child position"));
                }
                parent_node.children.remove(*index);
                let child_node = self
                    .widgets
                    .get_mut(child)
                    .ok_or(BackendError::StaleWidget)?;
                if child_node.parent != Some(*parent) {
                    return Err(BackendError::Invalid("child parent mismatch"));
                }
                child_node.parent = None;
                RecordedEvent::ChildRemoved {
                    parent: *parent,
                    child: *child,
                    index: *index,
                }
            }
            InsertChild {
                parent,
                child,
                index,
            } => {
                self.check_session(parent.session)?;
                self.check_session(child.session)?;
                let child_node = self.widgets.get(child).ok_or(BackendError::StaleWidget)?;
                if parent == child || self.root == Some(*child) || child_node.parent.is_some() {
                    return Err(BackendError::Invalid("insert requires detached child"));
                }
                let parent_node = self
                    .widgets
                    .get_mut(parent)
                    .ok_or(BackendError::StaleWidget)?;
                if parent_node.kind != Kind::Container || *index > parent_node.children.len() {
                    return Err(BackendError::Invalid("insert child position/kind"));
                }
                if parent_node.children.len() >= limits.max_children {
                    return Err(BackendError::Budget("children"));
                }
                parent_node.children.insert(*index, *child);
                self.widgets.get_mut(child).unwrap().parent = Some(*parent);
                RecordedEvent::ChildInserted {
                    parent: *parent,
                    child: *child,
                    index: *index,
                }
            }
            MoveChild {
                parent,
                child,
                from,
                to,
            } => {
                self.check_session(parent.session)?;
                self.check_session(child.session)?;
                let node = self
                    .widgets
                    .get_mut(parent)
                    .ok_or(BackendError::StaleWidget)?;
                if node.children.get(*from) != Some(child) || *to >= node.children.len() {
                    return Err(BackendError::Invalid("move child position"));
                }
                node.children.remove(*from);
                node.children.insert(*to, *child);
                RecordedEvent::ChildMoved {
                    parent: *parent,
                    child: *child,
                    from: *from,
                    to: *to,
                }
            }
        };
        if self.last_events.len() >= limits.max_patches {
            return Err(BackendError::Budget("event batch"));
        }
        self.last_events.push(event);
        Ok(())
    }
    fn validate_final(&self, limits: &Limits) -> Result<(), BackendError> {
        let Some(root) = self.root else {
            if self.widgets.is_empty() && self.slots.is_empty() && self.subscriptions.is_empty() {
                return Ok(());
            }
            return Err(BackendError::Invalid(
                "unmounted transaction retains widgets",
            ));
        };
        // Separate session/backend limits are permitted, so the backend must
        // independently retain enough patch budget to close its accepted tree.
        let mut teardown = 1usize
            .checked_add(self.widgets.len())
            .and_then(|n| n.checked_add(self.slots.len()))
            .and_then(|n| n.checked_add(self.subscriptions.len()))
            .ok_or(BackendError::Budget("teardown patches"))?;
        for widget in self.widgets.values() {
            teardown = teardown
                .checked_add(widget.children.len())
                .ok_or(BackendError::Budget("teardown patches"))?;
        }
        if teardown > limits.max_patches {
            return Err(BackendError::Budget("teardown patches"));
        }
        let mut total_text = 0usize;
        for value in self.slots.values() {
            if let Value::Text(text) = value {
                if text.len() > limits.max_text_bytes {
                    return Err(BackendError::Budget("property text"));
                }
                total_text = total_text
                    .checked_add(text.len())
                    .ok_or(BackendError::Budget("total text"))?;
                if total_text > limits.max_total_text_bytes {
                    return Err(BackendError::Budget("total text"));
                }
            }
        }
        let mut seen = BTreeSet::new();
        let mut stack = vec![(root, 1usize)];
        while let Some((widget, depth)) = stack.pop() {
            if depth > limits.max_depth {
                return Err(BackendError::Budget("depth"));
            }
            if !seen.insert(widget) {
                return Err(BackendError::Invalid("cycle or shared child"));
            }
            let node = self.widgets.get(&widget).ok_or(BackendError::StaleWidget)?;
            for child in &node.children {
                if self
                    .widgets
                    .get(child)
                    .ok_or(BackendError::StaleWidget)?
                    .parent
                    != Some(widget)
                {
                    return Err(BackendError::Invalid("broken parent relation"));
                }
                stack.push((*child, depth + 1));
            }
        }
        if seen.len() != self.widgets.len() {
            return Err(BackendError::Invalid("disconnected widgets"));
        }
        Ok(())
    }
}
