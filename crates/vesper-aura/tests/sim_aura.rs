use vesper_aura::{
    AuditTarget, AuraSlotTable, CloseAction, CloseOutcome, CommitError, Event, HandlerKey, Key,
    Kind, Limits, NativeUiBackend, Node, NodeOp, Property, Session, SimAura, SimAuraError,
    SlotValue, UiThread, Value, ViewTree, WcagViolation, audit_view_tree, encode_view_tree,
    layout_flags,
};

fn valid_tree() -> ViewTree {
    ViewTree::new(
        Key(1),
        vec![
            Node::new(Key(1), Kind::Container).children([Key(2), Key(3), Key(4)]),
            Node::new(Key(2), Kind::Text).property(Property::Text, Value::Text("Inspector".into())),
            Node::new(Key(3), Kind::Button)
                .property(Property::Text, Value::Text("Save Checkpoint".into()))
                .property(Property::Enabled, Value::Bool(true))
                .on(Event::Click, HandlerKey(10))
                .on(Event::Focused, HandlerKey(11)),
            Node::new(Key(4), Kind::Input)
                .property(Property::Text, Value::Text("Filter".into()))
                .property(Property::Value, Value::Text(String::new()))
                .on(Event::Changed, HandlerKey(20)),
        ],
    )
}

#[test]
fn node_op_encoding_and_slot_table_track_changes() {
    assert_eq!(core::mem::size_of::<NodeOp>(), 16);
    assert_eq!(core::mem::align_of::<NodeOp>(), 4);
    assert_ne!(NativeUiBackend::SimAura, NativeUiBackend::Gtk4);
    assert_ne!(NativeUiBackend::WinUi3, NativeUiBackend::SwiftUi);

    let tree = valid_tree();
    let (ops, mut slots): (Vec<NodeOp>, AuraSlotTable) =
        encode_view_tree(&tree, &Limits::default()).unwrap();
    assert_eq!(ops.len(), 4);
    assert_eq!(ops[0].kind_tag, NodeOp::kind_tag_of(Kind::Container));
    assert_eq!(ops[0].child_count, 3);
    assert_eq!(ops[2].kind_tag, NodeOp::kind_tag_of(Kind::Button));
    assert_ne!(ops[2].layout_flags & layout_flags::INTERACTIVE, 0);
    assert_ne!(ops[2].layout_flags & layout_flags::HAS_ACCESSIBLE_LABEL, 0);

    // No-op write does not mark dirty; changed write marks dirty once; OOB returns false.
    let text_slot = ops[1].property_slot(Kind::Text, Property::Text).unwrap();
    assert_eq!(slots.property(text_slot), Some(Property::Text));
    let initial = slots.values[text_slot as usize].clone();
    assert_eq!(initial, SlotValue::Text("Inspector".into()));
    assert!(!slots.set_slot(text_slot, initial));
    assert!(slots.dirty_slots.is_empty());
    assert!(slots.set_slot(text_slot, SlotValue::Text("Updated".into())));
    assert_eq!(slots.drain_dirty(), vec![text_slot]);
    assert!(!slots.set_slot(999, SlotValue::Bool(true)));
}

#[test]
fn wcag_audit_detects_missing_labels_and_hidden_subscribed_controls() {
    let valid = audit_view_tree(&valid_tree(), &Limits::default()).unwrap();
    assert!(valid.is_compliant());
    assert_eq!(valid.interactive_controls, 2);

    let bad = ViewTree::new(
        Key(1),
        vec![
            Node::new(Key(1), Kind::Container).children([Key(2), Key(3)]),
            Node::new(Key(2), Kind::Button).property(Property::Text, Value::Text("   ".into())),
            Node::new(Key(3), Kind::Input)
                .property(Property::Visible, Value::Bool(false))
                .on(Event::Changed, HandlerKey(7)),
        ],
    );
    let report = audit_view_tree(&bad, &Limits::default()).unwrap();
    assert!(!report.is_compliant());
    assert_eq!(
        report.violations,
        vec![
            WcagViolation::MissingAccessibleLabel {
                target: AuditTarget::TreeKey(Key(2)),
                kind: Kind::Button,
            },
            WcagViolation::HiddenSubscribedControl {
                target: AuditTarget::TreeKey(Key(3)),
                kind: Kind::Input,
            },
        ]
    );
}

