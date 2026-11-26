// Sugar expansion (SH1 lane 14, #1157), from `modules/sugar`:
// `vesper_syntax::sugar_expand::expand_sugar_with_scopes_for_execution`,
// the driver behind package sugar imports (src/packages/sugar.rs) and the
// forge front end (src/forge_frontend), built by `@vesper/sugar`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/sugar.rs"]
mod sugar_binding;

#[path = "../../../../tests/common/sugar_words.rs"]
mod sugar_words;

use super::{Registry, Swap, must, on, pass, stats};

/// Expansions run by the Vesper driver.
static SUGAR_CALLS: AtomicU64 = AtomicU64::new(0);
/// Expansions left to Rust: a length the input's numbers cannot hold.
static SUGAR_KEPT: AtomicU64 = AtomicU64::new(0);
/// The sugar swap's costs.
static COST: stats::Cost = stats::Cost::new("sugar");

/// `vesper_syntax::sugar_expand::SugarExpandTable` from `modules/sugar`.
const VESPER_SUGAR: vesper_syntax::sugar_expand::SugarExpandTable =
    vesper_syntax::sugar_expand::SugarExpandTable {
        expand: |source, imports, scopes, visible, include_tests| {
            let rust = || {
                vesper_syntax::sugar_expand::rust_expand_sugar(
                    source,
                    imports,
                    scopes,
                    visible,
                    include_tests,
                )
            };
            let timer = stats::Timer::start();
            let input = sugar_words::input(source, imports, scopes, visible);
            let Some(input) = input else {
                SUGAR_KEPT.fetch_add(1, Relaxed);
                return rust();
            };
            let timer = timer.lap(&COST.encode);
            SUGAR_CALLS.fetch_add(1, Relaxed);
            let words = sugar_binding::expand_words(&input, u64::from(include_tests));
            let timer = timer.lap(&COST.vesper);
            let expanded = sugar_words::decode(&words);
            timer.lap(&COST.decode);
            COST.call(false, rust, |r| *r == expanded);
            expanded
        },
    };

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[pass("sugar", "sugar", "#1157")];

pub(super) fn register(_r: &mut Registry) {
    if on("sugar") {
        // The input layout `sugar_words::input` writes must be the module's.
        if sugar_binding::format() != 1 {
            std::process::abort();
        }
        must(vesper_syntax::sugar_expand::install_sugar_expand(
            VESPER_SUGAR,
        ));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: sugar expanded {} sources from Vesper, {} kept on Rust",
        SUGAR_CALLS.load(Relaxed),
        SUGAR_KEPT.load(Relaxed),
    );
    COST.report();
}
