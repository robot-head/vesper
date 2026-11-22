//! `f32`, float conversions and math intrinsics (`vesper.float.ieee754/1`):
//! the transcendental prelude.
//!
//! The float kinds, operators, intrinsics and conversions are specified in
//! [`crate::numeric::float`]. The checker rules live in
//! `src/float_math/check.rs`. Each transcendental function (`exp_f64`,
//! `sin_f32`, ...) is an ordinary source function built only from
//! correctly rounded IEEE operations, integer operations and bit
//! reinterpretation, added to the module when the module mentions it and does
//! not define a function of that name. Every backend therefore runs it
//! without dedicated support and computes the same bits on every target,
//! instead of calling the platform's libm.
use crate::ast::*;
use crate::numeric::float::{transcendental, transcendental_closure, transcendental_source};
use std::collections::BTreeSet;

/// Whether `name` is one of the prelude's source functions (a
/// transcendental or one of its helpers).
pub fn is_prelude_function(name: &str) -> bool {
    transcendental_source(name).is_some()
}

/// The capability effects a prelude function carries implicitly. Its body
/// uses only correctly rounded IEEE operations, is not recursive and does
/// not allocate, so `eff [DetFloat]` and `eff [Realtime]` code may call it.
/// It is not declared with them, so that using it does not turn on NaN
/// canonicalisation for the whole program. Only the generated definitions
/// carry them: a user function of the same name keeps its declared effects.
pub fn carried_effects(world: &crate::check::World, name: &str, effects: &Effects) -> Effects {
    if !world.float_prelude.contains(name) || !is_prelude_function(name) {
        return effects.clone();
    }
    let mut out = effects.clone();
    for cap in [
        crate::capability_effects::DET_FLOAT,
        crate::capability_effects::REALTIME,
    ] {
        if !out.instances.iter().any(|e| e.name == cap) {
            out.instances.push(Effect {
                label: None,
                name: cap.into(),
                args: vec![],
            });
        }
    }
    out
}

/// The transcendental functions `module` uses, with their helpers, as a
/// module whose spans all lie after `after` and after every span of
/// `module`.
pub(crate) fn prelude(module: &Module, after: usize) -> Module {
    let mut used = BTreeSet::new();
    let mut defined = BTreeSet::new();
    let end = after.max(crate::integer_widths::module_end(module));
    for item in &module.items {
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
    let mut names: Vec<&'static str> = vec![];
    for n in used.iter().filter(|n| !defined.contains(*n)) {
        if transcendental(n).is_none() {
            continue;
        }
        for f in transcendental_closure(n) {
            if !defined.contains(f) && !names.contains(&f) {
                names.push(f);
            }
        }
    }
    if names.is_empty() {
        return Module::default();
    }
    let source: String = names
        .iter()
        .filter_map(|n| transcendental_source(n))
        .map(|f| format!("{f}\n"))
        .collect();
    let parsed = crate::integer_widths::parse_at(&source, end + 1);
    debug_assert!(
        parsed.diagnostics.is_empty(),
        "float prelude: {:?}",
        parsed.diagnostics
    );
    parsed.module
}

/// The typed refusal of Lean extraction for the float intrinsic `name`, or
/// `None` when `name` is not one. Lean models `f64` as `Float` and `f32` as
/// `Float32` for the basic operations only.
pub fn lean_refusal(name: &str) -> Option<String> {
    crate::numeric::float::is_float_intrinsic(name).then(|| {
        format!(
            "Lean extraction does not model the float intrinsic '{name}' yet (vesper.float.ieee754/1: only + - * /, negation and comparisons of f32 and f64 are modeled)"
        )
    })
}
