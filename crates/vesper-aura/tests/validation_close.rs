use vesper_aura::*;
fn tree() -> ViewTree {
    ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Container).children([Key(1)]),
            Node::new(Key(1), Kind::Button)
                .property(Property::Text, Value::Text("Go".into()))
                .on(Event::Click, HandlerKey(7)),
        ],
    )
}
fn session(ui: &UiThread, limits: Limits) -> Session<RecordingBackend> {
    Session::new(ui, RecordingBackend::new(ui, limits.clone()), limits).unwrap()
}
fn mount(app: &mut Session<RecordingBackend>, ui: &UiThread) {
    let plan = app.prepare(ui, Some(tree())).unwrap();
    app.commit(ui, plan).unwrap();
}
fn snapshot(app: &Session<RecordingBackend>, ui: &UiThread) -> RecordingSnapshot {
    app.backend(ui).unwrap().snapshot(ui).unwrap().clone()
}
#[test]
fn malformed_topologies_preserve_input_and_mounted_state() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui, Limits::default());
    mount(&mut app, &ui);
    let before = snapshot(&app, &ui);
    let mut duplicate = tree();
    duplicate.nodes.push(duplicate.nodes[1].clone());
    let mut missing_child = tree();
    missing_child.nodes[0].children.push(Key(90));
    let mut shared = tree();
    shared.nodes[0].children.push(Key(1));
    let mut root_parent = tree();
    root_parent.nodes[1] = Node::new(Key(1), Kind::Container).children([Key(0)]);
    let disconnected = ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Container),
            Node::new(Key(1), Kind::Text),
        ],
    );
    let cycle = ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Container),
            Node::new(Key(1), Kind::Container).children([Key(2)]),
            Node::new(Key(2), Kind::Container).children([Key(1)]),
        ],
    );
    let leaf = ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Text).children([Key(1)]),
            Node::new(Key(1), Kind::Text),
        ],
    );
    let wrong_prop = ViewTree::new(
        Key(0),
        vec![Node::new(Key(0), Kind::Text).property(Property::Enabled, Value::Text("yes".into()))],
    );
    let wrong_event = ViewTree::new(
        Key(0),
        vec![Node::new(Key(0), Kind::Text).on(Event::Click, HandlerKey(1))],
    );
    let negative_width = ViewTree::new(
        Key(0),
        vec![Node::new(Key(0), Kind::Text).property(Property::Width, Value::Int(-1))],
    );
    let container_text = ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Container).property(Property::Text, Value::Text("bad".into())),
        ],
    );
    let cases = vec![
        (duplicate, ViewError::DuplicateKey(Key(1))),
        (
            missing_child,
            ViewError::MissingChild {
                parent: Key(0),
                child: Key(90),
            },
        ),
        (shared, ViewError::MultipleParents(Key(1))),
        (root_parent, ViewError::RootHasParent),
        (disconnected, ViewError::CycleOrDisconnected),
        (cycle, ViewError::CycleOrDisconnected),
        (leaf, ViewError::ChildrenOnLeaf(Key(0))),
        (
            wrong_prop,
            ViewError::PropertyType {
                key: Key(0),
                property: Property::Enabled,
            },
        ),
        (
            wrong_event,
            ViewError::UnsupportedEvent {
                key: Key(0),
                event: Event::Click,
            },
        ),
        (
            negative_width,
            ViewError::PropertyType {
                key: Key(0),
                property: Property::Width,
            },
        ),
        (
            container_text,
            ViewError::PropertyType {
                key: Key(0),
                property: Property::Text,
            },
        ),
        (
            ViewTree::new(Key(99), vec![]),
            ViewError::MissingRoot(Key(99)),
        ),
    ];
    for (input, error) in cases {
        let failure = app.prepare(&ui, Some(input.clone())).unwrap_err();
        assert_eq!(failure.error, PrepareError::View(error));
        assert_eq!(failure.tree, Some(input));
        assert_eq!(snapshot(&app, &ui), before);
    }
    app.shutdown(&ui).unwrap();
}

