//! The `chroma` tool facts: Chroma's Tier 1 semantic facts for
//! `vesper highlight` and `vesper shot` (design §43), gathered read-only from
//! the parser's AST and the checker's outputs. Moved from
//! crates/vesper-tools/src/highlight/facts.rs (#2670): the tool now reads
//! them from `vesper tool-facts chroma`. The records mirror
//! `vesper_highlight::SemanticFacts`, which the compiler does not link.
use serde::Serialize;
use std::ops::Range;
use vesper::{
    ast::{
        Expr, ExprKind, Function, Item, Mode as ValueMode, Module, Pass, Pattern, PatternKind,
        Statement, Type, substitute,
    },
    check::World,
    tooling::frontend::CheckedSource,
};

/// A live linear binding: where it is minted, used and discharged.
#[derive(Serialize)]
pub(super) struct LinearBindingFact {
    name: String,
    mint: Range<usize>,
    occurrences: Vec<Range<usize>>,
    discharge: Option<Range<usize>>,
    hover: Option<u32>,
}

/// A FieldTree leaf, mutated or read-only.
#[derive(Serialize)]
pub(super) struct FieldLeafFact {
    span: Range<usize>,
    mutated: bool,
}

/// A declared effect and whether it is linear (versus abortable).
#[derive(Serialize)]
pub(super) struct EffectFact {
    name: String,
    linear: bool,
}

/// Hover text for a source span.
#[derive(Serialize)]
pub(super) struct HoverEntry {
    span: Range<usize>,
    /// Boxed like `vesper_highlight`'s `SmolStr`, so the shared collector converts.
    text: Box<str>,
}

/// Everything Tier 1 highlighting needs.
#[derive(Default, Serialize)]
pub(super) struct SemanticFacts {
    linear_bindings: Vec<LinearBindingFact>,
    field_leaves: Vec<FieldLeafFact>,
    effects: Vec<EffectFact>,
    hovers: Vec<HoverEntry>,
}

#[path = "../highlight_facts.rs"]
mod collector;
/// Chroma's Tier 1 facts about `source`, as the serializable records.
pub(super) fn collect_facts(source: &str, checked: &CheckedSource) -> SemanticFacts {
    collector::collect_facts(source, checked)
}
