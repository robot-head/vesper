//! The wire between the public Rust types and the Loom package in vesper/: trees,
//! patches and limits become words plus one byte buffer of text, and the
//! library's faults, plans and recorded state become the crate's types
//! again. Nothing here decides anything; the codes are listed at the top of
//! the Loom package in vesper/.

use super::binding::{self, BackendFault, Op, RStateRef, ViewFault};
use crate::{
    BackendError, Event, HandlerKey, Key, Kind, Limits, Patch, Property, RecordedEvent,
    RecordingSnapshot, SlotId, SubscriptionId, Value, ViewError, ViewTree, WidgetId,
    WidgetSnapshot,
};
use std::collections::BTreeMap;

pub(crate) fn limits(limits: &Limits) -> [u64; 8] {
    [
        limits.max_nodes,
        limits.max_children,
        limits.max_depth,
        limits.max_properties,
        limits.max_subscriptions,
        limits.max_text_bytes,
        limits.max_total_text_bytes,
        limits.max_patches,
    ]
    .map(|n| n as u64)
}

pub(crate) fn kind(kind: Kind) -> u64 {
    match kind {
        Kind::Container => 0,
        Kind::Text => 1,
        Kind::Button => 2,
        Kind::Input => 3,
    }
}

pub(crate) fn kind_of(code: u64) -> Kind {
    match code {
        0 => Kind::Container,
        1 => Kind::Text,
        2 => Kind::Button,
        _ => Kind::Input,
    }
}

pub(crate) fn property(property: Property) -> u64 {
    match property {
        Property::Text => 0,
        Property::Value => 1,
        Property::Enabled => 2,
        Property::Visible => 3,
        Property::Width => 4,
    }
}

pub(crate) fn property_of(code: u64) -> Property {
    match code {
        0 => Property::Text,
        1 => Property::Value,
        2 => Property::Enabled,
        3 => Property::Visible,
        _ => Property::Width,
    }
}

pub(crate) fn event(event: Event) -> u64 {
    match event {
        Event::Click => 0,
        Event::Changed => 1,
        Event::Focused => 2,
    }
}

pub(crate) fn event_of(code: u64) -> Event {
    match code {
        0 => Event::Click,
        1 => Event::Changed,
        _ => Event::Focused,
    }
}

/// A value as (tag, payload, text start, text length), appending its text.
fn value(value: &Value, text: &mut Vec<u8>) -> [u64; 4] {
    match value {
        Value::Text(s) => {
            let start = text.len() as u64;
            text.extend_from_slice(s.as_bytes());
            [0, 0, start, s.len() as u64]
        }
        Value::Bool(b) => [1, u64::from(*b), 0, 0],
        Value::Int(n) => [2, *n as u64, 0, 0],
    }
}

fn value_of(tag: u64, payload: u64, text: &[u8]) -> Value {
    match tag {
        0 => Value::Text(String::from_utf8_lossy(text).into_owned()),
        1 => Value::Bool(payload != 0),
        _ => Value::Int(payload as i64),
    }
}

/// The tree in the layout `decode` in the Loom package in vesper/ reads.
pub(crate) fn tree(tree: &ViewTree) -> (Vec<u64>, Vec<u8>) {
    let mut words = vec![tree.root.0, tree.nodes.len() as u64];
    let mut text = Vec::new();
    for node in &tree.nodes {
        words.extend([
            node.key.0,
            kind(node.kind),
            node.properties.len() as u64,
            node.subscriptions.len() as u64,
            node.children.len() as u64,
        ]);
        for (&name, v) in &node.properties {
            words.push(property(name));
            words.extend(value(v, &mut text));
        }
        for (&e, handler) in &node.subscriptions {
            words.extend([event(e), handler.0]);
        }
        words.extend(node.children.iter().map(|k| k.0));
    }
    (words, text)
}

fn budget(code: u64) -> &'static str {
    match code {
        0 => "nodes",
        1 => "teardown patches",
        2 => "children",
        3 => "properties",
        4 => "subscriptions",
        5 => "property text",
        6 => "total text",
        7 => "depth",
        8 => "patches",
        9 => "slots",
        10 => "handler",
        11 => "event batch",
        12 => "subscription counter",
        _ => "disconnect counter",
    }
}

