//! `SimAura`, `encode_view_tree` and `audit_view_tree` over the Loom package in vesper/
//! (`sim_*`, `encode_tree`, `audit_tree`). The focus field and the
//! dispatched-event log are plain owned state here; every decision (focus
//! order, wrap-around, which handler fires, WCAG findings, the topology
//! encoding) comes from the Vesper build.

use super::{binding, codec};
use crate::{
    AuditTarget, AuraSlotTable, Backend, BackendError, Event, HandlerKey, Key, Limits,
    NativeUiBackend, NodeOp, RecordingBackend, RecordingSnapshot, SimAuraError, SimulatedEvent,
    SlotValue, Transaction, UiThread, ViewError, ViewTree, WcagReport, WcagViolation, WidgetId,
};

/// Encodes a validated [`ViewTree`] in depth-first pre-order into padding-free
/// [`NodeOp`] instructions paired with an initial [`AuraSlotTable`].
pub fn encode_view_tree(
    tree: &ViewTree,
    limits: &Limits,
) -> Result<(Vec<NodeOp>, AuraSlotTable), ViewError> {
    let (words, text) = codec::tree(tree);
    let q = binding::encode_tree(&words, &text, &codec::limits(limits));
    if let Some(fault) = q.fault() {
        return Err(codec::view_error(fault));
    }
    let view = q.as_ref();
    let items = view.items().as_slice();
    let ops = items
        .as_chunks::<5>()
        .0
        .iter()
        .map(|w| NodeOp {
            kind_tag: w[0] as u16,
            child_count: w[1] as u16,
            prop_slot_base: w[2] as u32,
            event_action_id: w[3] as u32,
            layout_flags: w[4] as u32,
        })
        .collect();
    let mut values = Vec::new();
    let mut properties = Vec::new();
    for w in view.values().as_slice().as_chunks::<3>().0 {
        let property = codec::property_of(w[0]);
        properties.push(property);
        let value = (w[2] == 1)
            .then(|| tree.nodes.get(w[1] as usize))
            .flatten()
            .and_then(|n| n.properties.get(&property));
        values.push(value.map(SlotValue::from).unwrap_or(SlotValue::Unset));
    }
    Ok((ops, AuraSlotTable::with_slots(values, properties)))
}

/// Audits a declarative [`ViewTree`] against WCAG 4.1.2 accessible-label rules.
pub fn audit_view_tree(tree: &ViewTree, limits: &Limits) -> Result<WcagReport, ViewError> {
    let (words, text) = codec::tree(tree);
    let q = binding::audit_tree(&words, &text, &codec::limits(limits));
    if let Some(fault) = q.fault() {
        return Err(codec::view_error(fault));
    }
    Ok(report(&q, |k| AuditTarget::TreeKey(Key(k))))
}

