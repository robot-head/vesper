//! The Vesper build (port wave P1 lane 13, #910): the public API forwards
//! to the Loom package in vesper/ through the committed binding in port/binding.rs.
//! These wrappers keep the Rust-only parts: the thread-confined owners
//! (`UiThread` checks, `PhantomData<Rc<()>>`), the generic `Backend`
//! adaptor that `Session<B>` applies a plan to, and the conversions in
//! [`codec`].

// Generated and committed, outside src/ (see the porting recipe).
#[rustfmt::skip]
#[path = "../../port/binding.rs"]
mod binding;

mod codec;
mod recording;
mod session;
mod sim_aura;

pub use recording::RecordingBackend;
pub use session::{CloseDecision, PreparedUpdate, Session};
pub use sim_aura::{SimAura, audit_view_tree, encode_view_tree};

/// The allocation counter is process-wide, so every unit test that calls
/// into the port holds this lock while the leak test measures.
#[cfg(test)]
pub(crate) static PORT_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::binding::live_allocations;
    use crate::{
        Backend, CloseAction, CloseOutcome, Event, HandlerKey, Key, Kind, Limits, Node, Property,
        RecordingBackend, Session, SimAura, UiThread, Value, ViewTree, audit_view_tree,
        encode_view_tree,
    };

    fn tree(round: u64) -> ViewTree {
        let keys: Vec<Key> = (0..4).map(|i| Key(10 + (i + round) % 4)).collect();
        let mut nodes = vec![Node::new(Key(1), Kind::Container).children(keys.clone())];
        for (i, key) in keys.iter().enumerate() {
            nodes.push(
                Node::new(*key, Kind::Button)
                    .property(
                        Property::Text,
                        Value::Text(format!("b{}", round + i as u64)),
                    )
                    .property(Property::Width, Value::Int(round as i64))
                    .on(Event::Click, HandlerKey(round % 3)),
            );
        }
        ViewTree::new(Key(1), nodes)
    }

    /// Runs 50 update rounds and a close, returning the most allocations
    /// live after a commit.
    fn cycle<B: Backend + std::fmt::Debug>(ui: &UiThread, backend: B) -> i64
    where
        B::Error: std::fmt::Debug,
    {
        let limits = Limits::default();
        let mut app = Session::new(ui, backend, limits.clone()).unwrap();
        let mut peak = 0;
        for round in 0..50 {
            let update = app.prepare(ui, Some(tree(round))).unwrap();
            // A discarded alternative frees everything it reserved.
            drop(app.prepare(ui, Some(tree(round + 7))).unwrap());
            app.commit(ui, update).unwrap();
            peak = peak.max(live_allocations());
            encode_view_tree(&tree(round), &limits).unwrap();
            audit_view_tree(&tree(round), &limits).unwrap();
        }
        let decision = app.request_close(ui).unwrap();
        assert!(matches!(
            app.resolve_close(ui, decision, CloseAction::Allow),
            Ok(CloseOutcome::Closed(_))
        ));
        drop(app.shutdown(ui).unwrap());
        peak
    }

    // Native code does not free a dropped local sequence yet, so the port
    // keeps every allocation in a handle; this checks that dropping the
    // handles returns the live count to where it started.
    #[test]
    fn port_frees_every_allocation_it_makes() {
        let _serial = super::PORT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let ui = UiThread::claim_current();
        let before = live_allocations();
        let backend = RecordingBackend::new(&ui, Limits::default());
        // Empty sequences allocate nothing (#1021), so the counter is
        // checked once the session holds a committed tree.
        assert!(cycle(&ui, backend) > before, "the counter does not count");
        let mut sim = SimAura::with_wcag_gate(&ui, Limits::default());
        sim.focus_next(&ui).unwrap_err();
        cycle(&ui, sim);
        assert_eq!(live_allocations(), before, "the port leaked");
    }
}