#[test]
fn budgets_fail_before_backend_and_accepted_tree_remains_unmountable() {
    let ui = UiThread::claim_current();
    let mut cases = Vec::new();
    let base = Limits::default();
    let mut l = base.clone();
    l.max_nodes = 1;
    cases.push((l, "nodes"));
    let mut l = base.clone();
    l.max_children = 0;
    cases.push((l, "children"));
    let mut l = base.clone();
    l.max_depth = 1;
    cases.push((l, "depth"));
    let mut l = base.clone();
    l.max_properties = 0;
    cases.push((l, "properties"));
    let mut l = base.clone();
    l.max_subscriptions = 0;
    cases.push((l, "subscriptions"));
    let mut l = base.clone();
    l.max_text_bytes = 1;
    cases.push((l, "property text"));
    let mut l = base.clone();
    l.max_total_text_bytes = 1;
    cases.push((l, "total text"));
    let mut l = base;
    l.max_patches = 2;
    cases.push((l, "teardown patches"));
    for (limits, expected) in cases {
        let mut app = session(&ui, limits);
        let before = snapshot(&app, &ui);
        let failure = app.prepare(&ui, Some(tree())).unwrap_err();
        assert_eq!(
            failure.error,
            PrepareError::View(ViewError::Budget(expected))
        );
        assert_eq!(snapshot(&app, &ui), before);
        app.shutdown(&ui).unwrap();
    }
    let limits = Limits {
        max_patches: 6,
        ..Limits::default()
    };
    let mut app = session(&ui, limits);
    mount(&mut app, &ui); // create(2)+slot+subscription+edge+root = 6
    let unmount = app.prepare(&ui, None).unwrap();
    assert_eq!(unmount.transaction().patches().len(), 6);
    app.commit(&ui, unmount).unwrap();
    app.shutdown(&ui).unwrap();
}

#[test]
fn patch_budget_exhaustion_does_not_advance_revision_or_lose_tree() {
    let ui = UiThread::claim_current();
    let limits = Limits {
        max_patches: 6,
        ..Limits::default()
    };
    let mut app = session(&ui, limits);
    mount(&mut app, &ui);
    let before = snapshot(&app, &ui);
    let replacement = ViewTree::new(
        Key(2),
        vec![
            Node::new(Key(2), Kind::Container).children([Key(3)]),
            Node::new(Key(3), Kind::Button)
                .property(Property::Text, Value::Text("New".into()))
                .on(Event::Click, HandlerKey(8)),
        ],
    );
    // Both trees fit individually, but replacing both at once exceeds six.
    let failure = app.prepare(&ui, Some(replacement.clone())).unwrap_err();
    assert_eq!(
        failure.error,
        PrepareError::View(ViewError::Budget("patches"))
    );
    assert_eq!(failure.tree, Some(replacement));
    assert_eq!(snapshot(&app, &ui), before);
    app.shutdown(&ui).unwrap();
}

#[test]
fn veto_then_allow_close_is_explicit_and_tears_down_exactly_once() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui, Limits::default());
    mount(&mut app, &ui);
    let before = snapshot(&app, &ui);
    let decision = app.request_close(&ui).unwrap();
    assert_eq!(
        app.request_close(&ui).unwrap_err(),
        CloseRequestError::AlreadyPending
    );
    assert_eq!(
        app.resolve_close(&ui, decision, CloseAction::Veto).unwrap(),
        CloseOutcome::Vetoed
    );
    assert_eq!(snapshot(&app, &ui), before);
    let decision = app.request_close(&ui).unwrap();
    assert!(matches!(
        app.resolve_close(&ui, decision, CloseAction::Allow),
        Ok(CloseOutcome::Closed(_))
    ));
    assert!(snapshot(&app, &ui).widgets.is_empty());
    assert_eq!(
        app.request_close(&ui).unwrap_err(),
        CloseRequestError::Closed
    );
    assert_eq!(
        app.prepare(&ui, Some(tree())).unwrap_err().error,
        PrepareError::View(ViewError::Closed)
    );
    let backend = app.shutdown(&ui).unwrap();
    let final_state = backend.snapshot(&ui).unwrap();
    assert_eq!(final_state.created_subscriptions, 1);
    assert_eq!(final_state.disconnected_subscriptions, 1);
}

#[test]
fn rejected_close_keeps_decision_widgets_and_subscriptions_for_retry() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui, Limits::default());
    mount(&mut app, &ui);
    let before = snapshot(&app, &ui);
    let decision = app.request_close(&ui).unwrap();
    app.backend_mut(&ui)
        .unwrap()
        .reject_next_at(&ui, usize::MAX)
        .unwrap();
    let failure = app
        .resolve_close(&ui, decision, CloseAction::Allow)
        .unwrap_err();
    let CloseError::Commit(rejected) = failure.error else {
        panic!("wrong rejection")
    };
    assert!(matches!(
        rejected.error,
        CommitError::Backend(BackendError::Injected(_))
    ));
    assert!(rejected.update.into_tree().is_none());
    assert_eq!(snapshot(&app, &ui), before);
    app.resolve_close(&ui, failure.decision, CloseAction::Allow)
        .unwrap();
    app.shutdown(&ui).unwrap();
}

