//! Runs the host reconciliation contract, without opening a native window.
use vesper_aura::*;

fn view(value: u64) -> ViewTree {
    ViewTree::new(
        Key(0),
        vec![
            Node::new(Key(0), Kind::Container).children([Key(1), Key(2)]),
            Node::new(Key(1), Kind::Text).property(Property::Text, Value::Text(value.to_string())),
            Node::new(Key(2), Kind::Button)
                .property(Property::Text, Value::Text("Increment".into()))
                .on(Event::Click, HandlerKey(1)),
        ],
    )
}
fn main() {
    let ui = UiThread::claim_current();
    let limits = Limits::default();
    let backend = RecordingBackend::new(&ui, limits.clone());
    let mut app = Session::new(&ui, backend, limits).unwrap();
    let mounted = app.prepare(&ui, Some(view(0))).unwrap();
    app.commit(&ui, mounted).unwrap();
    let registration = app.subscription(Key(2), Event::Click).unwrap();
    assert_eq!(
        app.backend(&ui).unwrap().dispatch(&ui, registration),
        Ok(HandlerKey(1))
    );
    let updated = app.prepare(&ui, Some(view(1))).unwrap();
    assert_eq!(updated.transaction().patches().len(), 1);
    app.commit(&ui, updated).unwrap();
    println!(
        "{:?}",
        app.backend(&ui).unwrap().snapshot(&ui).unwrap().last_events
    );
    let decision = app.request_close(&ui).unwrap();
    app.resolve_close(&ui, decision, CloseAction::Allow)
        .unwrap();
    let backend = app.shutdown(&ui).unwrap();
    let done = backend.snapshot(&ui).unwrap();
    assert!(done.widgets.is_empty());
    assert_eq!(done.created_subscriptions, done.disconnected_subscriptions);
}
