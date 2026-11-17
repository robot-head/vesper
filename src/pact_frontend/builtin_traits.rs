//! Builtin `Eq`, `Ord` and `Hash` traits with derived conformances (port
//! wave P0, lane 5).
//!
//! The traits are ordinary source declarations, injected into a package
//! that names one of them (or `Ordering`) and declares none of those names
//! itself:
//!
//! ```text
//! copy enum Ordering { Less, Equal, Greater }
//! trait Eq   { fn eq(in self, in other: Self) -> bool; }
//! trait Ord  { fn cmp(in self, in other: Self) -> Ordering; }
//! trait Hash { fn hash(in self) -> u64; }
//! ```
//!
//! Canonical conformances exist for `u8`, `u32`, `u64`, `i64`, `bool` and
//! `()` and for tuples of two to four conforming elements. A struct conforms
//! only when the package asks: an empty canonical impl (`impl Ord for Point
//! {}`, or `impl[T] Ord for Pair[T] {}` for a generic struct) derives the
//! conformance, lexicographic in field order like tuples. Every field must
//! conform (a field of another package struct needs that struct's own
//! derived or written impl). A struct parameter that a field mentions gets a
//! where-bound, so `impl[T] Ord for Pair[T] {}` derives
//! `impl[T] Ord for Pair[T] where T: Ord`. The empty-impl spelling needs no
//! new syntax, so every front end (the self-hosted one included) parses it.
//!
//! `bool` orders `false` before `true`. `Hash` is a deterministic
//! multiplicative fold over 64-bit words with the FNV-1a constants
//! (`mix(h, v) = (h + v) * PRIME` modulo 2^64, addition in place of xor so
//! every backend, Lean included, models it): a scalar hashes to
//! `mix(OFFSET, bits)` and a tuple or struct folds its elements' hashes into
//! `OFFSET` in order. It never depends on addresses or iteration order. `f64` has no builtin
//! conformance (NaN has no total order). Lane 8 moves these declarations
//! into `@vesper-std/core`.
//!
//! Unused builtin functions are pruned after elaboration, so a package only
//! carries the conformances it calls.
use crate::ast::*;
use std::collections::BTreeMap;

/// Names a package must not declare for the builtins to be injected.
pub(super) const BUILTIN_NAMES: [&str; 4] = ["Eq", "Ord", "Hash", "Ordering"];
const TRAITS: [&str; 3] = ["Eq", "Ord", "Hash"];
const SCALARS: [&str; 5] = ["u8", "u32", "u64", "i64", "bool"];
/// FNV-1a 64-bit offset basis and prime.
const OFFSET: &str = "14695981039346656037";
const PRIME: &str = "1099511628211";
/// Helper functions of the builtin `Hash` conformances.
/// Prefix of the builtin helper functions.
pub(super) const HELPER: &str = "vesper_builtin_";
const HASH_MIX: &str = "vesper_builtin_hash_mix";
const I64_WORD: &str = "vesper_builtin_i64_word";

/// Whether `module` names a builtin trait or `Ordering` without declaring
/// any of the builtin names itself.
pub(super) fn wanted(module: &Module) -> bool {
    let declares = module.items.iter().any(|i| match i {
        Item::Adt(a) => BUILTIN_NAMES.contains(&a.name.as_str()),
        Item::Trait(t) => BUILTIN_NAMES.contains(&t.name.as_str()),
        Item::Effect(e) => BUILTIN_NAMES.contains(&e.name.as_str()),
        Item::Conformance(c) => matches!(
            &c.kind,
            ConformanceKind::Model { name, .. } if BUILTIN_NAMES.contains(&name.as_str())
        ),
        _ => false,
    });
    if declares {
        return false;
    }
    let named = |r: &TraitRef| r.package.is_none() && TRAITS.contains(&r.name.as_str());
    if module.trait_bounds.iter().any(|b| named(&b.trait_ref)) {
        return true;
    }
    for item in &module.items {
        if let Item::Conformance(c) = item
            && (named(&c.trait_ref) || c.where_clauses.iter().any(|w| named(&w.trait_ref)))
        {
            return true;
        }
    }
    // Path calls (`Ord::cmp(a, b)`), `Ordering` in types and its variants
    // in expressions only reach the module as strings.
    let Ok(text) = serde_json::to_string(&module.items) else {
        return false;
    };
    ["\"Eq::", "\"Ord::", "\"Hash::", "\"Ordering\""]
        .iter()
        .any(|needle| text.contains(needle))
}

