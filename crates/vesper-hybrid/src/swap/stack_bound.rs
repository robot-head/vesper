// The stack-bound report (SH2 lane 10, #1182), from `modules/stack_bound`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/stack_bound.rs"]
mod stack_bound_binding;

// The stack-bound swap's encoder and decoder (#1182), shared with the root
// package's tests/self_host_check_loops_atomics.rs.
#[path = "../../../../tests/common/stack_words.rs"]
mod stack_words;

use super::{Registry, Swap, must, on, open};

/// Reports made by the Vesper stack-bound module.
static STACK_BOUND_CALLS: AtomicU64 = AtomicU64::new(0);

/// `vesper::stack_bound::StackBoundTable` from `modules/stack_bound`: the
/// stack-bound report of `@vesper/check-loops-atomics` (`vesper ebpf check`
/// and `--verify-stack`).
const VESPER_STACK_BOUND: vesper::stack_bound::StackBoundTable =
    vesper::stack_bound::StackBoundTable {
        report: |graph, policy| {
            STACK_BOUND_CALLS.fetch_add(1, Relaxed);
            let input = stack_words::stack_input(graph, policy);
            stack_words::decode_stack_report(
                graph.origin,
                &stack_bound_binding::report_words(&input),
            )
        },
    };

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("stack_bound", "2.54x, above the bar")];

pub(super) fn register(_r: &mut Registry) {
    if on("stack_bound") {
        must(vesper::stack_bound::install_stack_bound(VESPER_STACK_BOUND));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: stack-bound answered {} calls from Vesper",
        STACK_BOUND_CALLS.load(Relaxed),
    );
}