fn invalid(code: u64) -> &'static str {
    match code {
        0 => "invalid recording limits",
        1 => "reused identity",
        2 => "root already has a parent",
        3 => "destroy requires detached unsubscribed unbound widget",
        4 => "property already bound",
        5 => "invalid property type/kind",
        6 => "redundant slot notification",
        7 => "slot/property mismatch",
        8 => "event already subscribed",
        9 => "unsupported event for kind",
        10 => "subscription/widget mismatch",
        11 => "remove child position",
        12 => "child parent mismatch",
        13 => "insert requires detached child",
        14 => "insert child position/kind",
        15 => "move child position",
        16 => "unmounted transaction retains widgets",
        17 => "cycle or shared child",
        18 => "broken parent relation",
        _ => "disconnected widgets",
    }
}

pub(crate) fn view_error(fault: ViewFault) -> ViewError {
    match fault {
        ViewFault::InvalidLimits => ViewError::InvalidLimits,
        ViewFault::Budget(code) => ViewError::Budget(budget(code)),
        ViewFault::DuplicateKey(k) => ViewError::DuplicateKey(Key(k)),
        ViewFault::MissingRoot(k) => ViewError::MissingRoot(Key(k)),
        ViewFault::MissingChild { parent, child } => ViewError::MissingChild {
            parent: Key(parent),
            child: Key(child),
        },
        ViewFault::MultipleParents(k) => ViewError::MultipleParents(Key(k)),
        ViewFault::RootHasParent => ViewError::RootHasParent,
        ViewFault::CycleOrDisconnected => ViewError::CycleOrDisconnected,
        ViewFault::ChildrenOnLeaf(k) => ViewError::ChildrenOnLeaf(Key(k)),
        ViewFault::PropertyType { key, property } => ViewError::PropertyType {
            key: Key(key),
            property: property_of(property),
        },
        ViewFault::UnsupportedEvent { key, event } => ViewError::UnsupportedEvent {
            key: Key(key),
            event: event_of(event),
        },
        ViewFault::IdentityExhausted => ViewError::IdentityExhausted,
        ViewFault::Closed => ViewError::Closed,
    }
}

pub(crate) fn backend_error(fault: BackendFault) -> BackendError {
    match fault {
        BackendFault::ForeignSession => BackendError::ForeignSession,
        BackendFault::StaleRevision => BackendError::StaleRevision,
        BackendFault::StaleWidget => BackendError::StaleWidget,
        BackendFault::StaleSlot => BackendError::StaleSlot,
        BackendFault::StaleSubscription => BackendError::StaleSubscription,
        BackendFault::Invalid(code) => BackendError::Invalid(invalid(code)),
        BackendFault::Budget(code) => BackendError::Budget(budget(code)),
        BackendFault::Injected(i) => BackendError::Injected(i as usize),
    }
}

/// The plan's patches; slot values are cloned from the input tree.
pub(crate) fn patches(session: u64, ops: &[Op], input: Option<&ViewTree>) -> Vec<Patch> {
    let widget = |serial| WidgetId { session, serial };
    let slot = |serial| SlotId { session, serial };
    let value_at = |node: u64, name: u64| -> Value {
        input
            .and_then(|t| t.nodes.get(node as usize))
            .and_then(|n| n.properties.get(&property_of(name)))
            .cloned()
            .unwrap_or(Value::Bool(false))
    };
    ops.iter()
        .map(|o| match o.tag {
            0 => Patch::SetRoot((o.a == 1).then(|| widget(o.b))),
            1 => Patch::Create {
                widget: widget(o.a),
                kind: kind_of(o.b),
            },
            2 => Patch::Destroy {
                widget: widget(o.a),
            },
            3 => Patch::BindSlot {
                widget: widget(o.a),
                property: property_of(o.b),
                slot: slot(o.c),
                value: value_at(o.d, o.b),
            },
            4 => Patch::SetSlot {
                slot: slot(o.a),
                value: value_at(o.d, o.b),
            },
            5 => Patch::UnbindSlot {
                widget: widget(o.a),
                property: property_of(o.b),
                slot: slot(o.c),
            },
            6 => Patch::Subscribe {
                widget: widget(o.a),
                event: event_of(o.b),
                handler: HandlerKey(o.c),
                subscription: SubscriptionId {
                    session,
                    serial: o.d,
                },
            },
            7 => Patch::Disconnect {
                subscription: SubscriptionId {
                    session,
                    serial: o.a,
                },
            },
            8 => Patch::RemoveChild {
                parent: widget(o.a),
                child: widget(o.b),
                index: o.c as usize,
            },
            9 => Patch::InsertChild {
                parent: widget(o.a),
                child: widget(o.b),
                index: o.c as usize,
            },
            _ => Patch::MoveChild {
                parent: widget(o.a),
                child: widget(o.b),
                from: o.c as usize,
                to: o.d as usize,
            },
        })
        .collect()
}