/// A hint for a missing builtin conformance of a struct: derivation is
/// opt-in.
pub(super) fn derive_hint(t: &str, receiver: &Type) -> String {
    match receiver {
        Type::Named(n, args) if TRAITS.contains(&t) && !SCALARS.contains(&n.as_str()) => {
            if args.is_empty() {
                format!("; to derive it, write `impl {t} for {n} {{}}`")
            } else {
                format!(
                    "; to derive it, write an empty impl over all of the struct's parameters, such as `impl[T] {t} for {n}[T] {{}}`"
                )
            }
        }
        _ => String::new(),
    }
}

/// An empty canonical impl of a builtin trait asks for the derived
/// conformance.
fn is_request(c: &Conformance) -> bool {
    c.kind == ConformanceKind::Canonical
        && c.trait_ref.package.is_none()
        && TRAITS.contains(&c.trait_ref.name.as_str())
        && c.trait_ref.args.is_empty()
        && c.methods.is_empty()
        && c.assoc.is_empty()
}

/// `module` without its derive requests, the builtin declarations (spans
/// relative to the generated source; the caller relocates them) and, for
/// each declaration, the span of the request it derives.
pub(super) fn prelude(module: &Module) -> Result<Prelude, Vec<Diagnostic>> {
    let derived = derivable(module)?;
    let mut stripped = module.clone();
    stripped
        .items
        .retain(|i| !matches!(i, Item::Conformance(c) if is_request(c)));
    let (items, owners) = items(module, &derived);
    Ok((stripped, items, owners))
}

/// The module without derive requests, the builtin items and their owners.
pub(super) type Prelude = (Module, Vec<Item>, Vec<Option<Span>>);

