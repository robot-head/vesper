use crate::{Event, HandlerKey, Kind, Property, SlotId, SubscriptionId, Value, WidgetId};
use std::{marker::PhantomData, rc::Rc};

/// Retained-backend operations, in a deterministic safe application order.
///
/// `SetSlot` updates the one bound property and schedules exactly that slot's
/// change notification after successful commit. `BindSlot` is initialization,
/// not a change notification. Detach/reorder does not destroy widget state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Patch {
    SetRoot(Option<WidgetId>),
    Create {
        widget: WidgetId,
        kind: Kind,
    },
    Destroy {
        widget: WidgetId,
    },
    BindSlot {
        widget: WidgetId,
        property: Property,
        slot: SlotId,
        value: Value,
    },
    SetSlot {
        slot: SlotId,
        value: Value,
    },
    UnbindSlot {
        widget: WidgetId,
        property: Property,
        slot: SlotId,
    },
    Subscribe {
        widget: WidgetId,
        event: Event,
        handler: HandlerKey,
        subscription: SubscriptionId,
    },
    Disconnect {
        subscription: SubscriptionId,
    },
    RemoveChild {
        parent: WidgetId,
        child: WidgetId,
        index: usize,
    },
    InsertChild {
        parent: WidgetId,
        child: WidgetId,
        index: usize,
    },
    /// `to` is the final index after removing `from` from the same child list.
    MoveChild {
        parent: WidgetId,
        child: WidgetId,
        from: usize,
        to: usize,
    },
}

/// Immutable plan. Only a Session can construct transactions; backends can
/// inspect them. Widget/slot IDs are process-local opaque values, not an FFI ABI.
/// The plan itself is thread-confined, including shared borrows of it.
///
/// ```compile_fail
/// use vesper_aura::Transaction;
/// fn send<T: Send>() {}
/// send::<&Transaction>();
/// ```
#[derive(Debug)]
pub struct Transaction {
    pub(crate) session: u64,
    pub(crate) base_revision: u64,
    pub(crate) next_revision: u64,
    pub(crate) patches: Vec<Patch>,
    pub(crate) _local: PhantomData<Rc<()>>,
}
impl Transaction {
    pub fn patches(&self) -> &[Patch] {
        &self.patches
    }
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }
    pub fn next_revision(&self) -> u64 {
        self.next_revision
    }
}