#[test]
fn sim_aura_focus_events_atomic_wcag_gate_and_close_decision() {
    let thread = UiThread::claim_current();
    let backend = SimAura::with_wcag_gate(&thread, Limits::default());
    assert_eq!(backend.backend_kind(), NativeUiBackend::SimAura);

    let mut session = Session::new(&thread, backend, Limits::default()).unwrap();
    let initial_plan = session.prepare(&thread, Some(valid_tree())).unwrap();
    let report = session.commit(&thread, initial_plan).unwrap();
    assert_eq!(report.revision, 1);

    let w_btn = session.widget(Key(3)).unwrap();
    let w_input = session.widget(Key(4)).unwrap();

    // Atomic WCAG gate: proposing an unlabeled visible Button fails without
    // advancing revision or disturbing the previously mounted widgets.
    let bad_update = session
        .prepare(
            &thread,
            Some(ViewTree::new(
                Key(1),
                vec![
                    Node::new(Key(1), Kind::Container).children([Key(5)]),
                    Node::new(Key(5), Kind::Button),
                ],
            )),
        )
        .unwrap();
    let fail = session.commit(&thread, bad_update).unwrap_err();
    match fail.error {
        CommitError::Backend(SimAuraError::WcagRejected(wcag)) => {
            assert!(!wcag.is_compliant());
        }
        other => panic!("expected WcagRejected, got {other:?}"),
    }
    assert_eq!(
        session
            .backend(&thread)
            .unwrap()
            .snapshot(&thread)
            .unwrap()
            .revision,
        1
    );
    assert!(
        session
            .backend(&thread)
            .unwrap()
            .audit_wcag(&thread)
            .unwrap()
            .is_compliant()
    );

    // Deterministic keyboard focus traversal + event dispatch.
    let order = session
        .backend(&thread)
        .unwrap()
        .focusable_order(&thread)
        .unwrap();
    assert_eq!(order, vec![w_btn, w_input]);
    assert_eq!(
        session
            .backend_mut(&thread)
            .unwrap()
            .focus_next(&thread)
            .unwrap(),
        w_btn
    );
    assert_eq!(
        session
            .backend_mut(&thread)
            .unwrap()
            .focus_next(&thread)
            .unwrap(),
        w_input
    );
    assert_eq!(
        session
            .backend_mut(&thread)
            .unwrap()
            .focus_prev(&thread)
            .unwrap(),
        w_btn
    );
    let handler = session
        .backend_mut(&thread)
        .unwrap()
        .trigger_event(&thread, w_btn, Event::Click)
        .unwrap();
    assert_eq!(handler, HandlerKey(10));
    assert_eq!(
        session.backend(&thread).unwrap().dispatched_events().len(),
        3
    );

    // Transactional close decision: veto first, then allow close.
    let token = session.request_close(&thread).unwrap();
    assert_eq!(
        session
            .resolve_close(&thread, token, CloseAction::Veto)
            .unwrap(),
        CloseOutcome::Vetoed
    );
    let token2 = session.request_close(&thread).unwrap();
    match session
        .resolve_close(&thread, token2, CloseAction::Allow)
        .unwrap()
    {
        CloseOutcome::Closed(closed_report) => assert_eq!(closed_report.revision, 2),
        other => panic!("expected Closed, got {other:?}"),
    }
}