/// Derived conformances: struct name to (trait, request span).
type Derived = BTreeMap<String, Vec<(&'static str, Span)>>;

fn items(module: &Module, derived: &Derived) -> (Vec<Item>, Vec<Option<Span>>) {
    let mut src = String::new();
    src.push_str("copy enum Ordering { Less, Equal, Greater }\n");
    src.push_str("trait Eq { fn eq(in self, in other: Self) -> bool; }\n");
    src.push_str("trait Ord { fn cmp(in self, in other: Self) -> Ordering; }\n");
    src.push_str("trait Hash { fn hash(in self) -> u64; }\n");
    src.push_str(&format!(
        "fn {HASH_MIX}(h: u64, v: u64) -> u64 {{ wrapping_mul(wrapping_add(h, v), {PRIME}) }}\n"
    ));
    // A scalar's bits as a u64, one binary digit per step with plain
    // arithmetic, so every backend (Lean included) models it; i64 uses two's
    // complement. There is no integer conversion intrinsic yet (lane 2 adds
    // one).
    for t in ["u8", "u32", "i64"] {
        src.push_str(&format!(
            "fn {HELPER}{t}_bits(x: {t}, acc: u64, weight: u64) -> u64 {{ if x == 0 {{ acc }} else {{ {HELPER}{t}_bits(x / 2, if x % 2 == 1 {{ wrapping_add(acc, weight) }} else {{ acc }}, wrapping_mul(weight, 2)) }} }}\n"
        ));
    }
    src.push_str(&format!(
        "fn {I64_WORD}(x: i64) -> u64 {{ if x < 0 {{ wrapping_add({HELPER}i64_bits(wrapping_add(wrapping_add(x, 9223372036854775807), 1), 0, 1), 9223372036854775808) }} else {{ {HELPER}i64_bits(x, 0, 1) }} }}\n"
    ));
    for s in SCALARS {
        let (eq, cmp, bits) = match s {
            "bool" => (
                "self == other".to_owned(),
                "if self == other { Equal } else if other { Less } else { Greater }".to_owned(),
                "if self { 1 } else { 0 }".to_owned(),
            ),
            _ => (
                "self == other".to_owned(),
                "if self < other { Less } else if self == other { Equal } else { Greater }"
                    .to_owned(),
                match s {
                    "i64" => format!("{I64_WORD}(self)"),
                    "u64" => "self".to_owned(),
                    _ => format!("{HELPER}{s}_bits(self, 0, 1)"),
                },
            ),
        };
        src.push_str(&format!(
            "impl Eq for {s} {{ fn eq(in self, in other: {s}) -> bool {{ {eq} }} }}\n\
             impl Ord for {s} {{ fn cmp(in self, in other: {s}) -> Ordering {{ {cmp} }} }}\n\
             impl Hash for {s} {{ fn hash(in self) -> u64 {{ {HASH_MIX}({OFFSET}, {bits}) }} }}\n"
        ));
    }
    src.push_str(
        "impl Eq for () { fn eq(in self, in other: ()) -> bool { true } }\n\
         impl Ord for () { fn cmp(in self, in other: ()) -> Ordering { Equal } }\n",
    );
    src.push_str(&format!(
        "impl Hash for () {{ fn hash(in self) -> u64 {{ {OFFSET} }} }}\n"
    ));
    for n in 2..=4usize {
        let params: Vec<String> = (0..n).map(|i| format!("T{i}")).collect();
        // Elements are copied out of the borrowed tuples by destructuring.
        let generics: Vec<Generic> = params
            .iter()
            .map(|p| Generic {
                name: p.clone(),
                bound: Mode::Copy,
                row: false,
            })
            .collect();
        let ty = format!("({})", params.join(", "));
        for t in TRAITS {
            let wheres: Vec<String> = params.iter().map(|p| format!("{p}: {t}")).collect();
            src.push_str(&conformance(t, &generics, &wheres, &ty, &Access::Tuple(n)));
        }
    }
    let fixed = crate::parser::parse(&src).module.items.len();
    let mut owners: Vec<Option<Span>> = vec![None; fixed];
    for (adt, traits) in derived {
        let a = module
            .items
            .iter()
            .find_map(|i| match i {
                Item::Adt(a) if a.name == *adt => Some(a),
                _ => None,
            })
            .expect("derivable structs come from the module");
        let ty = if a.generics.is_empty() {
            a.name.clone()
        } else {
            format!(
                "{}[{}]",
                a.name,
                a.generics
                    .iter()
                    .map(|g| g.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let fields: Vec<String> = a.variants[0]
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let bounded: Vec<&str> = a
            .generics
            .iter()
            .filter(|g| {
                a.variants[0]
                    .fields
                    .iter()
                    .any(|f| mentions_name(&f.ty, &g.name))
            })
            .map(|g| g.name.as_str())
            .collect();
        // The struct's own parameter bounds hold wherever it is used.
        let own: Vec<String> = module
            .trait_bounds
            .iter()
            .filter(|b| b.owner == a.name)
            .map(|b| {
                let r = &b.trait_ref;
                if r.args.is_empty() {
                    format!("{}: {}", b.param, r.name)
                } else {
                    let args: Vec<String> = r.args.iter().map(ToString::to_string).collect();
                    format!("{}: {}[{}]", b.param, r.name, args.join(", "))
                }
            })
            .collect();
        for (t, request) in traits {
            owners.push(Some(request.clone()));
            let mut wheres: Vec<String> = bounded.iter().map(|p| format!("{p}: {t}")).collect();
            for w in &own {
                if !wheres.contains(w) {
                    wheres.push(w.clone());
                }
            }
            src.push_str(&conformance(
                t,
                &a.generics,
                &wheres,
                &ty,
                &Access::Fields(&fields),
            ));
        }
    }
    let parsed = crate::parser::parse(&src);
    debug_assert!(
        parsed.diagnostics.is_empty(),
        "builtin trait source: {:?}\n{src}",
        parsed.diagnostics
    );
    debug_assert_eq!(parsed.module.items.len(), owners.len());
    (parsed.module.items, owners)
}

/// How a conformance body reaches the components of `self` and `other`.
enum Access<'a> {
    /// Struct fields, projected as `self.f`.
    Fields(&'a [String]),
    /// Tuple elements of copy type, destructured into `a0..` and `b0..`.
    Tuple(usize),
}

/// Source of one canonical conformance of builtin trait `t` for `ty`
/// (lexicographic over the components).
fn conformance(
    t: &str,
    generics: &[Generic],
    wheres: &[String],
    ty: &str,
    access: &Access,
) -> String {
    let (lhs, rhs): (Vec<String>, Vec<String>) = match access {
        Access::Fields(fields) => fields
            .iter()
            .map(|f| (format!("self.{f}"), format!("other.{f}")))
            .unzip(),
        Access::Tuple(n) => (0..*n).map(|i| (format!("a{i}"), format!("b{i}"))).unzip(),
    };
    let wrap = |body: String, both: bool| match access {
        Access::Fields(_) => body,
        Access::Tuple(_) => {
            let inner = if both {
                format!("match other {{ ({}) => {body} }}", rhs.join(", "))
            } else {
                body
            };
            format!("match self {{ ({}) => {inner} }}", lhs.join(", "))
        }
    };
    let head = if generics.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            generics
                .iter()
                .map(|g| match g.bound {
                    Mode::Linear => g.name.clone(),
                    Mode::Affine => format!("{}: mod affine", g.name),
                    Mode::Copy => format!("{}: mod copy", g.name),
                })
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let wheres = if wheres.is_empty() {
        String::new()
    } else {
        format!(" where {}", wheres.join(", "))
    };
    let body = match t {
        "Eq" => {
            let mut body = "true".to_owned();
            for (l, r) in lhs.iter().zip(&rhs).rev() {
                body = format!("if Eq::eq({l}, {r}) {{ {body} }} else {{ false }}");
            }
            format!(
                "fn eq(in self, in other: {ty}) -> bool {{ {} }}",
                wrap(body, true)
            )
        }
        "Ord" => {
            let mut body = "Equal".to_owned();
            for (l, r) in lhs.iter().zip(&rhs).rev() {
                body = if body == "Equal" {
                    format!("Ord::cmp({l}, {r})")
                } else {
                    format!("match Ord::cmp({l}, {r}) {{ Equal => {body}, unequal => unequal }}")
                };
            }
            format!(
                "fn cmp(in self, in other: {ty}) -> Ordering {{ {} }}",
                wrap(body, true)
            )
        }
        _ => {
            let mut body = OFFSET.to_owned();
            for l in &lhs {
                body = format!("{HASH_MIX}({body}, Hash::hash({l}))");
            }
            format!("fn hash(in self) -> u64 {{ {} }}", wrap(body, false))
        }
    };
    format!("impl{head} {t} for {ty}{wheres} {{ {body} }}\n")
}

fn mentions_name(t: &Type, name: &str) -> bool {
    match t {
        Type::Named(n, args) => n == name || args.iter().any(|a| mentions_name(a, name)),
        Type::Tuple(ts) => ts.iter().any(|a| mentions_name(a, name)),
        Type::Function { .. } | Type::Row(_) => true,
        _ => false,
    }
}

/// The derive requests of `module`, checked: each names a package struct
/// (all of its parameters, in order) whose fields all conform.
fn derivable(module: &Module) -> Result<Derived, Vec<Diagnostic>> {
    let structs: BTreeMap<&str, &Adt> = module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Adt(a) if !a.is_enum && !a.generics.iter().any(|g| g.row) => {
                Some((a.name.as_str(), a))
            }
            _ => None,
        })
        .collect();
    // Canonical user impls with bodies: (trait, self type, its generics).
    let user: Vec<(&str, &Type, &[Generic])> = module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Conformance(c)
                if c.kind == ConformanceKind::Canonical
                    && c.trait_ref.package.is_none()
                    && !is_request(c) =>
            {
                Some((c.trait_ref.name.as_str(), &c.self_ty, c.generics.as_slice()))
            }
            _ => None,
        })
        .collect();
    let mut errors = vec![];
    let mut requests: Vec<(&'static str, &str, Span)> = vec![];
    for item in &module.items {
        let Item::Conformance(c) = item else {
            continue;
        };
        if !is_request(c) {
            continue;
        }
        let t = TRAITS
            .into_iter()
            .find(|t| *t == c.trait_ref.name)
            .expect("requests name a builtin trait");
        let target = match &c.self_ty {
            Type::Named(n, args) => structs.get(n.as_str()).filter(|a| {
                args.len() == a.generics.len()
                    && c.generics.len() == args.len()
                    && args.iter().zip(&c.generics).all(
                        |(arg, g)| matches!(arg, Type::Named(p, x) if x.is_empty() && *p == g.name),
                    )
                    && c.where_clauses.is_empty()
            }),
            _ => None,
        };
        let Some(a) = target else {
            errors.push(Diagnostic::new(
                "E0616",
                format!(
                    "an empty `impl {t} for {}` derives {t}, which needs a struct of this package with all its parameters in order and no where clause (for example `impl[T] {t} for Pair[T] {{}}`); write the methods of {t} otherwise",
                    c.self_ty
                ),
                c.span.clone(),
            ));
            continue;
        };
        requests.push((t, a.name.as_str(), c.span.clone()));
    }
    let requested: Vec<(&str, &str)> = requests.iter().map(|(t, n, _)| (*t, *n)).collect();
    let mut out: Derived = BTreeMap::new();
    for (t, name, span) in requests {
        let cx = Cx {
            structs: &structs,
            user: &user,
            requested: &requested,
        };
        let mut memo = BTreeMap::new();
        let a = structs[name];
        let params = params(a);
        if let Some(f) = a.variants[0]
            .fields
            .iter()
            .find(|f| !type_conforms(&f.ty, t, &params, &cx, &mut memo))
        {
            errors.push(Diagnostic::new(
                "E0616",
                format!(
                    "cannot derive {t} for '{name}': field '{}' of type {} does not conform to {t} (derive or write {t} for it first)",
                    f.name, f.ty
                ),
                span,
            ));
            continue;
        }
        out.entry(name.to_owned()).or_default().push((t, span));
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// What conformance of a field type may rely on.
struct Cx<'a> {
    structs: &'a BTreeMap<&'a str, &'a Adt>,
    user: &'a [(&'a str, &'a Type, &'a [Generic])],
    /// Derive requests: (trait, struct).
    requested: &'a [(&'a str, &'a str)],
}

