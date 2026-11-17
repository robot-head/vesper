//! Integer widths (`vesper.arith.checked/1`): the byte-order prelude.
//!
//! The integer kinds, operators, conversions and byte-order intrinsics are
//! specified in [`crate::numeric`]. The checker rules live in
//! `src/integer_widths/check.rs`. Each byte-order intrinsic
//! (`to_le_bytes_u32`, `from_be_bytes_i16`, ...) is an ordinary source
//! function over shifts, `|` and conversions, added to the module when the
//! module mentions it and does not define a function of that name, so every
//! backend runs it without dedicated support.
use crate::ast::*;
use std::collections::BTreeSet;

fn item_end(item: &Item) -> usize {
    match item {
        Item::Adt(a) => a.span.end,
        Item::Function(f) => f.span.end,
        Item::Effect(e) => e.span.end,
        Item::Impl { methods, .. } => methods.iter().map(|m| m.span.end).max().unwrap_or(0),
        Item::Trait(t) => t.span.end,
        Item::Conformance(c) => c.span.end,
    }
}

/// The byte-order functions `module` uses, as a module whose spans all lie
/// after `after` and after every span of `module`.
pub(crate) fn prelude(module: &Module, after: usize) -> Module {
    let mut used = BTreeSet::new();
    let mut defined = BTreeSet::new();
    let mut end = after;
    for item in &module.items {
        end = end.max(item_end(item));
        let functions: Vec<&Function> = match item {
            Item::Function(f) => {
                defined.insert(f.name.clone());
                vec![f]
            }
            Item::Impl { methods, .. } => methods.iter().collect(),
            Item::Trait(t) => t.methods.iter().collect(),
            Item::Conformance(c) => c.methods.iter().collect(),
            Item::Adt(_) | Item::Effect(_) => vec![],
        };
        for f in functions {
            if let Some(body) = &f.body {
                crate::capability_effects::used_names(body, &mut used);
            }
        }
    }
    let source: String = used
        .iter()
        .filter(|n| !defined.contains(*n))
        .filter_map(|n| crate::numeric::byte_order_source(n))
        .map(|f| f + "\n")
        .collect();
    if source.is_empty() {
        return Module::default();
    }
    let padded = format!("{}{source}", " ".repeat(end + 1));
    let parsed = crate::parser::parse(&padded);
    debug_assert!(
        parsed.diagnostics.is_empty(),
        "byte-order prelude: {:?}",
        parsed.diagnostics
    );
    parsed.module
}

/// The largest span end in `module`.
pub(crate) fn module_end(module: &Module) -> usize {
    module.items.iter().map(item_end).max().unwrap_or(0)
}
