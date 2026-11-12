use vesper_aura::*;

fn button(key: u64, text: &str) -> Node {
    Node::new(Key(key), Kind::Button)
        .property(Property::Text, Value::Text(text.into()))
        .on(Event::Click, HandlerKey(key))
}
fn tree(order: &[u64]) -> ViewTree {
    let mut nodes =
        vec![Node::new(Key(0), Kind::Container).children(order.iter().map(|k| Key(*k)))];
    nodes.extend(order.iter().map(|k| button(*k, &format!("item-{k}"))));
    ViewTree::new(Key(0), nodes)
}
fn session(thread: &UiThread) -> Session<RecordingBackend> {
    let limits = Limits::default();
    Session::new(
        thread,
        RecordingBackend::new(thread, limits.clone()),
        limits,
    )
    .unwrap()
}
fn mount(
    session: &mut Session<RecordingBackend>,
    thread: &UiThread,
    tree: ViewTree,
) -> CommitReport {
    let update = session.prepare(thread, Some(tree)).unwrap();
    session.commit(thread, update).unwrap()
}
fn snapshot(session: &Session<RecordingBackend>, thread: &UiThread) -> RecordingSnapshot {
    session
        .backend(thread)
        .unwrap()
        .snapshot(thread)
        .unwrap()
        .clone()
}

#[test]
fn sparse_slot_updates_preserve_native_state_and_unmodified_subscriptions() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui);
    mount(&mut app, &ui, tree(&[1, 2]));
    let widget = app.widget(Key(1)).unwrap();
    let slot = app.slot(Key(1), Property::Text).unwrap();
    let sub = app.subscription(Key(1), Event::Click).unwrap();
    app.backend_mut(&ui)
        .unwrap()
        .set_user_state(&ui, widget, 91)
        .unwrap();
    let mut changed = tree(&[1, 2]);
    changed.nodes[1]
        .properties
        .insert(Property::Text, Value::Text("new".into()));
    let update = app.prepare(&ui, Some(changed.clone())).unwrap();
    assert_eq!(
        update.transaction().patches(),
        &[Patch::SetSlot {
            slot,
            value: Value::Text("new".into())
        }]
    );
    app.commit(&ui, update).unwrap();
    assert_eq!(app.widget(Key(1)), Some(widget));
    assert_eq!(app.slot(Key(1), Property::Text), Some(slot));
    assert_eq!(app.subscription(Key(1), Event::Click), Some(sub));
    assert_eq!(
        app.backend(&ui)
            .unwrap()
            .widget(&ui, widget)
            .unwrap()
            .user_state,
        91
    );
    assert_eq!(
        snapshot(&app, &ui).last_events,
        vec![RecordedEvent::SlotChanged(slot)]
    );
    let update = app.prepare(&ui, Some(changed)).unwrap();
    assert!(update.transaction().patches().is_empty());
    app.commit(&ui, update).unwrap();
    assert!(snapshot(&app, &ui).last_events.is_empty());
    let backend = app.shutdown(&ui).unwrap();
    let final_state = backend.snapshot(&ui).unwrap();
    assert!(final_state.widgets.is_empty());
    assert_eq!(
        final_state.created_subscriptions,
        final_state.disconnected_subscriptions
    );
}

#[test]
fn keyed_insert_remove_reorder_and_stale_handle_rejection() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui);
    mount(&mut app, &ui, tree(&[1, 2, 3]));
    let root = app.widget(Key(0)).unwrap();
    let one = app.widget(Key(1)).unwrap();
    let two = app.widget(Key(2)).unwrap();
    let two_slot = app.slot(Key(2), Property::Text).unwrap();
    let two_sub = app.subscription(Key(2), Event::Click).unwrap();
    let three = app.widget(Key(3)).unwrap();
    mount(&mut app, &ui, tree(&[3, 4, 1]));
    let four = app.widget(Key(4)).unwrap();
    let backend = app.backend(&ui).unwrap();
    assert_eq!(
        backend.widget(&ui, root).unwrap().children,
        vec![three, four, one]
    );
    assert_eq!(app.widget(Key(1)), Some(one));
    assert_eq!(app.widget(Key(3)), Some(three));
    assert_eq!(backend.widget(&ui, two), Err(BackendError::StaleWidget));
    assert_eq!(backend.slot(&ui, two_slot), Err(BackendError::StaleSlot));
    assert_eq!(
        backend.dispatch(&ui, two_sub),
        Err(BackendError::StaleSubscription)
    );
    let events = &backend.snapshot(&ui).unwrap().last_events;
    let disconnect = events
        .iter()
        .position(|e| *e == RecordedEvent::Disconnected(two_sub))
        .unwrap();
    let destroy = events
        .iter()
        .position(|e| *e == RecordedEvent::Destroyed(two))
        .unwrap();
    assert!(disconnect < destroy);
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == RecordedEvent::Disconnected(two_sub))
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RecordedEvent::ChildMoved { child, .. } if *child == three))
    );
    let backend = app.shutdown(&ui).unwrap();
    assert_eq!(backend.snapshot(&ui).unwrap().created_subscriptions, 4);
    assert_eq!(backend.snapshot(&ui).unwrap().disconnected_subscriptions, 4);
}