fn params(a: &Adt) -> BTreeMap<&str, Mode> {
    a.generics
        .iter()
        .map(|g| (g.name.as_str(), g.bound))
        .collect()
}

/// Whether a canonical user impl of `t` covers type `ty` (an exact head or
/// a head whose arguments are all its own parameters).
fn user_impl(user: &[(&str, &Type, &[Generic])], t: &str, ty: &Type) -> bool {
    user.iter().any(|(trait_name, self_ty, generics)| {
        *trait_name == t
            && match (self_ty, ty) {
                (Type::Named(n, args), Type::Named(m, _)) if n == m => {
                    args.iter().all(|a| {
                        matches!(a, Type::Named(p, x) if x.is_empty() && generics.iter().any(|g| g.name == *p))
                    }) || *self_ty == ty
                }
                _ => *self_ty == ty,
            }
    })
}

fn struct_conforms(name: &str, t: &str, cx: &Cx, memo: &mut BTreeMap<String, bool>) -> bool {
    if let Some(v) = memo.get(name) {
        return *v;
    }
    // A struct on the current path (only possible through invalid
    // recursion) does not conform.
    memo.insert(name.to_owned(), false);
    let a = cx.structs[name];
    let params = params(a);
    let ok = a.variants[0]
        .fields
        .iter()
        .all(|f| type_conforms(&f.ty, t, &params, cx, memo));
    memo.insert(name.to_owned(), ok);
    ok
}

