//! Leak check of the kit's export ABI v2 handles and sequences (#898): 10^5
//! cycles of every owning form, after which the library's live sequence and
//! box allocations are back where they started. The divergent build omits
//! one Vesper-side free, and this test must fail against it, so it checks
//! only the counters (the other divergence edits change values). It is the only
//! test in this binary, so no other test allocates while it counts.
use vesper_port_pilot::kit::patterns::TokenSlab;
use vesper_port_pilot::kit::*;

#[test]
fn handles_and_sequences_return_every_allocation() {
    let before = live_allocations();
    let mut slab = TokenSlab::new();
    for i in 0..100_000u64 {
        let mut c = Counter::new();
        c.record(i);
        c.record(1);
        assert_eq!(c.hits().len(), 2);
        c.reset();
        c.record(2);
        drop(c);
        assert_eq!(squares(3).len(), 3);
        let mut xs = vec![1];
        push_squares(&mut xs, 2);
        assert_eq!(xs.len(), 3);
        let _ = Tree::build(2).sum();
        drop(Tree::build(1));
        let _ = Ticket::open(i).close();
        slab.push(i);
        if i % 2 == 1 {
            slab.pop();
            slab.pop();
        }
        assert_eq!(plan_sends(1, 1, 1).len(), 2);
    }
    drop(slab);
    assert_eq!(
        live_allocations(),
        before,
        "Vesper allocations leaked over 10^5 cycles"
    );
}
