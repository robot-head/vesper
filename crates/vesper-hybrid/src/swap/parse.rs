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