fn type_conforms(
    ty: &Type,
    t: &str,
    params: &BTreeMap<&str, Mode>,
    cx: &Cx,
    memo: &mut BTreeMap<String, bool>,
) -> bool {
    match ty {
        Type::Named(n, args) if args.is_empty() && params.contains_key(n.as_str()) => true,
        Type::Named(n, args) if args.is_empty() && SCALARS.contains(&n.as_str()) => true,
        Type::Tuple(ts) => {
            (ts.is_empty() || (2..=4).contains(&ts.len()))
                && ts.iter().all(|e| {
                    copy_type(e, params, cx.structs) && type_conforms(e, t, params, cx, memo)
                })
        }
        Type::Named(n, args) => {
            if user_impl(cx.user, t, ty) {
                return true;
            }
            let Some(a) = cx.structs.get(n.as_str()) else {
                return false;
            };
            if !cx.requested.contains(&(t, n.as_str())) {
                return false;
            }
            // A generic struct conforms at these arguments when each
            // argument for a field-mentioned parameter conforms.
            if !struct_conforms(n, t, cx, memo) {
                return false;
            }
            a.generics.iter().zip(args).all(|(g, arg)| {
                !a.variants[0]
                    .fields
                    .iter()
                    .any(|f| mentions_name(&f.ty, &g.name))
                    || type_conforms(arg, t, params, cx, memo)
            })
        }
        _ => false,
    }
}

/// Whether `ty` is certainly copy (the builtin tuple conformances bound
/// their elements by `mod copy`).
fn copy_type(ty: &Type, params: &BTreeMap<&str, Mode>, structs: &BTreeMap<&str, &Adt>) -> bool {
    match ty {
        Type::Named(n, args) if args.is_empty() && params.contains_key(n.as_str()) => {
            params[n.as_str()] == Mode::Copy
        }
        Type::Named(n, _) if SCALARS.contains(&n.as_str()) => true,
        Type::Tuple(ts) => ts.iter().all(|e| copy_type(e, params, structs)),
        Type::Named(n, _) => structs
            .get(n.as_str())
            .is_some_and(|a| a.mode == Mode::Copy),
        _ => false,
    }
}
