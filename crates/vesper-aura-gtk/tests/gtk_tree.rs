//! Acceptance tests against a real GTK 4 under Xvfb (or any display).
//!
//! GTK may only be used from the thread that initialized it, so this target
//! uses `harness = false` and runs every case sequentially on the process's
//! main thread. Run it with `xvfb-run -a cargo test -p vesper-aura-gtk
//! --features gtk` (see .github/workflows/aura-gtk.yml).

use gtk::{glib, prelude::*};
use vesper_aura::*;
use vesper_aura_gtk::gtk4 as gtk;
use vesper_aura_gtk::*;

type GtkSession = Session<GtkBackend>;

fn text(key: u64, s: &str) -> Node {
    Node::new(Key(key), Kind::Text).property(Property::Text, Value::Text(s.into()))
}
fn button(key: u64, s: &str) -> Node {
    Node::new(Key(key), Kind::Button)
        .property(Property::Text, Value::Text(s.into()))
        .on(Event::Click, HandlerKey(key))
}
fn input(key: u64, placeholder: &str, value: &str) -> Node {
    Node::new(Key(key), Kind::Input)
        .property(Property::Text, Value::Text(placeholder.into()))
        .property(Property::Value, Value::Text(value.into()))
        .on(Event::Changed, HandlerKey(key))
}
fn column(key: u64, children: &[u64]) -> Node {
    Node::new(Key(key), Kind::Container).children(children.iter().map(|k| Key(*k)))
}

fn session(thread: &UiThread) -> GtkSession {
    let limits = Limits::default();
    let backend = GtkBackend::new(thread, limits.clone()).expect("GTK backend");
    Session::new(thread, backend, limits).expect("session")
}

fn commit(session: &mut GtkSession, thread: &UiThread, tree: ViewTree) -> CommitReport {
    let update = session.prepare(thread, Some(tree)).expect("prepare");
    let report = session.commit(thread, update).expect("commit");
    audit(session, thread);
    report
}

fn backend<'a>(session: &'a GtkSession, thread: &UiThread) -> &'a GtkBackend {
    session.backend(thread).unwrap()
}

fn audit(session: &GtkSession, thread: &UiThread) {
    backend(session, thread).audit(thread).unwrap();
}

fn native(session: &GtkSession, thread: &UiThread) -> NativeNode {
    backend(session, thread)
        .native_tree(thread)
        .unwrap()
        .expect("mounted root")
}

fn widget(session: &GtkSession, thread: &UiThread, key: u64) -> gtk::Widget {
    let id = session.widget(Key(key)).expect("key mounted");
    backend(session, thread).native_widget(thread, id).unwrap()
}

/// Leaf summary in GTK order: (GTK type, text).
fn leaves(node: &NativeNode) -> Vec<(String, String)> {
    node.children
        .iter()
        .map(|c| (c.type_name.clone(), c.text.clone().unwrap_or_default()))
        .collect()
}

fn shutdown(session: GtkSession, thread: &UiThread) -> TeardownReport {
    let backend = session.shutdown(thread).expect("shutdown");
    assert!(backend.native_tree(thread).unwrap().is_none());
    backend.close(thread).map_err(|f| f.error).expect("close")
}