#[test]
fn encode_view_tree_preserves_distinct_property_identities() {
    let only_enabled = ViewTree::new(
        Key(1),
        vec![Node::new(Key(1), Kind::Input).property(Property::Enabled, Value::Bool(true))],
    );
    let only_visible = ViewTree::new(
        Key(1),
        vec![Node::new(Key(1), Kind::Input).property(Property::Visible, Value::Bool(true))],
    );

    let (ops_en, mut slots_en) = encode_view_tree(&only_enabled, &Limits::default()).unwrap();
    let (ops_vis, mut slots_vis) = encode_view_tree(&only_visible, &Limits::default()).unwrap();

    assert_ne!(slots_en, slots_vis);
    assert_eq!(
        slots_en.values.len(),
        NodeOp::slot_count(Kind::Input) as usize
    );
    assert_eq!(
        NodeOp::property_offset(Kind::Container, Property::Text),
        None
    );

    let en_slot = ops_en[0]
        .property_slot(Kind::Input, Property::Enabled)
        .unwrap();
    let vis_slot = ops_vis[0]
        .property_slot(Kind::Input, Property::Visible)
        .unwrap();
    assert_ne!(en_slot, vis_slot);
    assert_eq!(slots_en.property(en_slot), Some(Property::Enabled));
    assert_eq!(slots_en.values[en_slot as usize], SlotValue::Bool(true));
    assert_eq!(slots_en.values[vis_slot as usize], SlotValue::Unset);

    assert_eq!(slots_vis.property(vis_slot), Some(Property::Visible));
    assert_eq!(slots_vis.values[vis_slot as usize], SlotValue::Bool(true));
    assert_eq!(slots_vis.values[en_slot as usize], SlotValue::Unset);

    assert!(slots_en.set_slot(en_slot, SlotValue::Bool(false)));
    assert_eq!(slots_en.drain_dirty(), vec![en_slot]);
    assert!(slots_vis.set_slot(vis_slot, SlotValue::Bool(false)));
    assert_eq!(slots_vis.drain_dirty(), vec![vis_slot]);
}

#[test]
fn encode_view_tree_rejects_handler_keys_exceeding_u32_max() {
    let max_ok = ViewTree::new(
        Key(1),
        vec![
            Node::new(Key(1), Kind::Button)
                .property(Property::Text, Value::Text("OK".into()))
                .on(Event::Click, HandlerKey(u64::from(u32::MAX))),
        ],
    );
    let (ops, _) = encode_view_tree(&max_ok, &Limits::default()).unwrap();
    assert_eq!(ops[0].event_action_id, u32::MAX);

    let overflow_first = ViewTree::new(
        Key(1),
        vec![
            Node::new(Key(1), Kind::Button)
                .property(Property::Text, Value::Text("Bad".into()))
                .on(Event::Click, HandlerKey(u64::from(u32::MAX) + 1)),
        ],
    );
    assert_eq!(
        encode_view_tree(&overflow_first, &Limits::default()),
        Err(vesper_aura::ViewError::Budget("handler"))
    );

    let overflow_second = ViewTree::new(
        Key(1),
        vec![
            Node::new(Key(1), Kind::Button)
                .property(Property::Text, Value::Text("Bad2".into()))
                .on(Event::Click, HandlerKey(1))
                .on(Event::Focused, HandlerKey(u64::from(u32::MAX) + 1)),
        ],
    );
    assert_eq!(
        encode_view_tree(&overflow_second, &Limits::default()),
        Err(vesper_aura::ViewError::Budget("handler"))
    );
}

