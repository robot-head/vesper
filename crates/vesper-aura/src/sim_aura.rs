//! Cross-platform Aura topology encoding, slot storage, and headless `SimAura`
//! WCAG 2.2 AA simulation backend (design §37.6).

use crate::{BackendError, Event, HandlerKey, Key, Kind, Property, Value, WidgetId};

/// Native or simulated UI backend target supported by Vesper Aura (§37.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeUiBackend {
    /// Windows 10/11: Windows App SDK 2.x COM + DirectComposition.
    WinUi3,
    /// Linux/BSD: libgtk-4 + libadwaita + Vulkan GSK + Wayland.
    Gtk4,
    /// macOS: NSHostingView + Swift 6 ObservationRegistrar Slot Store.
    SwiftUi,
    /// Headless deterministic test & WCAG accessibility backend.
    SimAura,
}

/// Layout bitflags packed into `NodeOp::layout_flags`.
pub mod layout_flags {
    pub const VISIBLE: u32 = 1 << 0;
    pub const ENABLED: u32 = 1 << 1;
    pub const INTERACTIVE: u32 = 1 << 2;
    pub const HAS_ACCESSIBLE_LABEL: u32 = 1 << 3;
}

/// Padding-free `wire struct` topology instruction shared across native slot
/// bridges and `SimAura` snapshots (16 bytes, zero implicit padding).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeOp {
    pub kind_tag: u16,
    pub child_count: u16,
    pub prop_slot_base: u32,
    pub event_action_id: u32,
    pub layout_flags: u32,
}

const _: () = {
    assert!(core::mem::size_of::<NodeOp>() == 16);
    assert!(core::mem::align_of::<NodeOp>() == 4);
};

impl NodeOp {
    pub const fn kind_tag_of(kind: Kind) -> u16 {
        match kind {
            Kind::Container => 0,
            Kind::Text => 1,
            Kind::Button => 2,
            Kind::Input => 3,
        }
    }

    pub const fn slot_count(kind: Kind) -> u32 {
        match kind {
            Kind::Container => 3,
            Kind::Text | Kind::Button => 4,
            Kind::Input => 5,
        }
    }

    pub const fn property_offset(kind: Kind, property: Property) -> Option<u32> {
        match (kind, property) {
            (Kind::Container, Property::Enabled) => Some(0),
            (Kind::Container, Property::Visible) => Some(1),
            (Kind::Container, Property::Width) => Some(2),
            (Kind::Text | Kind::Button, Property::Text) => Some(0),
            (Kind::Text | Kind::Button, Property::Enabled) => Some(1),
            (Kind::Text | Kind::Button, Property::Visible) => Some(2),
            (Kind::Text | Kind::Button, Property::Width) => Some(3),
            (Kind::Input, Property::Text) => Some(0),
            (Kind::Input, Property::Value) => Some(1),
            (Kind::Input, Property::Enabled) => Some(2),
            (Kind::Input, Property::Visible) => Some(3),
            (Kind::Input, Property::Width) => Some(4),
            _ => None,
        }
    }

    pub fn property_slot(&self, kind: Kind, property: Property) -> Option<u32> {
        Self::property_offset(kind, property).and_then(|off| self.prop_slot_base.checked_add(off))
    }
}

/// Typed reactive property slot stored inside [`AuraSlotTable`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotValue {
    Unset,
    Int(i64),
    Bool(bool),
    Text(String),
}

impl From<&Value> for SlotValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Int(n) => Self::Int(*n),
            Value::Bool(b) => Self::Bool(*b),
            Value::Text(s) => Self::Text(s.clone()),
        }
    }
}

/// Reactive slot store queueing O(1) dirty notifications only on value changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuraSlotTable {
    pub values: Vec<SlotValue>,
    pub properties: Vec<Property>,
    pub dirty_slots: Vec<u32>,
}

impl AuraSlotTable {
    pub fn with_values(values: Vec<SlotValue>) -> Self {
        Self {
            values,
            properties: Vec::new(),
            dirty_slots: Vec::new(),
        }
    }

    pub fn with_slots(values: Vec<SlotValue>, properties: Vec<Property>) -> Self {
        Self {
            values,
            properties,
            dirty_slots: Vec::new(),
        }
    }

    pub fn property(&self, slot_id: u32) -> Option<Property> {
        self.properties.get(slot_id as usize).copied()
    }

    /// Updates a reactive slot and queues a dirty entry if and only if the value
    /// changed and `slot_id` is in range.
    #[inline]
    pub fn set_slot(&mut self, slot_id: u32, next: SlotValue) -> bool {
        let Some(entry) = self.values.get_mut(slot_id as usize) else {
            return false;
        };
        if *entry != next {
            *entry = next;
            self.dirty_slots.push(slot_id);
            true
        } else {
            false
        }
    }

    pub fn drain_dirty(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.dirty_slots)
    }
}

/// Node identifier reported in WCAG 2.2 AA audit findings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuditTarget {
    TreeKey(Key),
    MountedWidget(WidgetId),
}

/// Specific accessibility violation detected on a semantic Aura tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WcagViolation {
    /// Interactive control (`Button` or `Input`) has no non-whitespace accessible label.
    MissingAccessibleLabel { target: AuditTarget, kind: Kind },
    /// Interactive control subscribes to an event handler while disabled or hidden,
    /// or a tree has duplicate accessible labels among visible buttons.
    HiddenSubscribedControl { target: AuditTarget, kind: Kind },
}

/// Report produced by a headless WCAG 4.1.2 accessibility check.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WcagReport {
    pub inspected_nodes: usize,
    pub interactive_controls: usize,
    pub violations: Vec<WcagViolation>,
}

impl WcagReport {
    pub fn is_compliant(&self) -> bool {
        self.violations.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimAuraError {
    Backend(BackendError),
    WcagRejected(WcagReport),
    NoFocusableWidgets,
    WidgetNotFocusable(WidgetId),
    NotSubscribed { widget: WidgetId, event: Event },
}

impl From<BackendError> for SimAuraError {
    fn from(err: BackendError) -> Self {
        Self::Backend(err)
    }
}

/// Dispatched simulated UI event recorded by [`SimAura`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimulatedEvent {
    pub widget: WidgetId,
    pub event: Event,
    pub handler: HandlerKey,
}