fn reconcile_sequence(thread: &UiThread) {
    let mut s = session(thread);
    // Plan 1: initial mount.
    commit(
        &mut s,
        thread,
        ViewTree::new(
            Key(0),
            vec![
                column(0, &[1, 2, 3]),
                text(1, "title"),
                button(2, "ok"),
                input(3, "name", "Ada"),
            ],
        ),
    );
    let tree = native(&s, thread);
    assert_eq!(tree.type_name, "GtkBox");
    assert_eq!(
        leaves(&tree),
        [
            ("GtkLabel".into(), "title".into()),
            ("GtkButton".into(), "ok".into()),
            ("GtkEntry".into(), "name".into()),
        ]
    );
    assert_eq!(tree.children[2].value.as_deref(), Some("Ada"));
    let ok_button = widget(&s, thread, 2);
    let entry = widget(&s, thread, 3);

    // Plan 2: reorder, change a property, disable the button, add a node.
    let plan2 = ViewTree::new(
        Key(0),
        vec![
            column(0, &[3, 2, 4, 1]),
            text(1, "renamed"),
            button(2, "ok").property(Property::Enabled, Value::Bool(false)),
            input(3, "name", "Ada"),
            text(4, "new").property(Property::Width, Value::Int(120)),
        ],
    );
    let update = s.prepare(thread, Some(plan2.clone())).unwrap();
    assert!(
        update
            .transaction()
            .patches()
            .iter()
            .any(|p| matches!(p, Patch::MoveChild { .. }))
    );
    drop(update);
    commit(&mut s, thread, plan2);
    let tree = native(&s, thread);
    assert_eq!(
        leaves(&tree),
        [
            ("GtkEntry".into(), "name".into()),
            ("GtkButton".into(), "ok".into()),
            ("GtkLabel".into(), "new".into()),
            ("GtkLabel".into(), "renamed".into()),
        ]
    );
    assert!(!tree.children[1].sensitive);
    assert_eq!(tree.children[2].width_request, 120);
    // Same key and kind keeps the same native widget across a reorder.
    assert_eq!(widget(&s, thread, 2), ok_button);
    assert_eq!(widget(&s, thread, 3), entry);

    // Plan 3: reparent into a nested row, replace a kind, drop a node.
    commit(
        &mut s,
        thread,
        ViewTree::new(
            Key(0),
            vec![
                column(0, &[5, 1]),
                column(5, &[2, 3]),
                button(1, "was text"),
                button(2, "ok"),
                input(3, "name", "Grace"),
            ],
        ),
    );
    let tree = native(&s, thread);
    assert_eq!(tree.children.len(), 2);
    assert_eq!(tree.children[0].type_name, "GtkBox");
    assert_eq!(
        leaves(&tree.children[0]),
        [
            ("GtkButton".into(), "ok".into()),
            ("GtkEntry".into(), "name".into()),
        ]
    );
    assert_eq!(tree.children[0].children[1].value.as_deref(), Some("Grace"));
    assert_eq!(tree.children[1].type_name, "GtkButton");
    assert_eq!(widget(&s, thread, 2), ok_button, "reparent keeps widget");
    assert_eq!(widget(&s, thread, 3), entry, "reparent keeps widget");
    assert_eq!(ok_button.parent(), Some(widget(&s, thread, 5)));
    // Re-enabling via property removal resets GTK's default.
    assert!(ok_button.is_sensitive());

    // Plan 4: replace the root entirely.
    commit(&mut s, thread, ViewTree::new(Key(9), vec![text(9, "solo")]));
    let tree = native(&s, thread);
    assert_eq!(tree.type_name, "GtkLabel");
    assert_eq!(tree.text.as_deref(), Some("solo"));
    assert!(ok_button.parent().is_none());

    let report = shutdown(s, thread);
    drop((ok_button, entry));
    assert!(report.created >= 8);
}