#[test]
fn reparenting_and_parent_kind_replacement_keep_unchanged_child_state() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui);
    let before = ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Container).children([Key(1), Key(2)]),
            Node::new(Key(1), Kind::Container).children([Key(3)]),
            Node::new(Key(2), Kind::Container),
            button(3, "child"),
        ],
    );
    mount(&mut app, &ui, before);
    let child = app.widget(Key(3)).unwrap();
    let sub = app.subscription(Key(3), Event::Click).unwrap();
    let parent = app.widget(Key(1)).unwrap();
    app.backend_mut(&ui)
        .unwrap()
        .set_user_state(&ui, child, 42)
        .unwrap();
    let after = ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Container).children([Key(2), Key(1)]),
            Node::new(Key(1), Kind::Text).property(Property::Text, Value::Text("replaced".into())),
            Node::new(Key(2), Kind::Container).children([Key(3)]),
            button(3, "child"),
        ],
    );
    mount(&mut app, &ui, after);
    assert_ne!(app.widget(Key(1)), Some(parent));
    assert_eq!(app.widget(Key(3)), Some(child));
    assert_eq!(app.subscription(Key(3), Event::Click), Some(sub));
    let backend = app.backend(&ui).unwrap();
    assert_eq!(
        backend.widget(&ui, child).unwrap().parent,
        app.widget(Key(2))
    );
    assert_eq!(backend.widget(&ui, child).unwrap().user_state, 42);
    assert!(
        !snapshot(&app, &ui)
            .last_events
            .contains(&RecordedEvent::Disconnected(sub))
    );
    app.shutdown(&ui).unwrap();
}

#[test]
fn root_reparenting_keeps_ids_and_properties_rebind_fresh_slots() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui);
    mount(
        &mut app,
        &ui,
        ViewTree::new(
            Key(1),
            vec![Node::new(Key(1), Kind::Container).property(Property::Visible, Value::Bool(true))],
        ),
    );
    let old_root = app.widget(Key(1)).unwrap();
    let old_slot = app.slot(Key(1), Property::Visible).unwrap();
    mount(
        &mut app,
        &ui,
        ViewTree::new(
            Key(0),
            vec![
                Node::new(Key(0), Kind::Container).children([Key(1)]),
                Node::new(Key(1), Kind::Container),
            ],
        ),
    );
    assert_eq!(app.widget(Key(1)), Some(old_root));
    assert_eq!(
        app.backend(&ui).unwrap().slot(&ui, old_slot),
        Err(BackendError::StaleSlot)
    );
    mount(
        &mut app,
        &ui,
        ViewTree::new(
            Key(1),
            vec![
                Node::new(Key(1), Kind::Container).property(Property::Visible, Value::Bool(false)),
            ],
        ),
    );
    assert_eq!(app.widget(Key(1)), Some(old_root));
    let new_slot = app.slot(Key(1), Property::Visible).unwrap();
    assert_ne!(new_slot, old_slot);
    assert_eq!(snapshot(&app, &ui).root, Some(old_root));
    app.shutdown(&ui).unwrap();
}