/// Ten words per patch (see `apply_patch` in the Loom package in vesper/): the tag, and
/// every identity as its session and serial.
pub(crate) fn transaction(patches: &[Patch]) -> (Vec<u64>, Vec<u8>) {
    let mut words = Vec::with_capacity(patches.len() * 10);
    let mut text = Vec::new();
    for patch in patches {
        let mut w = [0u64; 10];
        match patch {
            Patch::SetRoot(root) => {
                w[0] = 0;
                if let Some(r) = root {
                    w[1..4].copy_from_slice(&[1, r.session, r.serial]);
                }
            }
            Patch::Create { widget, kind: k } => {
                w[..4].copy_from_slice(&[1, widget.session, widget.serial, kind(*k)]);
            }
            Patch::Destroy { widget } => {
                w[..3].copy_from_slice(&[2, widget.session, widget.serial]);
            }
            Patch::BindSlot {
                widget,
                property: p,
                slot,
                value: v,
            } => {
                w[..6].copy_from_slice(&[
                    3,
                    widget.session,
                    widget.serial,
                    property(*p),
                    slot.session,
                    slot.serial,
                ]);
                w[6..].copy_from_slice(&value(v, &mut text));
            }
            Patch::SetSlot { slot, value: v } => {
                w[..3].copy_from_slice(&[4, slot.session, slot.serial]);
                w[3..7].copy_from_slice(&value(v, &mut text));
            }
            Patch::UnbindSlot {
                widget,
                property: p,
                slot,
            } => {
                w[..6].copy_from_slice(&[
                    5,
                    widget.session,
                    widget.serial,
                    property(*p),
                    slot.session,
                    slot.serial,
                ]);
            }
            Patch::Subscribe {
                widget,
                event: e,
                handler,
                subscription,
            } => {
                w[..7].copy_from_slice(&[
                    6,
                    widget.session,
                    widget.serial,
                    event(*e),
                    handler.0,
                    subscription.session,
                    subscription.serial,
                ]);
            }
            Patch::Disconnect { subscription } => {
                w[..3].copy_from_slice(&[7, subscription.session, subscription.serial]);
            }
            Patch::RemoveChild {
                parent,
                child,
                index,
            }
            | Patch::InsertChild {
                parent,
                child,
                index,
            } => {
                let tag = if matches!(patch, Patch::RemoveChild { .. }) {
                    8
                } else {
                    9
                };
                w[..6].copy_from_slice(&[
                    tag,
                    parent.session,
                    parent.serial,
                    child.session,
                    child.serial,
                    *index as u64,
                ]);
            }
            Patch::MoveChild {
                parent,
                child,
                from,
                to,
            } => {
                w[..7].copy_from_slice(&[
                    10,
                    parent.session,
                    parent.serial,
                    child.session,
                    child.serial,
                    *from as u64,
                    *to as u64,
                ]);
            }
        }
        words.extend(w);
    }
    (words, text)
}