/// Every permutation and prefix of a four-item keyed list, applied in sequence
/// to the same GTK box, matches the shadow model and the expected order.
fn keyed_list_transitions(thread: &UiThread) {
    let mut s = session(thread);
    let mut lists: Vec<Vec<u64>> = Vec::new();
    let items = [1u64, 2, 3, 4];
    for a in items {
        for b in items {
            for c in items {
                for d in items {
                    let p = [a, b, c, d];
                    let mut seen = std::collections::BTreeSet::new();
                    if p.iter().all(|x| seen.insert(*x)) {
                        lists.push(p.to_vec());
                    }
                }
            }
        }
    }
    let mut extra = Vec::new();
    for (i, l) in lists.iter().enumerate() {
        extra.push(l[..i % 5].to_vec());
    }
    lists.extend(extra);
    let mut retained = std::collections::BTreeMap::new();
    for order in &lists {
        let mut nodes = vec![column(0, order)];
        nodes.extend(order.iter().map(|k| button(*k, &format!("item-{k}"))));
        commit(&mut s, thread, ViewTree::new(Key(0), nodes));
        let labels: Vec<String> = native(&s, thread)
            .children
            .iter()
            .map(|c| c.text.clone().unwrap())
            .collect();
        let expect: Vec<String> = order.iter().map(|k| format!("item-{k}")).collect();
        assert_eq!(labels, expect);
        for k in order {
            let w = widget(&s, thread, *k);
            if let Some(prev) = retained.get(k) {
                assert_eq!(&w, prev, "key {k} keeps its GTK widget while mounted");
            }
            retained.insert(*k, w);
        }
        retained.retain(|k, _| order.contains(k));
    }
    drop(retained);
    let report = shutdown(s, thread);
    assert_eq!(report.still_alive, 0);
    println!("  {} transitions", lists.len());
}

fn callbacks_route_by_key(thread: &UiThread) {
    let mut s = session(thread);
    let tree = |handler: u64, value: &str| {
        ViewTree::new(
            Key(0),
            vec![
                column(0, &[1, 2]),
                button(1, "go").on(Event::Click, HandlerKey(handler)),
                input(2, "type here", value),
            ],
        )
    };
    commit(&mut s, thread, tree(100, ""));
    let go = widget(&s, thread, 1).downcast::<gtk::Button>().unwrap();
    let entry = widget(&s, thread, 2).downcast::<gtk::Entry>().unwrap();

    go.emit_clicked();
    go.emit_clicked();
    entry.set_text("hello"); // a user edit
    let batch = backend(&s, thread).drain_events(thread).unwrap();
    let click = s.subscription(Key(1), Event::Click).unwrap();
    let changed = s.subscription(Key(2), Event::Changed).unwrap();
    assert_eq!(
        batch.events,
        [
            UiEvent {
                subscription: click,
                handler: HandlerKey(100),
                event: Event::Click,
                payload: Payload::None
            },
            UiEvent {
                subscription: click,
                handler: HandlerKey(100),
                event: Event::Click,
                payload: Payload::None
            },
            UiEvent {
                subscription: changed,
                handler: HandlerKey(2),
                event: Event::Changed,
                payload: Payload::Text("hello".into())
            },
        ]
    );
    assert_eq!((batch.dropped, batch.stale), (0, 0));

    // A programmatic value change through the plan is not a user edit.
    commit(&mut s, thread, tree(100, "from plan"));
    assert_eq!(entry.text(), "from plan");
    assert!(
        backend(&s, thread)
            .drain_events(thread)
            .unwrap()
            .events
            .is_empty()
    );

    // A click queued before a handler change is stale, never delivered to
    // either handler; later clicks route to the new key.
    go.emit_clicked();
    commit(&mut s, thread, tree(200, "from plan"));
    go.emit_clicked();
    let batch = backend(&s, thread).drain_events(thread).unwrap();
    assert_eq!(batch.stale, 1);
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].handler, HandlerKey(200));

    // Removing the subscription disconnects the GTK handler.
    commit(
        &mut s,
        thread,
        ViewTree::new(Key(0), vec![column(0, &[1]), text(1, "no handler")]),
    );
    go.emit_clicked(); // the old button is detached and destroyed by Aura
    let batch = backend(&s, thread).drain_events(thread).unwrap();
    assert!(batch.events.is_empty() && batch.stale == 0);
    drop((go, entry));
    assert_eq!(shutdown(s, thread).still_alive, 0);
}