#[test]
fn hidden_or_disabled_containers_prune_focus_and_block_events() {
    let thread = UiThread::claim_current();
    let backend = SimAura::new(&thread, Limits::default());
    let mut session = Session::new(&thread, backend, Limits::default()).unwrap();

    // Container 2 is Visible=false; Container 3 is Enabled=false; Button 6 is visible & enabled.
    let tree = ViewTree::new(
        Key(1),
        vec![
            Node::new(Key(1), Kind::Container).children([Key(2), Key(3), Key(6)]),
            Node::new(Key(2), Kind::Container)
                .property(Property::Visible, Value::Bool(false))
                .children([Key(4)]),
            Node::new(Key(3), Kind::Container)
                .property(Property::Enabled, Value::Bool(false))
                .children([Key(5)]),
            Node::new(Key(4), Kind::Button)
                .property(Property::Text, Value::Text("In Hidden".into()))
                .property(Property::Visible, Value::Bool(true))
                .property(Property::Enabled, Value::Bool(true))
                .on(Event::Click, HandlerKey(10)),
            Node::new(Key(5), Kind::Input)
                .property(Property::Text, Value::Text("In Disabled".into()))
                .property(Property::Visible, Value::Bool(true))
                .property(Property::Enabled, Value::Bool(true))
                .on(Event::Changed, HandlerKey(20)),
            Node::new(Key(6), Kind::Button)
                .property(Property::Text, Value::Text("Active".into()))
                .on(Event::Click, HandlerKey(30)),
        ],
    );

    // WCAG audit sees Key(4) as effectively hidden and subscribed (and unlabeled
    // buttons inside a hidden container would not need a visible label).
    let wcag = audit_view_tree(&tree, &Limits::default()).unwrap();
    assert_eq!(
        wcag.violations,
        vec![WcagViolation::HiddenSubscribedControl {
            target: AuditTarget::TreeKey(Key(4)),
            kind: Kind::Button,
        }]
    );

    let plan = session.prepare(&thread, Some(tree)).unwrap();
    session.commit(&thread, plan).unwrap();

    let w_hidden_btn = session.widget(Key(4)).unwrap();
    let w_disabled_input = session.widget(Key(5)).unwrap();
    let w_active_btn = session.widget(Key(6)).unwrap();

    let mounted_wcag = session
        .backend(&thread)
        .unwrap()
        .audit_wcag(&thread)
        .unwrap();
    assert_eq!(
        mounted_wcag.violations,
        vec![WcagViolation::HiddenSubscribedControl {
            target: AuditTarget::MountedWidget(w_hidden_btn),
            kind: Kind::Button,
        }]
    );

    let order = session
        .backend(&thread)
        .unwrap()
        .focusable_order(&thread)
        .unwrap();
    assert_eq!(order, vec![w_active_btn]);

    assert_eq!(
        session
            .backend_mut(&thread)
            .unwrap()
            .trigger_event(&thread, w_hidden_btn, Event::Click),
        Err(SimAuraError::WidgetNotFocusable(w_hidden_btn))
    );
    assert_eq!(
        session.backend_mut(&thread).unwrap().trigger_event(
            &thread,
            w_disabled_input,
            Event::Changed
        ),
        Err(SimAuraError::WidgetNotFocusable(w_disabled_input))
    );

    // Focusing the active button works, and disabling its parent clears focus on commit.
    assert_eq!(
        session
            .backend_mut(&thread)
            .unwrap()
            .focus_next(&thread)
            .unwrap(),
        w_active_btn
    );
    assert_eq!(
        session.backend(&thread).unwrap().focused(&thread).unwrap(),
        Some(w_active_btn)
    );

    let disable_root = session
        .prepare(
            &thread,
            Some(ViewTree::new(
                Key(1),
                vec![
                    Node::new(Key(1), Kind::Container)
                        .property(Property::Enabled, Value::Bool(false))
                        .children([Key(6)]),
                    Node::new(Key(6), Kind::Button)
                        .property(Property::Text, Value::Text("Active".into()))
                        .on(Event::Click, HandlerKey(30)),
                ],
            )),
        )
        .unwrap();
    session.commit(&thread, disable_root).unwrap();

    assert_eq!(
        session.backend(&thread).unwrap().focused(&thread).unwrap(),
        None
    );
    assert_eq!(
        session.backend_mut(&thread).unwrap().focus_next(&thread),
        Err(SimAuraError::NoFocusableWidgets)
    );
    assert_eq!(
        session
            .backend_mut(&thread)
            .unwrap()
            .trigger_event(&thread, w_active_btn, Event::Click),
        Err(SimAuraError::WidgetNotFocusable(w_active_btn))
    );
}
