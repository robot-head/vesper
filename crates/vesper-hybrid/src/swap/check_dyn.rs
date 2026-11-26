// The dyn lowering (`dyn_fn::lower` and `dyn_trait::lower`, front-end
// wiring slice 4, #1976), from `modules/check_dyn`: `@vesper/check-dyn` on
// the checker AST crossing (#1910).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/check_dyn.rs"]
mod check_dyn_binding;

// The module as the `ast` section of the `parse_words` stream (#1910).
#[path = "../../../../tests/common/ast_words.rs"]
mod ast_words;

// The lowered module's `ast` section, read back with the parse swap's
// reader (`dyn_words` reads it through `super::parse_words`).
use super::parse::parse_words;

// The outcome's decoder, shared with tests/self_host_check_dyn.rs.
#[path = "../../../../tests/common/dyn_words.rs"]
mod dyn_words;

use super::{Registry, Swap, must, on, open, stats};

/// Modules the Vesper dyn module lowered, per pass.
static DYN_FN_CALLS: AtomicU64 = AtomicU64::new(0);
static DYN_TRAIT_CALLS: AtomicU64 = AtomicU64::new(0);
/// The swaps' costs.
static DYN_FN_COST: stats::Cost = stats::Cost::new("dyn_fn");
static DYN_TRAIT_COST: stats::Cost = stats::Cost::new("dyn_trait");

/// Lowered modules agree when their items and side tables do.
fn same_fn(a: &vesper::dyn_fn::Lowered, b: &vesper::dyn_fn::Lowered) -> bool {
    a.module == b.module && a.base == b.base && a.end == b.end && a.origins == b.origins
}

fn same_trait(a: &vesper::dyn_trait::Lowered, b: &vesper::dyn_trait::Lowered) -> bool {
    a.module == b.module
        && a.params == b.params
        && a.boxed == b.boxed
        && format!("{:?}", a.forwards) == format!("{:?}", b.forwards)
}

/// `vesper::dyn_fn::DynFnTable` from `modules/check_dyn`.
const VESPER_DYN_FN: vesper::dyn_fn::DynFnTable = vesper::dyn_fn::DynFnTable {
    lower: |module| {
        DYN_FN_CALLS.fetch_add(1, Relaxed);
        let timer = stats::Timer::start();
        let base = vesper::dyn_fn::span_base(module);
        let input = ast_words::module_words(module);
        let bytes: Vec<u8> = input.iter().flat_map(|w| w.to_le_bytes()).collect();
        let timer = timer.lap(&DYN_FN_COST.encode);
        let words = check_dyn_binding::dyn_fn_lower(&bytes, base as u64);
        let timer = timer.lap(&DYN_FN_COST.vesper);
        let lowered = dyn_words::decode_dyn_fn(module, base, &words);
        timer.lap(&DYN_FN_COST.decode);
        DYN_FN_COST.call(
            false,
            || vesper::dyn_fn::rust_lower(module),
            |rust| match (rust, &lowered) {
                (Ok(a), Ok(b)) => same_fn(a, b),
                (Err(a), Err(b)) => a == b,
                _ => false,
            },
        );
        Some(lowered)
    },
};

/// `vesper::dyn_trait::DynTraitTable` from `modules/check_dyn`.
const VESPER_DYN_TRAIT: vesper::dyn_trait::DynTraitTable = vesper::dyn_trait::DynTraitTable {
    lower: |module| {
        DYN_TRAIT_CALLS.fetch_add(1, Relaxed);
        let timer = stats::Timer::start();
        let input = ast_words::module_words(module);
        let bytes: Vec<u8> = input.iter().flat_map(|w| w.to_le_bytes()).collect();
        let timer = timer.lap(&DYN_TRAIT_COST.encode);
        let words = check_dyn_binding::dyn_trait_lower(&bytes);
        let timer = timer.lap(&DYN_TRAIT_COST.vesper);
        let lowered = dyn_words::decode_dyn_trait(module, &words);
        timer.lap(&DYN_TRAIT_COST.decode);
        DYN_TRAIT_COST.call(
            false,
            || vesper::dyn_trait::rust_lower(module),
            |rust| match (rust, &lowered) {
                (Ok(a), Ok(b)) => same_trait(a, b),
                (Err(a), Err(b)) => a == b,
                _ => false,
            },
        );
        Some(lowered)
    },
};

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open(
        "dyn_fn",
        "1.09x with dyn_trait (95% CI 1.07-1.10x), 0 diffs; no flip-gate configuration",
    ),
    open(
        "dyn_trait",
        "1.09x with dyn_fn (95% CI 1.07-1.10x), 0 diffs; no flip-gate configuration",
    ),
];

pub(super) fn register(_r: &mut Registry) {
    if on("dyn_fn") {
        must(vesper::dyn_fn::install_dyn_fn(VESPER_DYN_FN));
    }
    if on("dyn_trait") {
        must(vesper::dyn_trait::install_dyn_trait(VESPER_DYN_TRAIT));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: dyn_fn lowered {} modules from Vesper, dyn_trait {}",
        DYN_FN_CALLS.load(Relaxed),
        DYN_TRAIT_CALLS.load(Relaxed),
    );
    DYN_FN_COST.report();
    DYN_TRAIT_COST.report();
}
