use crate::{
    Event, HandlerKey, Key, Kind, Limits, Property, SlotId, SubscriptionId, Value, ViewError,
    WidgetId,
};
use crate::{identity::Ids, view::Validated};
use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    rc::Rc,
};

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

#[derive(Clone, Debug)]
pub(crate) struct SlotState {
    pub id: SlotId,
    pub value: Value,
}
#[derive(Clone, Debug)]
pub(crate) struct SubscriptionState {
    pub id: SubscriptionId,
    pub handler: HandlerKey,
}
#[derive(Clone, Debug)]
pub(crate) struct NodeState {
    pub id: WidgetId,
    pub kind: Kind,
    pub properties: BTreeMap<Property, SlotState>,
    pub subscriptions: BTreeMap<Event, SubscriptionState>,
    pub children: Vec<Key>,
}
#[derive(Debug)]
pub(crate) struct Mounted {
    pub root: Key,
    pub nodes: BTreeMap<Key, NodeState>,
    pub order: Vec<Key>,
}

struct Builder {
    patches: Vec<Patch>,
    max: usize,
}
impl Builder {
    fn push(&mut self, patch: Patch) -> Result<(), ViewError> {
        if self.patches.len() >= self.max {
            return Err(ViewError::Budget("patches"));
        }
        self.patches.push(patch);
        Ok(())
    }
}

