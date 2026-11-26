//! Methods on the builtin `Option[T]`, in every module.
//!
//! `x.is_some()`, `x.is_none()`, `x.unwrap_or(fallback)` and
//! `x.or(other)` read like Rust's, so a lookup can chain:
//!
//! ```text
//! let y = order.try_get(k).unwrap_or(x);
//! ```
//!
//! They are ordinary Vesper in the option prelude ([`PRELUDE`]), added to
//! a module that mentions one of their names, as the string prelude is, so
//! every backend runs them without dedicated support and no package has to
//! depend on `@vesper-std/core` for them. A method a module defines itself
//! in an `impl Option[T]` block (as `@vesper-std/core` does) replaces the
//! prelude's.

use crate::ast::*;
use std::collections::BTreeSet;

/// The option prelude.
pub const PRELUDE: &str = include_str!("preludes/option.ves");

/// The method names that bring in the prelude when a module mentions them.
pub const PRELUDE_NAMES: &[&str] = &["is_some", "is_none", "unwrap_or", "or"];

/// The function bodies of `module`, and the `Option` methods it defines.
fn scan(module: &Module) -> (Vec<&Expr>, BTreeSet<String>) {
    let mut bodies = vec![];
    let mut defined = BTreeSet::new();
    for item in &module.items {
        let functions: Vec<&Function> = match item {
            Item::Function(f) => vec![&**f],
            Item::Impl { ty, methods, .. } => {
                if ty.name() == "Option" {
                    defined.extend(methods.iter().map(|m| m.name.clone()));
                }
                methods.iter().collect()
            }
            Item::Trait(t) => t.methods.iter().collect(),
            Item::Conformance(c) => c.methods.iter().collect(),
            Item::Adt(_) | Item::Effect(_) => vec![],
        };
        bodies.extend(functions.into_iter().filter_map(|f| f.body.as_ref()));
    }
    (bodies, defined)
}

/// The option prelude as a module whose spans all lie after `after` and
/// after every span of `module`, or an empty module when `module` mentions
/// none of its names or defines all of them itself.
pub(crate) fn prelude(module: &Module, after: usize) -> Module {
    let (bodies, defined) = scan(module);
    let mut used = BTreeSet::new();
    for body in bodies {
        crate::capability_effects::used_names(body, &mut used);
    }
    if !PRELUDE_NAMES
        .iter()
        .any(|n| used.contains(*n) && !defined.contains(*n))
    {
        return Module::default();
    }
    let end = after.max(crate::integer_widths::module_end(module));
    let mut parsed = crate::integer_widths::parse_at(PRELUDE, end + 1);
    debug_assert!(
        parsed.diagnostics.is_empty(),
        "option prelude: {:?}",
        parsed.diagnostics
    );
    for item in &mut parsed.module.items {
        if let Item::Impl { methods, .. } = item {
            methods.retain(|m| !defined.contains(&m.name));
        }
    }
    parsed.module
}

#[cfg(test)]
mod tests {
    use super::*;

    fn methods(source: &str) -> Vec<String> {
        let module = crate::parser::parse(source).module;
        prelude(&module, 0)
            .items
            .iter()
            .flat_map(|item| match item {
                Item::Impl { methods, .. } => methods.iter().map(|m| m.name.clone()).collect(),
                _ => vec![],
            })
            .collect()
    }

    #[test]
    fn added_only_when_mentioned() {
        assert!(methods("fn f(x: u64) -> u64 { x }").is_empty());
        assert_eq!(
            methods("fn f(o: Option[u64]) -> u64 { o.unwrap_or(0) }"),
            ["is_some", "is_none", "unwrap_or", "or"]
        );
    }

    #[test]
    fn a_method_the_module_defines_replaces_the_preludes() {
        let source =
            "impl[T: mod copy] Option[T] { fn unwrap_or(self, fallback: T) -> T { fallback } }
            fn f(o: Option[u64]) -> u64 { o.or(Option::None).unwrap_or(0) }";
        assert_eq!(methods(source), ["is_some", "is_none", "or"]);
    }

    #[test]
    fn a_module_without_core_checks_its_calls() {
        let source = "fn pick(a: Option[u64], b: Option[u64]) -> u64 {
                if a.is_none() && b.is_some() { b.unwrap_or(1) } else { a.or(b).unwrap_or(0) }
            }";
        let checked = crate::check::check(&crate::parser::parse(source).module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    }
}
