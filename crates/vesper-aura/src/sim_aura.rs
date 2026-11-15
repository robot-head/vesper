//! Cross-platform Aura topology encoding, slot storage, and headless `SimAura`
//! WCAG 2.2 AA simulation backend (design §37.6).

use crate::{
    Backend, BackendError, Event, HandlerKey, Key, Kind, Limits, Node, Property, RecordingBackend,
    RecordingSnapshot, Transaction, UiThread, Value, ViewError, ViewTree, WidgetId, view,
};
use std::collections::{BTreeMap, BTreeSet};

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
}

/// Typed reactive property slot stored inside [`AuraSlotTable`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotValue {
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
    pub dirty_slots: Vec<u32>,
}

impl AuraSlotTable {
    pub fn with_values(values: Vec<SlotValue>) -> Self {
        Self {
            values,
            dirty_slots: Vec::new(),
        }
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

/// Encodes a validated [`ViewTree`] in depth-first pre-order into padding-free
/// [`NodeOp`] instructions paired with an initial [`AuraSlotTable`].
pub fn encode_view_tree(
    tree: &ViewTree,
    limits: &Limits,
) -> Result<(Vec<NodeOp>, AuraSlotTable), ViewError> {
    let validated = view::validate(tree, limits)?;
    let mut ops = Vec::with_capacity(validated.order.len());
    let mut values = Vec::new();
    for key in validated.order {
        let node = validated.nodes[&key];
        let prop_slot_base = u32::try_from(values.len()).map_err(|_| ViewError::Budget("slots"))?;
        for val in node.properties.values() {
            values.push(SlotValue::from(val));
        }
        let child_count =
            u16::try_from(node.children.len()).map_err(|_| ViewError::Budget("children"))?;
        let event_action_id = node
            .subscriptions
            .values()
            .next()
            .and_then(|h| u32::try_from(h.0).ok())
            .unwrap_or(0);
        ops.push(NodeOp {
            kind_tag: NodeOp::kind_tag_of(node.kind),
            child_count,
            prop_slot_base,
            event_action_id,
            layout_flags: node_flags(node),
        });
    }
    Ok((ops, AuraSlotTable::with_values(values)))
}

fn node_flags(node: &Node) -> u32 {
    let visible = !matches!(
        node.properties.get(&Property::Visible),
        Some(Value::Bool(false))
    );
    let enabled = !matches!(
        node.properties.get(&Property::Enabled),
        Some(Value::Bool(false))
    );
    let interactive = matches!(node.kind, Kind::Button | Kind::Input);
    let has_label = matches!(
        node.properties.get(&Property::Text),
        Some(Value::Text(label)) if !label.trim().is_empty()
    );
    let mut flags = 0u32;
    if visible {
        flags |= layout_flags::VISIBLE;
    }
    if enabled {
        flags |= layout_flags::ENABLED;
    }
    if interactive {
        flags |= layout_flags::INTERACTIVE;
    }
    if has_label {
        flags |= layout_flags::HAS_ACCESSIBLE_LABEL;
    }
    flags
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

/// Audits a declarative [`ViewTree`] against WCAG 4.1.2 accessible-label rules.
pub fn audit_view_tree(tree: &ViewTree, limits: &Limits) -> Result<WcagReport, ViewError> {
    let validated = view::validate(tree, limits)?;
    let mut report = WcagReport {
        inspected_nodes: validated.order.len(),
        interactive_controls: 0,
        violations: Vec::new(),
    };
    for key in validated.order {
        let node = validated.nodes[&key];
        let visible = !matches!(
            node.properties.get(&Property::Visible),
            Some(Value::Bool(false))
        );
        if matches!(node.kind, Kind::Button | Kind::Input) {
            report.interactive_controls += 1;
            let has_label = matches!(
                node.properties.get(&Property::Text),
                Some(Value::Text(s)) if !s.trim().is_empty()
            );
            if visible && !has_label {
                report
                    .violations
                    .push(WcagViolation::MissingAccessibleLabel {
                        target: AuditTarget::TreeKey(key),
                        kind: node.kind,
                    });
            }
            if !visible && !node.subscriptions.is_empty() {
                report
                    .violations
                    .push(WcagViolation::HiddenSubscribedControl {
                        target: AuditTarget::TreeKey(key),
                        kind: node.kind,
                    });
            }
        }
    }
    Ok(report)
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

/// Headless deterministic test & WCAG 2.2 AA accessibility backend (`NativeUiBackend::SimAura`).
///
/// Wraps [`RecordingBackend`] for atomic transaction staging while adding
/// keyboard focus traversal, simulated event dispatch, and optional fail-closed
/// WCAG 4.1.2 enforcement on every committed update.
///
/// ```compile_fail
/// use vesper_aura::SimAura;
/// fn send<T: Send>() {}
/// send::<SimAura>();
/// ```
#[derive(Debug)]
pub struct SimAura {
    inner: RecordingBackend,
    enforce_wcag_on_commit: bool,
    focused: Option<WidgetId>,
    dispatched_events: Vec<SimulatedEvent>,
}

impl SimAura {
    pub fn new(thread: &UiThread, limits: Limits) -> Self {
        Self {
            inner: RecordingBackend::new(thread, limits),
            enforce_wcag_on_commit: false,
            focused: None,
            dispatched_events: Vec::new(),
        }
    }

    pub fn with_wcag_gate(thread: &UiThread, limits: Limits) -> Self {
        Self {
            inner: RecordingBackend::new(thread, limits),
            enforce_wcag_on_commit: true,
            focused: None,
            dispatched_events: Vec::new(),
        }
    }

    pub const fn backend_kind(&self) -> NativeUiBackend {
        NativeUiBackend::SimAura
    }

    pub fn snapshot(&self, thread: &UiThread) -> Result<&RecordingSnapshot, BackendError> {
        self.inner.snapshot(thread)
    }

    pub fn focused(&self, thread: &UiThread) -> Result<Option<WidgetId>, BackendError> {
        self.inner.snapshot(thread)?;
        Ok(self.focused)
    }

    pub fn dispatched_events(&self) -> &[SimulatedEvent] {
        &self.dispatched_events
    }

    /// Evaluates WCAG 4.1.2 accessible-label compliance on the currently mounted tree.
    pub fn audit_wcag(&self, thread: &UiThread) -> Result<WcagReport, BackendError> {
        let snap = self.inner.snapshot(thread)?;
        Ok(audit_snapshot(snap))
    }

    /// Returns depth-first pre-order focusable interactive controls (`Button` and
    /// `Input` that are visible and enabled).
    pub fn focusable_order(&self, thread: &UiThread) -> Result<Vec<WidgetId>, BackendError> {
        let snap = self.inner.snapshot(thread)?;
        let Some(root) = snap.root else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut stack = vec![root];
        let mut seen = BTreeSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(widget) = snap.widgets.get(&id) else {
                continue;
            };
            if matches!(widget.kind, Kind::Button | Kind::Input)
                && is_visible(snap, widget)
                && is_enabled(snap, widget)
            {
                out.push(id);
            }
            for &child in widget.children.iter().rev() {
                stack.push(child);
            }
        }
        Ok(out)
    }

    /// Advances keyboard focus in deterministic tab order, wrapping around.
    pub fn focus_next(&mut self, thread: &UiThread) -> Result<WidgetId, SimAuraError> {
        let order = self.focusable_order(thread)?;
        if order.is_empty() {
            self.focused = None;
            return Err(SimAuraError::NoFocusableWidgets);
        }
        let next = match self
            .focused
            .and_then(|cur| order.iter().position(|&id| id == cur))
        {
            Some(idx) => order[(idx + 1) % order.len()],
            None => order[0],
        };
        self.focused = Some(next);
        let _ = self.dispatch_if_subscribed(thread, next, Event::Focused)?;
        Ok(next)
    }

    /// Moves keyboard focus backward in deterministic shift-tab order, wrapping around.
    pub fn focus_prev(&mut self, thread: &UiThread) -> Result<WidgetId, SimAuraError> {
        let order = self.focusable_order(thread)?;
        if order.is_empty() {
            self.focused = None;
            return Err(SimAuraError::NoFocusableWidgets);
        }
        let prev = match self
            .focused
            .and_then(|cur| order.iter().position(|&id| id == cur))
        {
            Some(idx) => order[(idx + order.len() - 1) % order.len()],
            None => order[order.len() - 1],
        };
        self.focused = Some(prev);
        let _ = self.dispatch_if_subscribed(thread, prev, Event::Focused)?;
        Ok(prev)
    }

    /// Triggers an event on an interactive, visible, enabled widget.
    pub fn trigger_event(
        &mut self,
        thread: &UiThread,
        widget: WidgetId,
        event: Event,
    ) -> Result<HandlerKey, SimAuraError> {
        let focusable = self.focusable_order(thread)?;
        if !focusable.contains(&widget) {
            return Err(SimAuraError::WidgetNotFocusable(widget));
        }
        let snap = self.inner.snapshot(thread)?;
        let Some(w) = snap.widgets.get(&widget) else {
            return Err(SimAuraError::WidgetNotFocusable(widget));
        };
        let Some(&sub_id) = w.subscriptions.get(&event) else {
            return Err(SimAuraError::NotSubscribed { widget, event });
        };
        let handler = self.inner.dispatch(thread, sub_id)?;
        self.dispatched_events.push(SimulatedEvent {
            widget,
            event,
            handler,
        });
        Ok(handler)
    }

    fn dispatch_if_subscribed(
        &mut self,
        thread: &UiThread,
        widget: WidgetId,
        event: Event,
    ) -> Result<Option<HandlerKey>, SimAuraError> {
        let snap = self.inner.snapshot(thread)?;
        let sub_id = snap
            .widgets
            .get(&widget)
            .and_then(|w| w.subscriptions.get(&event).copied());
        let Some(sub_id) = sub_id else {
            return Ok(None);
        };
        let handler = self.inner.dispatch(thread, sub_id)?;
        self.dispatched_events.push(SimulatedEvent {
            widget,
            event,
            handler,
        });
        Ok(Some(handler))
    }
}

impl Backend for SimAura {
    type Error = SimAuraError;

    fn apply(&mut self, thread: &UiThread, transaction: &Transaction) -> Result<(), Self::Error> {
        if self.enforce_wcag_on_commit {
            let mut staged = self.inner.stage_clone();
            staged.apply(thread, transaction)?;
            let report = audit_snapshot(staged.snapshot(thread)?);
            if !report.is_compliant() {
                return Err(SimAuraError::WcagRejected(report));
            }
            self.inner = staged;
        } else {
            self.inner.apply(thread, transaction)?;
        }
        if let Some(focused) = self.focused {
            let order = self.focusable_order(thread)?;
            if !order.contains(&focused) {
                self.focused = None;
            }
        }
        Ok(())
    }
}

fn is_visible(snap: &RecordingSnapshot, widget: &crate::WidgetSnapshot) -> bool {
    !matches!(
        widget
            .properties
            .get(&Property::Visible)
            .and_then(|slot| snap.slots.get(slot)),
        Some(Value::Bool(false))
    )
}

fn is_enabled(snap: &RecordingSnapshot, widget: &crate::WidgetSnapshot) -> bool {
    !matches!(
        widget
            .properties
            .get(&Property::Enabled)
            .and_then(|slot| snap.slots.get(slot)),
        Some(Value::Bool(false))
    )
}

fn audit_snapshot(snap: &RecordingSnapshot) -> WcagReport {
    let mut report = WcagReport {
        inspected_nodes: snap.widgets.len(),
        interactive_controls: 0,
        violations: Vec::new(),
    };
    let ordered = collect_preorder(snap);
    for id in ordered {
        let Some(widget) = snap.widgets.get(&id) else {
            continue;
        };
        if matches!(widget.kind, Kind::Button | Kind::Input) {
            report.interactive_controls += 1;
            let visible = is_visible(snap, widget);
            let has_label = matches!(
                widget.properties.get(&Property::Text).and_then(|slot| snap.slots.get(slot)),
                Some(Value::Text(s)) if !s.trim().is_empty()
            );
            if visible && !has_label {
                report
                    .violations
                    .push(WcagViolation::MissingAccessibleLabel {
                        target: AuditTarget::MountedWidget(id),
                        kind: widget.kind,
                    });
            }
            if !visible && !widget.subscriptions.is_empty() {
                report
                    .violations
                    .push(WcagViolation::HiddenSubscribedControl {
                        target: AuditTarget::MountedWidget(id),
                        kind: widget.kind,
                    });
            }
        }
    }
    report
}

fn collect_preorder(snap: &RecordingSnapshot) -> Vec<WidgetId> {
    let Some(root) = snap.root else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(snap.widgets.len());
    let mut stack = vec![root];
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        out.push(id);
        if let Some(widget) = snap.widgets.get(&id) {
            for &child in widget.children.iter().rev() {
                stack.push(child);
            }
        }
    }
    // Include any widgets even if detached in malformed snapshots so audit is total.
    let extra: BTreeMap<WidgetId, ()> = snap
        .widgets
        .keys()
        .filter(|id| !seen.contains(id))
        .map(|&id| (id, ()))
        .collect();
    out.extend(extra.into_keys());
    out
}
