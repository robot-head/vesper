use crate::{Event, HandlerKey, Kind, Property, SlotId, SubscriptionId, Value, WidgetId};
use std::collections::BTreeMap;

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
    pub(crate) session: Option<u64>,
    pub(crate) high_water: u64,
    pub(crate) slot_owners: BTreeMap<SlotId, (WidgetId, Property)>,
}