/// The published recording state as the crate's snapshot type.
pub(crate) fn snapshot(st: RStateRef<'_>) -> RecordingSnapshot {
    let session = st.session();
    let widget = |serial| WidgetId { session, serial };
    let text = st.text().to_vec();
    let mut children: BTreeMap<u64, Vec<WidgetId>> = BTreeMap::new();
    for e in st.edges().iter() {
        children.entry(e.parent).or_default().push(widget(e.child));
    }
    let mut properties: BTreeMap<u64, BTreeMap<Property, SlotId>> = BTreeMap::new();
    let mut slots = BTreeMap::new();
    let mut slot_owners = BTreeMap::new();
    let slive = st.slive();
    for (i, s) in st.slots().iter().enumerate() {
        if slive.get(i) != Some(true) {
            continue;
        }
        let id = SlotId {
            session,
            serial: s.serial,
        };
        let name = property_of(s.property);
        properties.entry(s.widget).or_default().insert(name, id);
        let bytes = text
            .get(s.tstart as usize..(s.tstart + s.tlen) as usize)
            .unwrap_or_default();
        slots.insert(id, value_of(s.tag, s.payload, bytes));
        slot_owners.insert(id, (widget(s.widget), name));
    }
    let mut subscribed: BTreeMap<u64, BTreeMap<Event, SubscriptionId>> = BTreeMap::new();
    let mut subscriptions = BTreeMap::new();
    let blive = st.blive();
    for (i, s) in st.subs().iter().enumerate() {
        if blive.get(i) != Some(true) {
            continue;
        }
        let id = SubscriptionId {
            session,
            serial: s.serial,
        };
        subscribed
            .entry(s.widget)
            .or_default()
            .insert(event_of(s.event), id);
        subscriptions.insert(
            id,
            (widget(s.widget), event_of(s.event), HandlerKey(s.handler)),
        );
    }
    let mut widgets = BTreeMap::new();
    let wlive = st.wlive();
    for (i, w) in st.widgets().iter().enumerate() {
        if wlive.get(i) != Some(true) {
            continue;
        }
        widgets.insert(
            widget(w.serial),
            WidgetSnapshot {
                kind: kind_of(w.kind),
                parent: w.has_parent.then(|| widget(w.parent)),
                children: children.remove(&w.serial).unwrap_or_default(),
                properties: properties.remove(&w.serial).unwrap_or_default(),
                subscriptions: subscribed.remove(&w.serial).unwrap_or_default(),
                user_state: w.user_state,
            },
        );
    }
    let last_events = st.events().iter().map(|o| recorded(session, &o)).collect();
    RecordingSnapshot {
        revision: st.revision(),
        root: st.has_root().then(|| widget(st.root())),
        widgets,
        slots,
        subscriptions,
        last_events,
        created_subscriptions: st.created(),
        disconnected_subscriptions: st.disconnected(),
        session: st.has_session().then_some(session),
        high_water: st.high_water(),
        slot_owners,
    }
}

fn recorded(session: u64, o: &binding::Op) -> RecordedEvent {
    let widget = |serial| WidgetId { session, serial };
    let slot = |serial| SlotId { session, serial };
    let sub = |serial| SubscriptionId { session, serial };
    match o.tag {
        0 => RecordedEvent::RootChanged((o.a == 1).then(|| widget(o.b))),
        1 => RecordedEvent::Created(widget(o.a)),
        2 => RecordedEvent::Destroyed(widget(o.a)),
        3 => RecordedEvent::SlotBound(slot(o.a)),
        4 => RecordedEvent::SlotChanged(slot(o.a)),
        5 => RecordedEvent::SlotUnbound(slot(o.a)),
        6 => RecordedEvent::Subscribed(sub(o.a)),
        7 => RecordedEvent::Disconnected(sub(o.a)),
        8 => RecordedEvent::ChildRemoved {
            parent: widget(o.a),
            child: widget(o.b),
            index: o.c as usize,
        },
        9 => RecordedEvent::ChildInserted {
            parent: widget(o.a),
            child: widget(o.b),
            index: o.c as usize,
        },
        _ => RecordedEvent::ChildMoved {
            parent: widget(o.a),
            child: widget(o.b),
            from: o.c as usize,
            to: o.d as usize,
        },
    }
}