fn focus_subscriptions_install_controllers(thread: &UiThread) {
    let mut s = session(thread);
    let focused = |on: bool| {
        let mut b = button(1, "focus me");
        if on {
            b = b.on(Event::Focused, HandlerKey(7));
        }
        ViewTree::new(Key(0), vec![column(0, &[1, 2]), b, text(2, "x")])
    };
    commit(&mut s, thread, focused(true));
    assert_eq!(native(&s, thread).children[0].aura_controllers, 1);
    let b = backend(&s, thread);
    b.present(thread).unwrap();
    b.pump(thread).unwrap();
    let w = widget(&s, thread, 1);
    let grabbed = w.grab_focus();
    b.pump(thread).unwrap();
    let batch = b.drain_events(thread).unwrap();
    // Xvfb without a window manager gives the only window keyboard focus.
    println!(
        "  grab_focus={grabbed} has_focus={} focus events={}",
        w.has_focus(),
        batch.events.len()
    );
    assert!(grabbed && w.has_focus());
    let focus = s.subscription(Key(1), Event::Focused).unwrap();
    assert_eq!(
        batch.events,
        [UiEvent {
            subscription: focus,
            handler: HandlerKey(7),
            event: Event::Focused,
            payload: Payload::None
        }]
    );
    commit(&mut s, thread, focused(false));
    assert_eq!(native(&s, thread).children[0].aura_controllers, 0);
    drop(w);
    assert_eq!(shutdown(s, thread).still_alive, 0);
}

fn rejected_plans_leave_gtk_untouched(thread: &UiThread) {
    let mut s = session(thread);
    let base = ViewTree::new(
        Key(0),
        vec![column(0, &[1, 2]), text(1, "a"), button(2, "b")],
    );
    commit(&mut s, thread, base);
    let before = native(&s, thread);
    let next = ViewTree::new(
        Key(0),
        vec![
            column(0, &[3, 2]),
            input(3, "c", "v"),
            button(2, "B").property(Property::Width, Value::Int(40)),
        ],
    );
    let len = s
        .prepare(thread, Some(next.clone()))
        .unwrap()
        .transaction()
        .patches()
        .len();
    for at in 0..=len {
        s.backend_mut(thread)
            .unwrap()
            .reject_next_at(thread, at)
            .unwrap();
        let update = s.prepare(thread, Some(next.clone())).unwrap();
        let failure = s.commit(thread, update).unwrap_err();
        assert!(matches!(
            failure.error,
            CommitError::Backend(GtkError::Rejected(BackendError::Injected(_)))
        ));
        assert_eq!(native(&s, thread), before, "reject at {at}");
        audit(&s, thread);
    }
    // GTK cannot represent this width; rejected before any native change.
    let wide = ViewTree::new(
        Key(0),
        vec![
            column(0, &[1, 2]),
            text(1, "a").property(Property::Width, Value::Int(i64::from(i32::MAX) + 1)),
            button(2, "b"),
        ],
    );
    let update = s.prepare(thread, Some(wide)).unwrap();
    let failure = s.commit(thread, update).unwrap_err();
    assert!(matches!(
        failure.error,
        CommitError::Backend(GtkError::Unsupported(_))
    ));
    assert_eq!(native(&s, thread), before);
    // GTK strings are NUL-terminated; an embedded NUL is rejected up front.
    let nul = ViewTree::new(
        Key(0),
        vec![column(0, &[1, 2]), text(1, "a\0b"), button(2, "b")],
    );
    let update = s.prepare(thread, Some(nul)).unwrap();
    let failure = s.commit(thread, update).unwrap_err();
    assert!(matches!(
        failure.error,
        CommitError::Backend(GtkError::Unsupported("text contains NUL"))
    ));
    assert_eq!(native(&s, thread), before);
    audit(&s, thread);
    // The retained plan still commits once the fault is gone.
    commit(&mut s, thread, next);
    assert_eq!(native(&s, thread).children[1].width_request, 40);
    assert_eq!(shutdown(s, thread).still_alive, 0);
}