#[test]
fn changed_handler_disconnects_once_before_reconnect() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui);
    mount(&mut app, &ui, tree(&[1]));
    let old = app.subscription(Key(1), Event::Click).unwrap();
    let mut changed = tree(&[1]);
    changed.nodes[1]
        .subscriptions
        .insert(Event::Click, HandlerKey(900));
    mount(&mut app, &ui, changed);
    let new = app.subscription(Key(1), Event::Click).unwrap();
    assert_ne!(new, old);
    assert_eq!(
        snapshot(&app, &ui).last_events,
        vec![
            RecordedEvent::Disconnected(old),
            RecordedEvent::Subscribed(new)
        ]
    );
    assert_eq!(
        app.backend(&ui).unwrap().dispatch(&ui, new),
        Ok(HandlerKey(900))
    );
    assert_eq!(
        app.backend(&ui).unwrap().dispatch(&ui, old),
        Err(BackendError::StaleSubscription)
    );
    app.shutdown(&ui).unwrap();
}

#[test]
fn every_partial_backend_rejection_returns_same_plan_and_unchanged_live_owners() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui);
    mount(&mut app, &ui, tree(&[1, 2, 3]));
    let before = snapshot(&app, &ui);
    let desired = tree(&[3, 4, 1]);
    let mut pending = app.prepare(&ui, Some(desired.clone())).unwrap();
    let patches = pending.transaction().patches().to_vec();
    for index in 0..=patches.len() {
        app.backend_mut(&ui)
            .unwrap()
            .reject_next_at(&ui, index)
            .unwrap();
        let failure = app.commit(&ui, pending).unwrap_err();
        assert!(
            matches!(failure.error, CommitError::Backend(BackendError::Injected(i)) if i == index)
        );
        assert_eq!(failure.update.tree(), Some(&desired));
        assert_eq!(failure.update.transaction().patches(), patches);
        assert_eq!(snapshot(&app, &ui), before);
        pending = failure.update;
    }
    app.commit(&ui, pending).unwrap();
    let before_unmount = snapshot(&app, &ui);
    let mut unmount = app.prepare(&ui, None).unwrap();
    for index in 0..=unmount.transaction().patches().len() {
        app.backend_mut(&ui)
            .unwrap()
            .reject_next_at(&ui, index)
            .unwrap();
        let failure = app.commit(&ui, unmount).unwrap_err();
        assert_eq!(snapshot(&app, &ui), before_unmount);
        unmount = failure.update;
    }
    app.commit(&ui, unmount).unwrap();
    let backend = app.shutdown(&ui).unwrap();
    let final_state = backend.snapshot(&ui).unwrap();
    assert!(final_state.widgets.is_empty());
    assert_eq!(
        final_state.created_subscriptions,
        final_state.disconnected_subscriptions
    );
}

#[test]
fn stale_and_foreign_plans_retain_input_and_do_not_call_backend() {
    let ui = UiThread::claim_current();
    let mut a = session(&ui);
    let mut b = session(&ui);
    let stale = a.prepare(&ui, Some(tree(&[1]))).unwrap();
    mount(&mut a, &ui, tree(&[2]));
    let before = snapshot(&a, &ui);
    let failure = a.commit(&ui, stale).unwrap_err();
    assert!(matches!(failure.error, CommitError::StaleRevision));
    assert_eq!(failure.update.into_tree(), Some(tree(&[1])));
    assert_eq!(snapshot(&a, &ui), before);
    let foreign = b.prepare(&ui, Some(tree(&[5]))).unwrap();
    let failure = a.commit(&ui, foreign).unwrap_err();
    assert!(matches!(failure.error, CommitError::ForeignSession));
    b.commit(&ui, failure.update).unwrap();
    let foreign_id = b.widget(Key(5)).unwrap();
    assert_eq!(
        a.backend(&ui).unwrap().widget(&ui, foreign_id),
        Err(BackendError::ForeignSession)
    );
    let foreign_slot = b.slot(Key(5), Property::Text).unwrap();
    let foreign_sub = b.subscription(Key(5), Event::Click).unwrap();
    assert_eq!(
        a.backend(&ui).unwrap().slot(&ui, foreign_slot),
        Err(BackendError::ForeignSession)
    );
    assert_eq!(
        a.backend(&ui).unwrap().dispatch(&ui, foreign_sub),
        Err(BackendError::ForeignSession)
    );
    a.shutdown(&ui).unwrap();
    b.shutdown(&ui).unwrap();
}

