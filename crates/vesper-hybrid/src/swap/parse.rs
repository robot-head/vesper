// The parser (#1155), from `modules/parse`: the front end v2 and the
// AST of `@vesper/ast`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/parse.rs"]
mod parse_binding;

#[path = "../parse_words.rs"]
mod parse_words;

use super::{Registry, Swap, must, on, open, stats};

/// Sources parsed by the Vesper parse module.
static PARSE_CALLS: AtomicU64 = AtomicU64::new(0);
/// The parse swap's costs.
static COST: stats::Cost = stats::Cost::new("parse");

/// `vesper_syntax::parser::ParseTable` from `modules/parse`: the front end
/// v2 and the AST of `@vesper/ast`, read back by `parse_words::decode`.
const VESPER_PARSE: vesper_syntax::parser::ParseTable = vesper_syntax::parser::ParseTable {
    parse: |source| {
        PARSE_CALLS.fetch_add(1, Relaxed);
        // The source is lent in place: the parse swap has no input to
        // encode, only its AST words to decode.
        let timer = stats::Timer::start();
        let words = parse_binding::parse_words(source);
        let timer = timer.lap(&COST.vesper);
        let parsed = parse_words::decode(source, &words);
        timer.lap(&COST.decode);
        COST.call(
            false,
            || vesper_syntax::parser::rust_parse(source),
            |rust| *rust == parsed,
        );
        parsed
    },
};

thread_local! {
    /// When the Vesper driver started its own parse (`driver_parse_begin`),
    /// with stats on.
    static DRIVER_PARSE_START: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Whether the Vesper driver parses a standalone source itself: only with
/// the parse swap installed (`VESPER_HYBRID_SWAPS`), since its parse is the
/// swap's. Starts the clock of the parse's Vesper time when stats are on.
pub(crate) fn driver_parse_begin() -> bool {
    let on = on("parse");
    if on && stats::level() > 0 {
        DRIVER_PARSE_START.with(|t| t.set(Some(std::time::Instant::now())));
    }
    on
}

/// A parse the Vesper driver ran (`modules/driver/src/front.ves`), read
/// back from its `parse_words` stream like the parse swap's; its Vesper
/// time runs from `driver_parse_begin`.
pub(crate) fn driver_parse(source: &str, words: &[u64]) -> vesper_syntax::parser::Parse {
    PARSE_CALLS.fetch_add(1, Relaxed);
    if let Some(start) = DRIVER_PARSE_START.with(|t| t.take()) {
        COST.vesper
            .fetch_add(start.elapsed().as_nanos() as u64, Relaxed);
    }
    let timer = stats::Timer::start();
    let parsed = parse_words::decode(source, words);
    timer.lap(&COST.decode);
    COST.call(
        false,
        || vesper_syntax::parser::rust_parse(source),
        |rust| *rust == parsed,
    );
    parsed
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("parse", "0 diffs; no flip-gate configuration")];

pub(super) fn register(_r: &mut Registry) {
    if on("parse") {
        must(vesper_syntax::parser::install_parse(VESPER_PARSE));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: parse answered {} calls from Vesper",
        PARSE_CALLS.load(Relaxed),
    );
    COST.report();
}