#[test]
fn foreign_close_owner_is_returned_to_its_session_and_pending_blocks_shutdown() {
    let ui = UiThread::claim_current();
    let mut a = session(&ui, Limits::default());
    let mut b = session(&ui, Limits::default());
    mount(&mut a, &ui);
    mount(&mut b, &ui);
    let decision = a.request_close(&ui).unwrap();
    let failure = b
        .resolve_close(&ui, decision, CloseAction::Allow)
        .unwrap_err();
    assert!(matches!(failure.error, CloseError::ForeignSession));
    let blocked = a.shutdown(&ui).unwrap_err();
    assert!(matches!(blocked.error, ShutdownError::PendingClose));
    let mut a = blocked.session;
    a.resolve_close(&ui, failure.decision, CloseAction::Veto)
        .unwrap();
    a.shutdown(&ui).unwrap();
    b.shutdown(&ui).unwrap();
}

#[test]
fn shutdown_rejection_returns_live_backend_owner_and_failed_plan() {
    let ui = UiThread::claim_current();
    let mut app = session(&ui, Limits::default());
    mount(&mut app, &ui);
    let before = snapshot(&app, &ui);
    app.backend_mut(&ui)
        .unwrap()
        .reject_next_at(&ui, 3)
        .unwrap();
    let failure = app.shutdown(&ui).unwrap_err();
    assert!(matches!(
        failure.error,
        ShutdownError::Commit(CommitError::Backend(BackendError::Injected(3)))
    ));
    assert!(failure.update.is_some());
    assert_eq!(snapshot(&failure.session, &ui), before);
    let backend = failure.session.shutdown(&ui).unwrap();
    let done = backend.snapshot(&ui).unwrap();
    assert!(done.widgets.is_empty());
    assert_eq!(done.created_subscriptions, done.disconnected_subscriptions);
}

#[test]
fn local_thread_claim_does_not_require_or_assert_os_main_thread() {
    // Portable core deliberately permits initialization on a worker. Only
    // the future native toolkit initialization boundary can attest OS main.
    std::thread::spawn(|| {
        let ui = UiThread::claim_current();
        let mut app = session(&ui, Limits::default());
        mount(&mut app, &ui);
        app.shutdown(&ui).unwrap();
    })
    .join()
    .unwrap();
}

#[test]
fn backend_own_budget_failure_preserves_the_complete_pending_owner() {
    let ui = UiThread::claim_current();
    let recording_limits = Limits {
        max_total_text_bytes: 1,
        ..Limits::default()
    };
    let backend = RecordingBackend::new(&ui, recording_limits);
    let mut app = Session::new(&ui, backend, Limits::default()).unwrap();
    let before = snapshot(&app, &ui);
    let pending = app.prepare(&ui, Some(tree())).unwrap();
    let failure = app.commit(&ui, pending).unwrap_err();
    assert!(matches!(
        failure.error,
        CommitError::Backend(BackendError::Budget("total text"))
    ));
    assert_eq!(failure.update.tree(), Some(&tree()));
    assert_eq!(snapshot(&app, &ui), before);
    assert_eq!(failure.update.into_tree(), Some(tree()));
    app.shutdown(&ui).unwrap();
}

#[test]
fn independent_backend_patch_cap_cannot_accept_an_unclosable_tree() {
    let ui = UiThread::claim_current();
    let backend = RecordingBackend::new(
        &ui,
        Limits {
            max_patches: 2,
            ..Limits::default()
        },
    );
    let mut app = Session::new(&ui, backend, Limits::default()).unwrap();
    let initial = ViewTree::new(Key(0), vec![Node::new(Key(0), Kind::Container)]);
    let plan = app.prepare(&ui, Some(initial)).unwrap();
    app.commit(&ui, plan).unwrap();
    let before = snapshot(&app, &ui);
    // This update has only two operations, but accepting it would require
    // four operations to close, beyond the independent backend's budget.
    let grows = ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Container)
                .property(Property::Enabled, Value::Bool(true))
                .property(Property::Visible, Value::Bool(true)),
        ],
    );
    let pending = app.prepare(&ui, Some(grows)).unwrap();
    assert_eq!(pending.transaction().patches().len(), 2);
    let failure = app.commit(&ui, pending).unwrap_err();
    assert!(matches!(
        failure.error,
        CommitError::Backend(BackendError::Budget("teardown patches"))
    ));
    assert_eq!(snapshot(&app, &ui), before);
    assert!(failure.update.into_tree().is_some());
    let decision = app.request_close(&ui).unwrap();
    app.resolve_close(&ui, decision, CloseAction::Allow)
        .unwrap();
    app.shutdown(&ui).unwrap();
}