#[test]
fn deterministic_plans_ignore_flat_node_storage_order() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui);
    let input = tree(&[1, 2, 3]);
    mount(&mut app, &ui, input.clone());
    let mut reordered_storage = input;
    reordered_storage.nodes.reverse();
    let plan = app.prepare(&ui, Some(reordered_storage)).unwrap();
    assert!(plan.transaction().patches().is_empty());
    app.commit(&ui, plan).unwrap();
    app.shutdown(&ui).unwrap();
}

fn permutations(xs: &mut [u64], index: usize, output: &mut Vec<Vec<u64>>) {
    if index == xs.len() {
        output.push(xs.to_vec());
        return;
    }
    for i in index..xs.len() {
        xs.swap(index, i);
        permutations(xs, index + 1, output);
        xs.swap(index, i);
    }
}

#[test]
fn all_small_keyed_order_transitions_match_independent_expected_order() {
    let ui = UiThread::claim_current();
    let mut orders = vec![vec![]];
    for mask in 1u32..16 {
        let mut items: Vec<u64> = (1..=4).filter(|i| mask & (1 << (i - 1)) != 0).collect();
        permutations(&mut items, 0, &mut orders);
    }
    // 65 distinct lists, 4,225 real retained-backend transitions. Expected
    // children come directly from the declarative keys, not from the planner.
    assert_eq!(orders.len(), 65);
    let mut app = session(&ui);
    for from in &orders {
        for to in &orders {
            mount(&mut app, &ui, tree(from));
            let retained: Vec<_> = from
                .iter()
                .filter(|k| to.contains(k))
                .map(|k| (*k, app.widget(Key(*k)).unwrap()))
                .collect();
            mount(&mut app, &ui, tree(to));
            let expected: Vec<_> = to.iter().map(|k| app.widget(Key(*k)).unwrap()).collect();
            let root = app.widget(Key(0)).unwrap();
            assert_eq!(
                app.backend(&ui)
                    .unwrap()
                    .widget(&ui, root)
                    .unwrap()
                    .children,
                expected
            );
            for (key, id) in retained {
                assert_eq!(app.widget(Key(key)), Some(id));
            }
        }
    }
    let backend = app.shutdown(&ui).unwrap();
    let end = backend.snapshot(&ui).unwrap();
    assert_eq!(end.created_subscriptions, end.disconnected_subscriptions);
}

#[test]
fn backend_itself_rejects_foreign_and_stale_transactions_atomically() {
    let ui = UiThread::claim_current();
    let mut a = session(&ui);
    let mut b = session(&ui);
    mount(&mut a, &ui, tree(&[1]));
    mount(&mut b, &ui, tree(&[2]));
    let pending = a.prepare(&ui, Some(tree(&[1, 3]))).unwrap();
    let before_b = snapshot(&b, &ui);
    assert_eq!(
        b.backend_mut(&ui)
            .unwrap()
            .apply(&ui, pending.transaction()),
        Err(BackendError::ForeignSession)
    );
    assert_eq!(snapshot(&b, &ui), before_b);
    mount(&mut a, &ui, tree(&[4]));
    let before_a = snapshot(&a, &ui);
    assert_eq!(
        a.backend_mut(&ui)
            .unwrap()
            .apply(&ui, pending.transaction()),
        Err(BackendError::StaleRevision)
    );
    assert_eq!(snapshot(&a, &ui), before_a);
    assert_eq!(pending.into_tree(), Some(tree(&[1, 3])));
    a.shutdown(&ui).unwrap();
    b.shutdown(&ui).unwrap();
}

#[test]
fn initial_mount_rejection_never_publishes_widgets_slots_or_registrations() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui);
    let before = snapshot(&app, &ui);
    let mut pending = app.prepare(&ui, Some(tree(&[1, 2]))).unwrap();
    for index in 0..=pending.transaction().patches().len() {
        app.backend_mut(&ui)
            .unwrap()
            .reject_next_at(&ui, index)
            .unwrap();
        let failure = app.commit(&ui, pending).unwrap_err();
        assert_eq!(snapshot(&app, &ui), before);
        assert!(app.widget(Key(1)).is_none());
        pending = failure.update;
    }
    app.commit(&ui, pending).unwrap();
    let backend = app.shutdown(&ui).unwrap();
    let done = backend.snapshot(&ui).unwrap();
    assert_eq!(done.created_subscriptions, 2);
    assert_eq!(done.disconnected_subscriptions, 2);
}