fn report(q: &binding::Query, target: impl Fn(u64) -> AuditTarget) -> WcagReport {
    let (inspected, interactive) = q.counts();
    let view = q.as_ref();
    let items = view.items().as_slice();
    let violations = items
        .as_chunks::<3>()
        .0
        .iter()
        .map(|w| {
            let target = target(w[1]);
            let kind = codec::kind_of(w[2]);
            if w[0] == 0 {
                WcagViolation::MissingAccessibleLabel { target, kind }
            } else {
                WcagViolation::HiddenSubscribedControl { target, kind }
            }
        })
        .collect();
    WcagReport {
        inspected_nodes: inspected as usize,
        interactive_controls: interactive as usize,
        violations,
    }
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
        self.inner.check_thread(thread)?;
        Ok(self.focused)
    }

    pub fn dispatched_events(&self) -> &[SimulatedEvent] {
        &self.dispatched_events
    }

    /// Evaluates WCAG 4.1.2 accessible-label compliance on the currently mounted tree.
    pub fn audit_wcag(&self, thread: &UiThread) -> Result<WcagReport, BackendError> {
        self.inner.check_thread(thread)?;
        Ok(audit_recorder(&self.inner))
    }

    /// Returns depth-first pre-order focusable interactive controls (`Button` and
    /// `Input` that are visible and enabled).
    pub fn focusable_order(&self, thread: &UiThread) -> Result<Vec<WidgetId>, BackendError> {
        self.inner.check_thread(thread)?;
        let session = self.session();
        Ok(binding::sim_focusable(&self.inner.rec)
            .as_ref()
            .items()
            .as_slice()
            .iter()
            .copied()
            .map(|serial| WidgetId { session, serial })
            .collect())
    }

    /// Advances keyboard focus in deterministic tab order, wrapping around.
    pub fn focus_next(&mut self, thread: &UiThread) -> Result<WidgetId, SimAuraError> {
        self.focus_step(thread, true)
    }

    /// Moves keyboard focus backward in deterministic shift-tab order, wrapping around.
    pub fn focus_prev(&mut self, thread: &UiThread) -> Result<WidgetId, SimAuraError> {
        self.focus_step(thread, false)
    }

    /// Triggers an event on an interactive, visible, enabled widget.
    pub fn trigger_event(
        &mut self,
        thread: &UiThread,
        widget: WidgetId,
        event: Event,
    ) -> Result<HandlerKey, SimAuraError> {
        self.inner.check_thread(thread)?;
        let same = widget.session == self.session();
        let q = binding::sim_trigger(&self.inner.rec, same, widget.serial, codec::event(event));
        let (code, _, _, _, handler, _) = q.answer();
        match code {
            0 => {
                let handler = HandlerKey(handler);
                self.dispatched_events.push(SimulatedEvent {
                    widget,
                    event,
                    handler,
                });
                Ok(handler)
            }
            1 => Err(SimAuraError::WidgetNotFocusable(widget)),
            _ => Err(SimAuraError::NotSubscribed { widget, event }),
        }
    }

    fn session(&self) -> u64 {
        self.inner.rec.as_ref().st().session()
    }

    fn focus_step(&mut self, thread: &UiThread, forward: bool) -> Result<WidgetId, SimAuraError> {
        self.inner.check_thread(thread)?;
        let session = self.session();
        let current = self.focused.filter(|f| f.session == session);
        let q = binding::sim_focus(
            &self.inner.rec,
            current.is_some(),
            current.map_or(0, |f| f.serial),
            forward,
        );
        let (_, found, target, has_handler, handler, _) = q.answer();
        if !found {
            self.focused = None;
            return Err(SimAuraError::NoFocusableWidgets);
        }
        let next = WidgetId {
            session,
            serial: target,
        };
        self.focused = Some(next);
        if has_handler {
            self.dispatched_events.push(SimulatedEvent {
                widget: next,
                event: Event::Focused,
                handler: HandlerKey(handler),
            });
        }
        Ok(next)
    }
}

fn audit_recorder(backend: &RecordingBackend) -> WcagReport {
    let session = backend.rec.as_ref().st().session();
    report(&binding::sim_audit(&backend.rec), |serial| {
        AuditTarget::MountedWidget(WidgetId { session, serial })
    })
}

impl Backend for SimAura {
    type Error = SimAuraError;

    fn apply(&mut self, thread: &UiThread, transaction: &Transaction) -> Result<(), Self::Error> {
        if self.enforce_wcag_on_commit {
            let mut staged = self.inner.stage_clone();
            staged.apply(thread, transaction)?;
            staged.check_thread(thread)?;
            let report = audit_recorder(&staged);
            if !report.is_compliant() {
                return Err(SimAuraError::WcagRejected(report));
            }
            self.inner = staged;
        } else {
            self.inner.apply(thread, transaction)?;
        }
        if let Some(focused) = self.focused {
            self.inner.check_thread(thread)?;
            let still = focused.session == self.session()
                && binding::sim_still_focusable(&self.inner.rec, focused.serial)
                    .answer()
                    .1;
            if !still {
                self.focused = None;
            }
        }
        Ok(())
    }
}