fn close_request_is_routed(thread: &UiThread) {
    let mut s = session(thread);
    commit(&mut s, thread, ViewTree::new(Key(0), vec![text(0, "bye")]));
    let b = backend(&s, thread);
    b.present(thread).unwrap();
    b.pump(thread).unwrap();
    let window = b.gtk_window(thread).unwrap();
    window.close();
    b.pump(thread).unwrap();
    let batch = backend(&s, thread).drain_events(thread).unwrap();
    assert!(batch.close_requested);
    assert!(window.child().is_some(), "the host decides, GTK does not");
    let decision = s.request_close(thread).unwrap();
    match s.resolve_close(thread, decision, CloseAction::Allow) {
        Ok(CloseOutcome::Closed(_)) => {}
        other => panic!("close: {other:?}"),
    }
    assert!(window.child().is_none());
    drop(window);
    assert_eq!(shutdown(s, thread).still_alive, 0);
}

fn other_threads_get_typed_errors(_thread: &UiThread) {
    let result = std::thread::spawn(|| {
        let token = UiThread::claim_current();
        GtkBackend::new(&token, Limits::default()).map(|_| ())
    })
    .join()
    .expect("no panic on the other thread");
    assert_eq!(result, Err(GtkError::NotGtkMainThread));
}

fn teardown_leaves_no_live_widgets(thread: &UiThread) {
    let mut s = session(thread);
    commit(
        &mut s,
        thread,
        ViewTree::new(
            Key(0),
            vec![
                column(0, &[1, 2, 5]),
                text(1, "a"),
                button(2, "b").on(Event::Focused, HandlerKey(9)),
                column(5, &[3]),
                input(3, "c", "d"),
            ],
        ),
    );
    let weak: Vec<glib::WeakRef<gtk::Widget>> = [0, 1, 2, 3, 5]
        .iter()
        .map(|k| widget(&s, thread, *k).downgrade())
        .collect();
    let window = backend(&s, thread).gtk_window(thread).unwrap().downgrade();
    assert!(backend(&s, thread).live_native_objects(thread).unwrap() >= 6);

    let backend_owner = s.shutdown(thread).unwrap();
    assert_eq!(backend_owner.live_native_objects(thread).unwrap(), 1);
    let report = backend_owner.close(thread).map_err(|f| f.error).unwrap();
    assert_eq!(report.still_alive, 0);
    assert!(weak.iter().all(|w| w.upgrade().is_none()));
    assert!(window.upgrade().is_none());

    let mut s = session(thread);
    commit(&mut s, thread, ViewTree::new(Key(0), vec![text(0, "x")]));
    let before = dropped_while_mounted();
    let label = widget(&s, thread, 0).downgrade();
    drop(s); // dropped while mounted: counted, window destroyed
    assert_eq!(dropped_while_mounted(), before + 1);
    assert!(label.upgrade().is_none());
}

type Case = (&'static str, fn(&UiThread));

fn main() {
    let thread = UiThread::claim_current();
    let cases: &[Case] = &[
        ("reconcile_sequence", reconcile_sequence),
        ("keyed_list_transitions", keyed_list_transitions),
        ("callbacks_route_by_key", callbacks_route_by_key),
        (
            "focus_subscriptions_install_controllers",
            focus_subscriptions_install_controllers,
        ),
        (
            "rejected_plans_leave_gtk_untouched",
            rejected_plans_leave_gtk_untouched,
        ),
        ("close_request_is_routed", close_request_is_routed),
        (
            "other_threads_get_typed_errors",
            other_threads_get_typed_errors,
        ),
        (
            "teardown_leaves_no_live_widgets",
            teardown_leaves_no_live_widgets,
        ),
    ];
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let mut ran = 0;
    for (name, case) in cases {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        println!("test {name} ...");
        case(&thread);
        println!("test {name} ... ok");
        ran += 1;
    }
    println!("\ntest result: ok. {ran} passed; 0 failed");
}