pub(crate) fn build(
    old: Option<&Mounted>,
    next: Option<(&Validated<'_>, Key)>,
    ids: &mut Ids,
    limits: &Limits,
) -> Result<(Vec<Patch>, Option<Mounted>), ViewError> {
    let mut b = Builder {
        patches: Vec::new(),
        max: limits.max_patches,
    };
    let empty = BTreeMap::new();
    let previous = old.map(|m| &m.nodes).unwrap_or(&empty);
    let retained: BTreeSet<Key> = next
        .map(|(v, _)| {
            v.nodes
                .iter()
                .filter_map(|(&key, n)| {
                    previous.get(&key).filter(|p| p.kind == n.kind).map(|_| key)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut nodes = BTreeMap::new();
    if let Some((next, _)) = next {
        for key in &next.order {
            let node = next.nodes[key];
            let previous = previous.get(key).filter(|_| retained.contains(key));
            let id = match previous {
                Some(p) => p.id,
                None => ids.widget().ok_or(ViewError::IdentityExhausted)?,
            };
            let mut properties = BTreeMap::new();
            for (&property, value) in &node.properties {
                let slot = match previous.and_then(|p| p.properties.get(&property)) {
                    Some(p) => p.id,
                    None => ids.slot().ok_or(ViewError::IdentityExhausted)?,
                };
                properties.insert(
                    property,
                    SlotState {
                        id: slot,
                        value: value.clone(),
                    },
                );
            }
            let mut subscriptions = BTreeMap::new();
            for (&event, &handler) in &node.subscriptions {
                let id = match previous
                    .and_then(|p| p.subscriptions.get(&event))
                    .filter(|p| p.handler == handler)
                {
                    Some(p) => p.id,
                    None => ids.subscription().ok_or(ViewError::IdentityExhausted)?,
                };
                subscriptions.insert(event, SubscriptionState { id, handler });
            }
            nodes.insert(
                *key,
                NodeState {
                    id,
                    kind: node.kind,
                    properties,
                    subscriptions,
                    children: node.children.clone(),
                },
            );
        }
    }
    let old_root = old.map(|m| m.nodes[&m.root].id);
    let new_root = next.map(|(_, root)| nodes[&root].id);
    if old_root.is_some() && old_root != new_root {
        b.push(Patch::SetRoot(None))?;
    }

    // First detach every edge not retained under the same parent. Remaining
    // edges are a subset of the new acyclic topology, so later inserts cannot
    // create intermediate cycles or give one widget two parents.
    let mut current_children: BTreeMap<Key, Vec<Key>> = BTreeMap::new();
    if let Some(old) = old {
        for key in &old.order {
            let node = &old.nodes[key];
            let mut children = node.children.clone();
            for i in (0..children.len()).rev() {
                let child = children[i];
                let keep = retained.contains(key)
                    && retained.contains(&child)
                    && next.is_some_and(|(v, _)| v.parents.get(&child) == Some(key));
                if !keep {
                    b.push(Patch::RemoveChild {
                        parent: node.id,
                        child: old.nodes[&child].id,
                        index: i,
                    })?;
                    children.remove(i);
                }
            }
            if retained.contains(key) {
                current_children.insert(*key, children);
            }
        }
        // Disconnect registrations before releasing their widget or properties.
        // Child-first teardown is deterministic even though detached nodes no
        // longer impose a destruction dependency.
        for key in old.order.iter().rev() {
            let node = &old.nodes[key];
            for (&event, subscription) in &node.subscriptions {
                let keep = nodes
                    .get(key)
                    .and_then(|n| n.subscriptions.get(&event))
                    .is_some_and(|next| next.id == subscription.id);
                if !keep {
                    b.push(Patch::Disconnect {
                        subscription: subscription.id,
                    })?;
                }
            }
            for (&property, slot) in &node.properties {
                let keep = nodes
                    .get(key)
                    .and_then(|n| n.properties.get(&property))
                    .is_some_and(|n| n.id == slot.id);
                if !keep {
                    b.push(Patch::UnbindSlot {
                        widget: node.id,
                        property,
                        slot: slot.id,
                    })?;
                }
            }
            if !retained.contains(key) {
                b.push(Patch::Destroy { widget: node.id })?;
            }
        }
    }
    if let Some((next, root)) = next {
        for key in &next.order {
            let node = &nodes[key];
            if !retained.contains(key) {
                b.push(Patch::Create {
                    widget: node.id,
                    kind: node.kind,
                })?;
            }
            let previous = previous.get(key).filter(|_| retained.contains(key));
            for (&property, slot) in &node.properties {
                match previous.and_then(|p| p.properties.get(&property)) {
                    Some(p) if p.value != slot.value => b.push(Patch::SetSlot {
                        slot: slot.id,
                        value: slot.value.clone(),
                    })?,
                    Some(_) => {}
                    None => b.push(Patch::BindSlot {
                        widget: node.id,
                        property,
                        slot: slot.id,
                        value: slot.value.clone(),
                    })?,
                }
            }
            for (&event, subscription) in &node.subscriptions {
                let keep = previous
                    .and_then(|p| p.subscriptions.get(&event))
                    .is_some_and(|s| s.id == subscription.id);
                if !keep {
                    b.push(Patch::Subscribe {
                        widget: node.id,
                        event,
                        handler: subscription.handler,
                        subscription: subscription.id,
                    })?;
                }
            }
        }
        for key in &next.order {
            let node = &nodes[key];
            let mut children = current_children.remove(key).unwrap_or_default();
            for (index, &child) in node.children.iter().enumerate() {
                if children.get(index) == Some(&child) {
                    continue;
                }
                if let Some(from) = children.iter().position(|c| *c == child) {
                    b.push(Patch::MoveChild {
                        parent: node.id,
                        child: nodes[&child].id,
                        from,
                        to: index,
                    })?;
                    children.remove(from);
                    children.insert(index, child);
                } else {
                    b.push(Patch::InsertChild {
                        parent: node.id,
                        child: nodes[&child].id,
                        index,
                    })?;
                    children.insert(index, child);
                }
            }
            debug_assert_eq!(children, node.children);
        }
        if new_root != old_root {
            b.push(Patch::SetRoot(new_root))?;
        }
        Ok((
            b.patches,
            Some(Mounted {
                root,
                nodes,
                order: next.order.clone(),
            }),
        ))
    } else {
        Ok((b.patches, None))
    }
}
