//! Bidirectional type/mode inference and path-sensitive place capability checking.
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
#[path = "vector/check.rs"]
mod vector_check;

#[path = "regions/check.rs"]
mod region_rules;

#[path = "float_math/check.rs"]
mod float_math_rules;
#[path = "integer_widths/check.rs"]
mod integer_widths_rules;
#[path = "tiled/check.rs"]
mod tiled_rules;

#[path = "arrays/check.rs"]
mod array_rules;
#[path = "check/call_facts.rs"]
mod call_facts;
#[path = "loops/for_sequences.rs"]
mod for_sequences;
#[path = "integer_widths/literal_typing.rs"]
mod literal_typing;
#[path = "loops/check.rs"]
mod loop_rules;
#[path = "loops/try_option.rs"]
mod try_option;
#[path = "check/world_mode.rs"]
mod world_mode;

const TRY_OPERAND: &str = "'?' requires Result[T, E], or Option[T] in a function returning Option";
#[path = "seq/check.rs"]
mod seq_rules;
#[path = "strings/check.rs"]
mod string_rules;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct World {
    pub adts: BTreeMap<String, Adt>,
    pub functions: BTreeMap<String, Function>,
    pub effects: BTreeMap<String, EffectDecl>,
    pub types: TypeMap,
    pub moves: BTreeMap<(usize, usize), Vec<Place>>,
    pub function_results: BTreeMap<String, Type>,
    pub handler_effects: BTreeMap<(usize, usize), Effects>,
    /// Primitive operations which can raise Fail[RuntimeError].
    pub partial_ops: BTreeSet<(usize, usize)>,
    /// Checked integer operations which can raise Fail[ArithOverflow]. Spans
    /// absent here were proved in range (see `vesper.arith.checked/1`).
    pub overflow_ops: BTreeSet<(usize, usize)>,
    /// `try_get`/`try_get_mut` calls that are the scrutinee of an element
    /// view `match` (§13): their `Some` binding names the element in place
    /// instead of a copy of it.
    pub element_views: BTreeMap<(usize, usize), Pass>,
    /// Lent closures (§4.2), by the name of their body's function
    /// ([`crate::ir::nested_name`]): the names each borrows `in` from its
    /// enclosing scope for the call it is lent to.
    pub lent_closures: BTreeMap<String, BTreeSet<String>>,
    pub string_capacity: usize,
    pub calls: BTreeMap<(usize, usize), BTreeMap<String, Type>>,
    /// Source trait/model elaboration metadata (empty without traits).
    pub elaboration: Elaboration,
    /// Names of the generated transcendental prelude functions this module
    /// uses (`vesper.float.ieee754/1`); a user definition of the same name
    /// is not one.
    pub float_prelude: BTreeSet<String>,
    /// Names of the string prelude functions this module received
    /// (`String::push_char`, `Str::next_char`, ...; §13 strings).
    pub string_prelude: BTreeSet<String>,
    /// Whether the module received the atomic prelude (`Atomic[T]`,
    /// `MemoryOrder`; [`crate::atomics`]).
    pub atomic_prelude: bool,
    /// Whether the module received the scoped-worker prelude
    /// (`parallel_for`; [`crate::atomics`]).
    pub workers_prelude: bool,
    /// Second-class trait objects: `dyn` parameters, vtable stubs and the
    /// vtables the program needs (empty without `dyn`; §7.1).
    pub dyn_objects: crate::dyn_trait::DynObjects,
    /// Memoized ADT expansions of [`World::mode`]. A clone starts empty and
    /// it never makes two worlds unequal.
    pub mode_cache: world_mode::ModeCache,
}
impl World {
    pub fn collect(module: &Module) -> (Self, Vec<Diagnostic>) {
        let mut w = Self::default();
        let mut errors = vec![];
        let mut builtins = crate::parser::parse(&format!("copy enum Option[T] {{ None, Some(T) }} copy enum Result[T, E] {{ Ok(T), Err(E) }} copy enum RuntimeError {{ DivisionByZero, IndexOutOfBounds }} {} effect Fail[E] {{ fn raise(sink error: E) -> Never; }}", crate::numeric::OVERFLOW_DECLARATION)).module;
        if crate::regions::needs_prelude(module) {
            builtins.items.extend(crate::regions::prelude());
        }
        if crate::fip::needs_prelude(module) {
            builtins.items.extend(crate::fip::prelude());
        }
        let builtin_count = builtins.items.len();
        // Built-in secret operations used by the module (§23.2).
        let secret_prelude = crate::capability_effects::secret_prelude(module);
        // Byte-order intrinsics used by the module (`vesper.arith.checked/1`).
        let width_prelude = crate::integer_widths::prelude(
            module,
            crate::integer_widths::module_end(&secret_prelude),
        );
        // Transcendental functions used by the module (`vesper.float.ieee754/1`).
        let float_prelude = crate::float_math::prelude(
            module,
            crate::integer_widths::module_end(&secret_prelude)
                .max(crate::integer_widths::module_end(&width_prelude)),
        );
        // UTF-8 string operations used by the module (§13 strings).
        let string_prelude = crate::strings::prelude(
            module,
            crate::integer_widths::module_end(&secret_prelude)
                .max(crate::integer_widths::module_end(&width_prelude))
                .max(crate::integer_widths::module_end(&float_prelude)),
        );
        // Atomic cells and memory orders used by the module (`atomics`).
        let atomic_prelude = crate::atomics::prelude(
            module,
            crate::integer_widths::module_end(&secret_prelude)
                .max(crate::integer_widths::module_end(&width_prelude))
                .max(crate::integer_widths::module_end(&float_prelude))
                .max(crate::integer_widths::module_end(&string_prelude)),
        );
        w.atomic_prelude = atomic_prelude
            .items
            .iter()
            .any(|i| matches!(i, Item::Adt(a) if a.name == crate::atomics::ATOMIC_TYPE));
        w.workers_prelude = atomic_prelude
            .items
            .iter()
            .any(|i| matches!(i, Item::Function(f) if f.name == crate::atomics::PARALLEL_FOR));
        w.string_prelude = crate::strings::prelude_function_names(&string_prelude);
        w.float_prelude = float_prelude
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        for (index, item) in builtins
            .items
            .iter()
            .chain(module.items.iter())
            .chain(secret_prelude.items.iter())
            .chain(width_prelude.items.iter())
            .chain(float_prelude.items.iter())
            .chain(string_prelude.items.iter())
            .chain(atomic_prelude.items.iter())
            .enumerate()
        {
            match item {
                Item::Adt(a) => {
                    if matches!(a.name.as_str(), "Unit" | "Never" | "String" | "Str") {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("'{}' is a reserved builtin type", a.name),
                            a.span.clone(),
                        ));
                    }
                    if w.adts.insert(a.name.clone(), a.clone()).is_some() {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("duplicate type '{}'", a.name),
                            a.span.clone(),
                        ));
                    }
                }
                Item::Effect(e) => {
                    if index >= builtin_count
                        && e.name == crate::regions::REGION_EFFECT
                        && builtin_count > 0
                        && crate::regions::needs_prelude(module)
                    {
                        errors.push(
                            Diagnostic::new(
                                "E0797",
                                "'Region' is the built-in Strata region effect",
                                e.span.clone(),
                            )
                            .with_help("open a lexical region with `region r { ... }`"),
                        );
                        continue;
                    }
                    if crate::capability_effects::is_capability_effect(&e.name) {
                        errors.push(crate::capability_effects::reserved_declaration(
                            &e.name,
                            e.span.clone(),
                        ));
                        continue;
                    }
                    if w.effects.insert(e.name.clone(), e.clone()).is_some() {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("duplicate effect '{}'", e.name),
                            e.span.clone(),
                        ));
                    }
                }
                Item::Function(f) => {
                    if crate::numeric::wrapping_intrinsic(&f.name).is_some()
                        || ((f.name == crate::integer_widths::MUL_WIDE
                            || crate::integer_widths::bit_builtin(&f.name).is_some()
                            || crate::integer_widths::checked_builtin(&f.name).is_some())
                            && (builtin_count..builtin_count + module.items.len()).contains(&index))
                    {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("'{}' is a reserved arithmetic intrinsic", f.name),
                            f.span.clone(),
                        ));
                    }
                    if w.functions.insert(f.name.clone(), (**f).clone()).is_some() {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("duplicate function '{}'", f.name),
                            f.span.clone(),
                        ));
                    }
                }
                // Elaborated by `pact_frontend` into generated functions.
                Item::Trait(_) | Item::Conformance(_) => {}
                Item::Impl {
                    ty,
                    generics,
                    methods,
                } => {
                    for method in methods {
                        let mut f = method.clone();
                        f.name = format!("{}::{}", ty.name(), f.name);
                        f.generics.splice(0..0, generics.clone());
                        for p in &mut f.params {
                            if p.name == "self" && p.ty == Type::Infer {
                                p.ty = ty.clone();
                            }
                        }
                        if w.functions.insert(f.name.clone(), f.clone()).is_some() {
                            errors.push(Diagnostic::new(
                                "E0100",
                                format!("duplicate method '{}'", f.name),
                                f.span.clone(),
                            ));
                        }
                    }
                }
            }
        }
        if crate::regions::active(&w) {
            // Region labels in signatures name the function's own capabilities.
            for f in w.functions.values_mut() {
                let labels = f
                    .effects
                    .instances
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.name == crate::regions::REGION_EFFECT)
                    .filter_map(|(i, e)| Some((e.label.clone()?, function_capability(&f.name, i))))
                    .collect();
                for p in &mut f.params {
                    p.ty = crate::regions::normalize(&p.ty, &labels);
                }
                f.result = crate::regions::normalize(&f.result, &labels);
            }
        }
        (w, errors)
    }
    pub fn constructor(&self, name: &str) -> Option<(&Adt, &Variant)> {
        // The checker has no namespace resolution for multi-component paths.
        if name.matches("::").count() > 1 {
            return None;
        }
        let short = name.rsplit("::").next().unwrap_or(name);
        self.adts.values().find_map(|a| {
            a.variants
                .iter()
                .find(|v| {
                    v.name == short
                        && (!name.contains("::") || name.starts_with(&format!("{}::", a.name)))
                })
                .map(|v| (a, v))
        })
    }
    /// The mode of `ty` in this world, with `bounds` giving the modes of
    /// type parameters. ADT expansions are memoized per world
    /// ([`world_mode`]); the answer is the original algorithm's
    /// ([`world_mode::reference`]).
    pub fn mode(&self, ty: &Type, bounds: &BTreeMap<String, Mode>) -> Mode {
        world_mode::mode(self, ty, bounds)
    }
    /// The original, unmemoized `mode` (for differential tests and benches).
    #[doc(hidden)]
    pub fn mode_reference(&self, ty: &Type, bounds: &BTreeMap<String, Mode>) -> Mode {
        world_mode::reference(self, ty, bounds)
    }
    /// Forgets the memoized modes. Call it after changing `adts` of a world
    /// that has answered a `mode` query.
    pub fn clear_mode_cache(&self) {
        self.mode_cache.clear();
    }
    /// How many ADT expansions the mode memo holds.
    #[doc(hidden)]
    pub fn mode_cache_len(&self) -> usize {
        world_mode::cached(self)
    }
    pub fn field(&self, ty: &Type, path: &[String]) -> Option<Type> {
        let mut ty = ty.clone();
        for name in path {
            ty = match &ty {
                // An interval loan `s[a..b]` of a sequence or slice place.
                Type::Named(n, args)
                    if crate::seq::is_segment(name)
                        && (n == crate::seq::CHUNK_SEQ_TYPE || n == crate::seq::SLICE_TYPE) =>
                {
                    crate::seq::slice_of(args.first()?.clone())
                }
                // A view loan `s[a..b]` or `s.as_str()` of a string place.
                Type::Named(n, args)
                    if crate::seq::is_segment(name)
                        && args.is_empty()
                        && (n == crate::strings::STRING_TYPE || n == crate::strings::STR_TYPE) =>
                {
                    Type::named(crate::strings::STR_TYPE)
                }
                Type::Named(n, args) if n == crate::tiled::TILED_SEQ_TYPE => {
                    let Type::Named(elem_name, elem_args) = args.first()? else {
                        return None;
                    };
                    let a = self.adts.get(elem_name)?;
                    if a.is_enum || a.tile_width.is_none() {
                        return None;
                    }
                    let f = a.variants[0].fields.iter().find(|f| &f.name == name)?;
                    substitute(
                        &f.ty,
                        &a.generics
                            .iter()
                            .zip(elem_args)
                            .map(|(p, t)| (p.name.clone(), t.clone()))
                            .collect(),
                    )
                }
                Type::Named(n, args) => {
                    let a = self.adts.get(n)?;
                    if a.is_enum {
                        return None;
                    }
                    let f = a.variants[0].fields.iter().find(|f| &f.name == name)?;
                    substitute(
                        &f.ty,
                        &a.generics
                            .iter()
                            .zip(args)
                            .map(|(p, t)| (p.name.clone(), t.clone()))
                            .collect(),
                    )
                }
                Type::Tuple(ts) => ts.get(name.parse::<usize>().ok()?)?.clone(),
                _ => return None,
            };
        }
        Some(ty)
    }

    /// Forks this checked [`World`], registers or replaces `func` in the clone,
    /// and runs the exact function-checking pass for `func` alone (body, modes,
    /// linear obligations, effects, and place capabilities) without re-checking
    /// unrelated sibling items in the module.
    ///
    /// Designed for `< 1 ms` in-memory negative tests (`@test(expect_error = ...)`)
    /// and speculative mutant filtering.
    pub fn check_isolated_item(&self, func: &Function) -> Vec<Diagnostic> {
        let mut world = self.clone();
        let mut func_norm = func.clone();
        if crate::regions::active(&world) {
            let labels = func_norm
                .effects
                .instances
                .iter()
                .enumerate()
                .filter(|(_, e)| e.name == crate::regions::REGION_EFFECT)
                .filter_map(|(i, e)| {
                    Some((e.label.clone()?, function_capability(&func_norm.name, i)))
                })
                .collect();
            for p in &mut func_norm.params {
                p.ty = crate::regions::normalize(&p.ty, &labels);
            }
            func_norm.result = crate::regions::normalize(&func_norm.result, &labels);
        }
        world
            .functions
            .insert(func_norm.name.clone(), func_norm.clone());
        world.function_results.remove(&func_norm.name);

        let mut diagnostics;
        let mut option_tries;
        let mut integer_nots: BTreeSet<(usize, usize)>;
        let mut types;
        let mut for_plans;
        let mut iteration = 0usize;
        loop {
            iteration += 1;
            let mut c = Checker {
                world: &world,
                diagnostics: vec![],
                types: BTreeMap::new(),
                calls: BTreeMap::new(),
                moves: BTreeMap::new(),
                results: world.function_results.clone(),
                handler_effects: world.handler_effects.clone(),
                observations: vec![],
                state_guards: vec![],
                partial_ops: BTreeSet::new(),
                overflow_ops: BTreeSet::new(),
                element_views: BTreeMap::new(),
                element_view: None,
                dead: 0,
                string_capacity: 0,
                static_text: false,
                bounds: BTreeMap::new(),
                generics: vec![],
                allowed: Effects::default(),
                caps: vec![],
                cap_aliases: BTreeMap::new(),
                loans: vec![],
                match_borrows: vec![],
                reserved_loans: BTreeSet::new(),
                lent_closures: BTreeSet::new(),
                lent_captures: vec![],
                lent_bodies: BTreeMap::new(),
                handler_frames: vec![],
                slice_args: BTreeMap::new(),
                effect_inference: vec![],
                traits: None,
                current: String::new(),
                resolved: BTreeMap::new(),
                control: vec![],
                shadowed: vec![],
                literals: Default::default(),
                option_tries: BTreeSet::new(),
                for_sequences: BTreeMap::new(),
                loop_borrows: vec![],
                view_sources: vec![],
                result_ranges: Default::default(),
                total_operands: 0,
                integer_nots: BTreeSet::new(),
            };
            c.function_with_literal_typing(&func_norm);
            let Checker {
                diagnostics: errors,
                results,
                handler_effects,
                option_tries: tries,
                types: checked_types,
                for_sequences: plans,
                integer_nots: nots,
                ..
            } = c;
            integer_nots = nots;
            option_tries = tries;
            types = checked_types;
            for_plans = plans;
            let changed =
                world.function_results != results || world.handler_effects != handler_effects;
            world.function_results = results;
            world.handler_effects = handler_effects;
            diagnostics = errors;
            if !changed {
                break;
            }
            if iteration > world.handler_effects.len() * 2 + 4 {
                diagnostics.push(Diagnostic::new(
                    "E0408",
                    "capability and continuation effect inference did not converge; add explicit type/effect annotations",
                    func_norm.span.clone(),
                ));
                break;
            }
        }

        if !for_plans.is_empty() {
            // Check the index loop a `for` over a sequence means instead.
            let after = func_norm
                .span
                .end
                .max(world.types.keys().map(|k| k.1).max().unwrap_or(0))
                + 1;
            let single = Module {
                items: vec![Item::Function(Box::new(func_norm))],
                ..Module::default()
            };
            let desugared = for_sequences::desugar(&single, &for_plans, after);
            if let Some(Item::Function(desugared_fn)) = desugared.items.into_iter().next() {
                return self.check_isolated_item(&desugared_fn);
            }
            return diagnostics;
        }
        if !option_tries.is_empty() {
            let after = func_norm
                .span
                .end
                .max(world.types.keys().map(|k| k.1).max().unwrap_or(0))
                + 1;
            let single = Module {
                items: vec![Item::Function(Box::new(func_norm.clone()))],
                ..Module::default()
            };
            let desugared = try_option::desugar(&single, &option_tries, after);
            if let Some(Item::Function(desugared_fn)) = desugared.items.into_iter().next() {
                return self.check_isolated_item(&desugared_fn);
            }
        }
        if !integer_nots.is_empty() {
            // Check the complements `!` on integers means instead.
            let single = Module {
                items: vec![Item::Function(Box::new(func_norm.clone()))],
                ..Module::default()
            };
            let rewritten = integer_widths_rules::complement_nots(&single, &integer_nots);
            if let Some(Item::Function(rewritten_fn)) = rewritten.items.into_iter().next() {
                return self.check_isolated_item(&rewritten_fn);
            }
        }
        let after = func_norm
            .span
            .end
            .max(types.keys().map(|k| k.1).max().unwrap_or(0))
            + 1;
        if let Some(rewritten) =
            crate::seq::element_paths::desugar_function(&func_norm, &world, &types, after)
        {
            return self.check_isolated_item(&rewritten);
        }

        let mut seen = BTreeSet::new();
        diagnostics
            .retain(|d| seen.insert((d.code.clone(), d.span.start, d.span.end, d.message.clone())));
        diagnostics
    }

    /// Convenience wrapper around [`Self::check_isolated_item`] returning `Ok(())`
    /// when `func` produces zero diagnostics, or `Err(diagnostics)` otherwise.
    pub fn check_isolated_module_fn(&self, func: &Function) -> Result<(), Vec<Diagnostic>> {
        let diagnostics = self.check_isolated_item(func);
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(diagnostics)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Place {
    pub root: String,
    pub path: Vec<String>,
}
impl Place {
    pub fn from_expr(e: &Expr) -> Option<Self> {
        match &e.kind {
            ExprKind::Name(n) => Some(Self {
                root: n.clone(),
                path: vec![],
            }),
            ExprKind::Field(e, f) => {
                let mut p = Self::from_expr(e)?;
                p.path.push(f.clone());
                Some(p)
            }
            _ => None,
        }
    }
    /// The place of element `index` of the sequence place `base` (§13): a
    /// path segment `[n]` for an integer literal, `[$ixK_m]` for an index
    /// the element-path rewrite bound and checked distinct
    /// ([`crate::seq::element_paths`]), and `[_]` for any other index.
    pub fn element(base: &Expr, index: &Expr) -> Option<Self> {
        let mut p = Self::from_expr(base)?;
        p.path.push(element_segment(index));
        Some(p)
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        self.root == other.root
            && self
                .path
                .iter()
                .zip(&other.path)
                .all(|(a, b)| a == b || !disjoint_segments(a, b))
    }
    pub fn display(&self) -> String {
        let mut out = self.root.clone();
        for segment in &self.path {
            if !is_element_segment(segment) {
                out.push('.');
            }
            out.push_str(segment);
        }
        out
    }
}
/// The path segment of an element index (see [`Place::element`]).
fn element_segment(index: &Expr) -> String {
    match &index.kind {
        ExprKind::Literal(Literal::Int(n)) => format!("[{n}]"),
        ExprKind::Name(n) if crate::seq::element_paths::is_distinct_index(n) => format!("[{n}]"),
        _ => "[_]".into(),
    }
}
/// Whether `segment` is an element segment of a place path ([`Place::element`]),
/// as opposed to a field or an interval loan (`crate::seq::interval_segment`).
pub fn is_element_segment(segment: &str) -> bool {
    segment.starts_with('[') && !segment.contains("..")
}
/// Whether two different path segments name disjoint parts: two fields, two
/// interval loans (proved disjoint by `seq_call_slices`), or two elements
/// known to differ (distinct literals, or two indices of one checked
/// group). An unknown index `[_]` may be any element, and an element may be
/// inside any interval.
fn disjoint_segments(a: &str, b: &str) -> bool {
    match (is_element_segment(a), is_element_segment(b)) {
        (false, false) => a.starts_with('[') == b.starts_with('['),
        (true, true) => {
            let inner = |s: &str| s[1..s.len() - 1].to_string();
            let (a, b) = (inner(a), inner(b));
            if a == "_" || b == "_" {
                return false;
            }
            match (a.parse::<i128>(), b.parse::<i128>()) {
                (Ok(x), Ok(y)) => x != y,
                (Err(_), Err(_)) => crate::seq::element_paths::distinct_indices(&a, &b),
                _ => false,
            }
        }
        _ => false,
    }
}
impl Place {
    /// Whether the place goes through a sequence element.
    pub fn through_element(&self) -> bool {
        self.path.iter().any(|s| is_element_segment(s))
    }
}
// Diagnostic-only evidence. This is deliberately excluded from ownership state
// comparisons and all published World/IR schemas. Missing/capped evidence stays
// absent rather than inventing a last use. Each binding retains at most four
// source events and only bounded place paths.
const MAX_MOVE_CAUSES: usize = 4;
#[derive(Debug, Clone)]
struct MoveCause {
    path: Vec<String>,
    span: Span,
}
/// Inclusive bounds of an integer binding, tied to the write identity
/// (`Binding::mutation`) they hold for: any write or invalidation renews that
/// identity and so drops the range, as it drops exact facts.
#[derive(Debug, Clone)]
struct IntRange {
    lo: i128,
    hi: i128,
    at: Arc<()>,
    /// The range of a `for` loop index (#1091), whose arithmetic may use
    /// its bounds to rule out overflow.
    looped: bool,
}
#[derive(Debug, Clone)]
struct Binding {
    ty: Type,
    pass: Pass,
    projections: Vec<Vec<String>>,
    moved: Vec<Vec<String>>,
    move_causes: Vec<MoveCause>,
    span: Span,
    integer: Option<i128>,
    nonzero: bool,
    /// Inclusive bounds from a dominating comparison or a write, valid only
    /// while `mutation` is the identity they were taken under.
    range: Option<IntRange>,
    // Identity changes on actual writes/invalidation, never on branch refinement.
    mutation: Arc<()>,
    /// Bound by `let` without `mut`: assignment and `inout` borrows are
    /// rejected (E0313).
    frozen: bool,
}
impl Binding {
    fn remember_move(&mut self, path: &[String], span: Span) {
        if self.move_causes.len() >= MAX_MOVE_CAUSES
            || path.len() > 64
            || path
                .iter()
                .try_fold(0usize, |n, p| n.checked_add(p.len()))
                .is_none_or(|n| n > 1024)
            || self
                .move_causes
                .iter()
                .any(|c| c.path == path && c.span == span)
        {
            return;
        }
        self.move_causes.push(MoveCause {
            path: path.to_vec(),
            span,
        });
    }
}
#[derive(Debug, Clone)]
struct HandlerFrame {
    cap: usize,
    outer: BTreeMap<String, Span>,
    kept: BTreeMap<String, Span>,
}
#[derive(Debug, Clone, Default)]
struct Env {
    bindings: BTreeMap<String, Binding>,
}
impl Env {
    fn invalidate_borrowed_facts(&mut self) {
        for binding in self.bindings.values_mut().filter(|b| b.pass != Pass::Sink) {
            binding.mutation = Arc::new(());
            binding.integer = None;
            binding.nonzero = false;
        }
    }
}
#[derive(Clone, Copy)]
enum Usage {
    Own,
    Borrow(Pass),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub world: World,
    pub diagnostics: Vec<Diagnostic>,
    pub expression_types: TypeMap,
}
/// Checks a module. Source `trait`/`impl ... for`/`model` declarations,
/// projections and `use model` scopes are elaborated by
/// [`crate::pact_frontend`] into ordinary core functions and calls first;
/// the returned World then contains only the existing core schema.
pub fn check(module: &Module) -> Checked {
    // Untyped value enum members are literals at every use (§2.2).
    if let Some((module, diagnostics)) = crate::arrays::untyped_members(module) {
        let mut checked = check(&module);
        checked.diagnostics.extend(diagnostics);
        return checked;
    }
    // Source tensor types (§28.2): a module that uses them is checked and
    // elaborated by `tensor_types`, which runs the core checker itself.
    if let Some(checked) = crate::tensor_types::check(module) {
        return checked;
    }
    let beacon = crate::trace_effects::with_prelude(module);
    let mut checked = crate::pact_frontend::check_module(beacon.module());
    beacon.check(&mut checked);
    // `use` imports resolve only through the package loader (lane F0).
    for u in &module.uses {
        checked.diagnostics.push(
            Diagnostic::new(
                crate::packages::USE_OUTSIDE_PACKAGE,
                "`use` imports need a package; check the package directory instead of one file",
                u.span.clone(),
            )
            .with_help("a package is a directory with a Loom.toml and src/**/*.ves"),
        );
    }
    checked
}

/// Whether `t` is a builtin scalar (an integer, float, `bool` or `Unit`),
/// which every backend passes as one word by value. Two-phase argument
/// loans (§4.3) let such a value be read from a reserved `inout` place.
pub(crate) fn is_scalar_type(t: &Type) -> bool {
    matches!(crate::numeric::default_type(t), Type::Named(n, a) if a.is_empty() && crate::numeric::is_scalar_name(&n))
}

/// Static resolution of trait method calls during the frontend's first
/// checking pass. Implemented by `pact_frontend`.
pub(crate) trait TraitCalls {
    /// `None` when `call` is not a trait call; otherwise the generated
    /// function the coherent Pact session selected, or a diagnostic.
    fn resolve(&self, call: &TraitCall<'_>) -> Option<Result<String, Diagnostic>>;
}
/// A receiver (`x.m(...)`) or path (`Trait::m(...)`, `Model::m(...)`) call.
pub(crate) struct TraitCall<'a> {
    /// Method name of a receiver call, or the full path of a path call.
    pub callee: &'a str,
    pub path: bool,
    /// Type of the receiver or first argument (`Infer` when unknown).
    pub receiver: Type,
    /// Generic parameters in scope at the call.
    pub generics: &'a [Generic],
    /// Name of the function containing the call.
    pub function: &'a str,
    pub span: Span,
}

/// The core checker. With `traits`, unresolved trait calls are resolved
/// through it and recorded by call span; the Place-IR validation is then
/// skipped because the module still contains surface trait calls.
pub(crate) fn check_core(
    module: &Module,
    traits: Option<&dyn TraitCalls>,
) -> (Checked, BTreeMap<(usize, usize), String>) {
    // Module constants (§2.2): every use is a call of the constant.
    let with_consts = crate::arrays::lower_consts(module);
    let module = with_consts.as_ref().unwrap_or(module);
    let (mut world, collection_errors) = World::collect(module);
    let mut resolved;
    let mut diagnostics = vec![];
    let mut iteration = 0;
    let mut option_tries: BTreeSet<(usize, usize)>;
    let mut integer_nots: BTreeSet<(usize, usize)>;
    let mut for_plans: BTreeMap<(usize, usize), for_sequences::Plan>;
    loop {
        iteration += 1;
        let mut c = Checker {
            world: &world,
            diagnostics: collection_errors.clone(),
            types: BTreeMap::new(),
            calls: BTreeMap::new(),
            moves: BTreeMap::new(),
            results: BTreeMap::new(),
            handler_effects: BTreeMap::new(),
            observations: vec![],
            state_guards: vec![],
            partial_ops: BTreeSet::new(),
            overflow_ops: BTreeSet::new(),
            element_views: BTreeMap::new(),
            element_view: None,
            dead: 0,
            string_capacity: 0,
            static_text: false,
            bounds: BTreeMap::new(),
            generics: vec![],
            allowed: Effects::default(),
            caps: vec![],
            cap_aliases: BTreeMap::new(),
            loans: vec![],
            match_borrows: vec![],
            reserved_loans: BTreeSet::new(),
            lent_closures: BTreeSet::new(),
            lent_captures: vec![],
            lent_bodies: BTreeMap::new(),
            handler_frames: vec![],
            slice_args: BTreeMap::new(),
            effect_inference: vec![],
            traits,
            current: String::new(),
            resolved: BTreeMap::new(),
            control: vec![],
            shadowed: vec![],
            literals: Default::default(),
            option_tries: BTreeSet::new(),
            for_sequences: BTreeMap::new(),
            loop_borrows: vec![],
            view_sources: vec![],
            result_ranges: Default::default(),
            total_operands: 0,
            integer_nots: BTreeSet::new(),
        };
        c.declarations();
        for f in world.functions.values() {
            c.function_with_literal_typing(f);
        }
        let Checker {
            diagnostics: errors,
            types,
            calls,
            moves,
            results,
            handler_effects,
            partial_ops,
            overflow_ops,
            element_views,
            lent_bodies,
            string_capacity,
            resolved: trait_calls,
            option_tries: tries,
            for_sequences: plans,
            integer_nots: nots,
            ..
        } = c;
        integer_nots = nots;
        option_tries = tries;
        for_plans = plans;
        resolved = trait_calls;
        let changed = world.function_results != results || world.handler_effects != handler_effects;
        world.types = types;
        world.calls = calls;
        world.moves = moves;
        world.function_results = results;
        world.handler_effects = handler_effects;
        world.partial_ops = partial_ops;
        world.overflow_ops = overflow_ops;
        world.element_views = element_views;
        world.lent_closures = lent_bodies;
        world.string_capacity = string_capacity;
        diagnostics = errors;
        if !changed {
            break;
        }
        if iteration > world.functions.len() + world.handler_effects.len() * 2 + 4 {
            diagnostics.push(Diagnostic::new("E0408","capability and continuation effect inference did not converge; add explicit type/effect annotations",0..0));
            break;
        }
    }
    if !for_plans.is_empty() {
        // Check the index loops `for` over a sequence means instead
        // (`for_sequences`).
        let after = world
            .functions
            .values()
            .map(|f| f.span.end)
            .chain(world.types.keys().map(|k| k.1))
            .max()
            .unwrap_or(0)
            + 1;
        return check_core(&for_sequences::desugar(module, &for_plans, after), traits);
    }
    if !option_tries.is_empty() {
        // Check the early exits `?` on `Option` means instead (`try_option`).
        let after = world
            .functions
            .values()
            .map(|f| f.span.end)
            .chain(world.types.keys().map(|k| k.1))
            .max()
            .unwrap_or(0)
            + 1;
        return check_core(&try_option::desugar(module, &option_tries, after), traits);
    }
    if !integer_nots.is_empty() {
        // Check the complements `!` on integers means instead.
        return check_core(
            &integer_widths_rules::complement_nots(module, &integer_nots),
            traits,
        );
    }
    // Field paths through sequence elements and calls that borrow two
    // elements of one sequence mean element views (§13): check those
    // instead (`crate::seq::element_paths`).
    let after = world
        .functions
        .values()
        .map(|f| f.span.end)
        .chain(world.types.keys().map(|k| k.1))
        .max()
        .unwrap_or(0)
        + 1;
    if let Some(rewritten) = crate::seq::element_paths::desugar(module, &world, &world.types, after)
    {
        return check_core(&rewritten, traits);
    }
    if diagnostics.is_empty() && traits.is_none() {
        diagnostics.extend(crate::capability_effects::check_world(&world));
        diagnostics.extend(crate::vector::check_world(&world));
        diagnostics.extend(crate::tiled::check_world_adts(&world));
    }
    if traits.is_none() {
        // Slice placement is independent of every other judgment.
        diagnostics.extend(crate::seq::check_world(&world));
        diagnostics.extend(crate::strings::check_world(&world));
        diagnostics.extend(crate::atomics::check_world(&world));
    }
    if diagnostics.is_empty() && traits.is_none() {
        diagnostics.extend(crate::regions::check_world(&world));
        diagnostics.extend(crate::fip::check_world(&world));
    }
    if traits.is_none() {
        diagnostics.extend(crate::arrays::check_world(&world));
        // Array writes and `len` in the forms Place IR carries (§5).
        crate::arrays::lower_world(&mut world);
    }
    if diagnostics.is_empty() && traits.is_none() {
        let program = crate::ir::lower_world(&world);
        diagnostics.extend(crate::ir_check::validate(&world, &program));
        diagnostics.extend(crate::atomics::check_program(&world, &program));
    }
    let mut seen = BTreeSet::new();
    diagnostics
        .retain(|d| seen.insert((d.code.clone(), d.span.start, d.span.end, d.message.clone())));
    (
        Checked {
            expression_types: world.types.clone(),
            world,
            diagnostics,
        },
        resolved,
    )
}
#[derive(Clone)]
struct Checker<'a> {
    world: &'a World,
    diagnostics: Vec<Diagnostic>,
    types: TypeMap,
    calls: BTreeMap<(usize, usize), BTreeMap<String, Type>>,
    moves: BTreeMap<(usize, usize), Vec<Place>>,
    results: BTreeMap<String, Type>,
    handler_effects: BTreeMap<(usize, usize), Effects>,
    observations: Vec<(usize, Effects)>,
    state_guards: Vec<(usize, usize, Type, Span)>,
    partial_ops: BTreeSet<(usize, usize)>,
    overflow_ops: BTreeSet<(usize, usize)>,
    element_views: BTreeMap<(usize, usize), Pass>,
    /// Set while the scrutinee of an element view `match` is checked: how
    /// the view borrows the element (§13).
    element_view: Option<Pass>,
    dead: usize,
    string_capacity: usize,
    /// Checking the body of a `-> Str` function: string literals the
    /// result expects are static `Str` text.
    static_text: bool,
    bounds: BTreeMap<String, Mode>,
    generics: Vec<Generic>,
    allowed: Effects,
    caps: Vec<Effect>,
    cap_aliases: BTreeMap<String, String>,
    loans: Vec<(Place, Pass, Span)>,
    /// Places that an enclosing borrowed `match` lends to its arms' `in`
    /// bindings (§4.3), with the scrutinee's span: inside the arms they may
    /// be read but not written, lent `inout` or moved.
    match_borrows: Vec<(Place, Span, Pass)>,
    /// Origin spans of the `inout` argument loans in `loans` (receivers and
    /// plain arguments): two-phase borrows (§4.3) reserve them while the
    /// call's other arguments are evaluated, so those arguments may read the
    /// loaned place.
    reserved_loans: BTreeSet<(usize, usize)>,
    /// Spans of closure literals lent to a function parameter (§4.2, lent
    /// closures): written directly as the argument for an `in` parameter
    /// whose function type is not `copy`. Such a closure borrows the names
    /// it uses `in` for the call instead of capturing them.
    lent_closures: BTreeSet<(usize, usize)>,
    /// The places the lent closure just checked borrows, with its span: the
    /// call adds them to its argument loans.
    lent_captures: Vec<(Place, Span)>,
    /// What becomes [`World::lent_closures`].
    lent_bodies: BTreeMap<String, BTreeSet<String>>,
    /// Lexical handlers being checked: the capability index each installs,
    /// the declaration span of every binding declared outside it, and the
    /// outer linear bindings that were live at an abortable checkpoint this
    /// handler catches.
    handler_frames: Vec<HandlerFrame>,
    /// Spans of call arguments that may be slice views (`s[a..b]`).
    slice_args: BTreeMap<(usize, usize), Pass>,
    effect_inference: Vec<(bool, usize, Effects, String)>,
    traits: Option<&'a dyn TraitCalls>,
    current: String,
    resolved: BTreeMap<(usize, usize), String>,
    /// Loop, function and handler boundaries for `break`/`return`.
    control: Vec<loop_rules::ControlFrame>,
    /// Bindings each enclosing block has shadowed, innermost last.
    shadowed: Vec<BTreeMap<String, Binding>>,
    /// Literal-initialized bindings typed from their uses.
    literals: literal_typing::LiteralTyping,
    /// Spans of `?` on `Option` ([`try_option`]), rewritten after checking.
    option_tries: BTreeSet<(usize, usize)>,
    /// `for` loops over arrays and sequences by the span of their iterable
    /// ([`for_sequences`]), rewritten to index loops after checking.
    for_sequences: BTreeMap<(usize, usize), for_sequences::Plan>,
    /// Places an enclosing `for` loop iterates: its body may read them but
    /// not write, lend `inout` or move them ([`for_sequences`]).
    loop_borrows: Vec<(Place, Span)>,
    /// Bound views of immutable owned bindings: the viewed place, the view
    /// binding's name and declaration span. While the view is in scope the
    /// place cannot be moved (`E1531`).
    view_sources: Vec<(Place, String, Span)>,
    /// Result-range summaries of called functions ([`call_facts`]).
    result_ranges: std::cell::RefCell<call_facts::ResultRanges>,
    /// Nonzero while a shift amount or a divisor is checked: its `+`, `-`
    /// and `*` may be proven in range by range facts
    /// ([`crate::integer_widths`] check rules).
    total_operands: usize,
    /// Spans of prefix `!` on an integer operand: the bitwise complement,
    /// rewritten to `~` and checked again.
    integer_nots: BTreeSet<(usize, usize)>,
}
impl Checker<'_> {
    fn error(&mut self, code: &str, msg: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic::new(code, msg, span));
    }
    // Called immediately after the unchanged primary producer. Fixed messages
    // and at most eight labels bound new metadata independently of source names.
    fn related(&mut self, span: Span, message: &'static str) {
        if let Some(d) = self.diagnostics.last_mut()
            && d.related.len() < 8
        {
            d.related.push(DiagnosticLabel {
                span,
                message: message.into(),
            });
        }
    }
    fn help(&mut self, message: &'static str) {
        if let Some(d) = self.diagnostics.last_mut() {
            d.help = Some(message.into());
        }
    }
    fn mode(&self, t: &Type) -> Mode {
        self.world.mode(t, &self.bounds)
    }
    /// Whether `t` is an opaque `.vsys` extern-type handle (§16.3), which
    /// `crate::ffi_lowering` generates as a struct whose one field no source
    /// identifier can spell.
    /// Whether a value of `t` holds an opaque `.vsys` handle anywhere:
    /// directly, or through type arguments, tuple items or ADT fields.
    fn extern_handle(&self, t: &Type) -> bool {
        self.holds_extern_handle(t, &mut BTreeSet::new())
    }
    /// Whether a value of `t` holds an affine `.vsys` handle whose type
    /// declares a drop import (`crate::ffi_lowering::drops`).
    fn droppable_handle(&self, t: &Type) -> bool {
        fn visit(c: &Checker, t: &Type, seen: &mut BTreeSet<String>) -> bool {
            match t {
                Type::Named(n, args) => {
                    if args.iter().any(|a| visit(c, a, seen)) {
                        return true;
                    }
                    if !seen.insert(n.clone()) {
                        return false;
                    }
                    if c.world
                        .functions
                        .contains_key(&format!("{}{n}", crate::ffi_lowering::drops::DROP_PREFIX))
                    {
                        return true;
                    }
                    c.world.adts.get(n).is_some_and(|a| {
                        a.variants
                            .iter()
                            .any(|v| v.fields.iter().any(|f| visit(c, &f.ty, seen)))
                    })
                }
                Type::Tuple(items) => items.iter().any(|i| visit(c, i, seen)),
                _ => false,
            }
        }
        visit(self, t, &mut BTreeSet::new())
    }
    /// [`Self::droppable_handle`] for the parts of a binding of `t` at
    /// `path` that `moved` has not moved out.
    fn droppable_remaining(&self, t: &Type, path: &mut Vec<String>, moved: &[Vec<String>]) -> bool {
        if moved.iter().any(|m| path.starts_with(m)) {
            return false;
        }
        if !moved.iter().any(|m| m.starts_with(path)) {
            return self.droppable_handle(t);
        }
        match t {
            Type::Tuple(items) => items.iter().enumerate().any(|(i, item)| {
                path.push(i.to_string());
                let found = self.droppable_remaining(item, path, moved);
                path.pop();
                found
            }),
            Type::Named(n, args) => match self.world.adts.get(n) {
                Some(a) if !a.is_enum && a.variants.len() == 1 => {
                    let vars: BTreeMap<String, Type> = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    a.variants[0].fields.iter().any(|f| {
                        path.push(f.name.clone());
                        let found =
                            self.droppable_remaining(&substitute(&f.ty, &vars), path, moved);
                        path.pop();
                        found
                    })
                }
                _ => self.droppable_handle(t),
            },
            _ => false,
        }
    }
    fn holds_extern_handle(&self, t: &Type, seen: &mut BTreeSet<String>) -> bool {
        match t {
            Type::Named(n, args) => {
                if args.iter().any(|a| self.holds_extern_handle(a, seen)) {
                    return true;
                }
                if !seen.insert(n.clone()) {
                    return false;
                }
                self.world.adts.get(n).is_some_and(|a| {
                    a.variants.iter().any(|v| {
                        v.fields.iter().any(|f| {
                            f.name == crate::ffi_lowering::HANDLE_FIELD
                                || self.holds_extern_handle(&f.ty, seen)
                        })
                    })
                })
            }
            Type::Tuple(items) => items.iter().any(|i| self.holds_extern_handle(i, seen)),
            _ => false,
        }
    }
    fn declarations(&mut self) {
        for a in self.world.adts.values() {
            self.valid_generics(&a.generics, a.span.clone());
            self.bounds = a
                .generics
                .iter()
                .filter(|g| !g.row)
                .map(|g| (g.name.clone(), g.bound))
                .collect();
            let mut names = BTreeSet::new();
            for v in &a.variants {
                if !names.insert(v.name.clone()) {
                    self.error("E0100", "duplicate variant", a.span.clone());
                }
                let mut fields = BTreeSet::new();
                for f in &v.fields {
                    if !fields.insert(&f.name) {
                        self.error("E0100", "duplicate field", a.span.clone());
                    }
                    self.valid_type(&f.ty, &a.generics, a.span.clone());
                    if let Some(modality) = f.modality
                        && self.mode(&f.ty) > modality
                    {
                        self.error(
                            "E0105",
                            format!(
                                "field '{}' cannot relax {:?} ownership to {:?}",
                                f.name,
                                self.mode(&f.ty),
                                modality
                            ),
                            a.span.clone(),
                        );
                    }
                }
            }
        }
        for effect in self.world.effects.values() {
            self.valid_generics(&effect.generics, effect.span.clone());
            self.bounds = effect
                .generics
                .iter()
                .filter(|g| !g.row)
                .map(|g| (g.name.clone(), g.bound))
                .collect();
            let mut names = BTreeSet::new();
            for op in &effect.ops {
                if !names.insert(&op.name) {
                    self.error("E0100", "duplicate effect operation", op.span.clone());
                }
                for p in &op.params {
                    self.valid_type(&p.ty, &effect.generics, p.span.clone());
                }
                self.valid_type(&op.result, &effect.generics, op.span.clone());
            }
        }
    }
    fn valid_type(&mut self, t: &Type, g: &[Generic], span: Span) {
        if crate::vector::is_vector_spelling(t) {
            if let Some(message) = crate::vector::type_error(t) {
                self.error("E1401", message, span);
            }
            return;
        }
        if crate::regions::active(self.world)
            && let Some(errors) = crate::regions::validate_type(t, span.clone())
        {
            self.diagnostics.extend(errors);
            // The capability argument is checked by `validate_type` itself.
            if let Type::Named(n, args) = t {
                if n == crate::tiled::TILED_SEQ_TYPE {
                    if let Some(elem) = args.first() {
                        self.valid_type(elem, g, span.clone());
                        self.check_tiled_record_type(elem, span);
                    }
                } else {
                    for a in args.iter().skip(1) {
                        self.valid_type(a, g, span.clone());
                    }
                }
            }
            return;
        }
        match t {
            Type::Named(n, args)
                if n == crate::seq::CHUNK_SEQ_TYPE || n == crate::seq::SLICE_TYPE =>
            {
                self.valid_seq_type(n, args, g, span);
            }
            // `dyn Trait[args]`, admitted by the frontend only as
            // `Box[dyn Trait[args]]` (§7.3); the trait is resolved there.
            Type::Named(n, args) if n == crate::ast::DYN_TYPE => {
                if let Some(Type::Named(_, targs)) = args.first() {
                    for a in targs {
                        self.valid_type(a, g, span.clone());
                    }
                }
            }
            Type::Named(n, args) if n == crate::tiled::TILED_SEQ_TYPE => {
                if !matches!(args.len(), 1 | 2) {
                    self.error(
                        "E0101",
                        format!("{n} expects 1 or 2 type arguments, found {}", args.len()),
                        span.clone(),
                    );
                }
                if args.len() == 2 {
                    self.error(
                        "E0797",
                        format!("{n}[T, r] requires a region capability in scope"),
                        span.clone(),
                    );
                }
                if let Some(elem) = args.first() {
                    self.valid_type(elem, g, span.clone());
                    self.check_tiled_record_type(elem, span);
                }
            }
            Type::Named(n, args) => {
                let expected = self
                    .world
                    .adts
                    .get(n)
                    .map(|a| a.generics.len())
                    .or_else(|| {
                        if crate::numeric::is_scalar_name(n)
                            || n == "String"
                            || n == crate::strings::STR_TYPE
                            || g.iter().any(|p| &p.name == n && !p.row)
                        {
                            Some(0)
                        } else {
                            None
                        }
                    });
                if let Some(arity) = expected {
                    if args.len() != arity {
                        self.error(
                            "E0101",
                            format!("{n} expects {arity} type arguments, found {}", args.len()),
                            span.clone(),
                        );
                    }
                } else if !crate::numeric::is_scalar_name(n)
                    && n != "String"
                    && n != crate::strings::STR_TYPE
                    && !g.iter().any(|p| &p.name == n)
                {
                    self.error("E0101", format!("unknown type '{n}'"), span.clone());
                }
                if let Some(decl) = self.world.adts.get(n) {
                    let vars = decl
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    self.generic_bounds(&decl.generics, &vars, span.clone());
                }
                for a in args {
                    self.valid_type(a, g, span.clone());
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    self.valid_type(t, g, span.clone());
                }
            }
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                for (_, t) in params {
                    self.valid_type(t, g, span.clone());
                }
                self.valid_type(result, g, span.clone());
                self.valid_effects(effects, g, span);
            }
            Type::Row(effects) => self.valid_effects(effects, g, span),
            _ => {}
        }
    }
    fn valid_generics(&mut self, gs: &[Generic], span: Span) {
        let mut seen = BTreeSet::new();
        for g in gs {
            if !seen.insert(&g.name) {
                self.error("E0100", "duplicate generic parameter", span.clone());
            }
        }
    }
    fn valid_effects(&mut self, effects: &Effects, gs: &[Generic], span: Span) {
        if let Some(row) = &effects.tail
            && !gs.iter().any(|g| g.row && &g.name == row)
        {
            self.error("E0406", format!("unknown effect row '{row}'"), span.clone());
        }
        let mut labels = BTreeSet::new();
        for e in &effects.instances {
            if let Some(label) = &e.label
                && !labels.insert(label)
            {
                self.error("E0400", "duplicate lexical capability label", span.clone());
            }
            if crate::capability_effects::is_capability_effect(&e.name) {
                self.diagnostics
                    .extend(crate::capability_effects::row_entry(e, span.clone()));
                continue;
            }
            if let Some(decl) = self.world.effects.get(&e.name) {
                if e.args.len() != decl.generics.len() {
                    self.error(
                        "E0400",
                        format!(
                            "effect '{}' expects {} type arguments",
                            e.name,
                            decl.generics.len()
                        ),
                        span.clone(),
                    );
                }
                let vars = decl
                    .generics
                    .iter()
                    .zip(&e.args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                self.generic_bounds(&decl.generics, &vars, span.clone());
                for t in &e.args {
                    self.valid_type(t, gs, span.clone());
                }
            } else {
                self.error(
                    "E0400",
                    format!("unknown effect '{}'", e.name),
                    span.clone(),
                );
            }
        }
    }
    fn function_body(&mut self, f: &Function) {
        self.current = f.name.clone();
        self.bounds = f
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| (g.name.clone(), g.bound))
            .collect();
        self.generics = f.generics.clone();
        self.valid_generics(&f.generics, f.span.clone());
        self.allowed = f.effects.clone();
        self.caps = f
            .effects
            .instances
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut e = e.clone();
                e.label = Some(function_capability(&f.name, i));
                e
            })
            .collect();
        self.cap_aliases = f
            .effects
            .instances
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                e.label
                    .clone()
                    .map(|label| (label, function_capability(&f.name, i)))
            })
            .collect();
        self.loans.clear();
        self.reserved_loans.clear();
        self.valid_type(&f.result, &f.generics, f.span.clone());
        let mut env = Env::default();
        for p in &f.params {
            if p.ty == Type::Infer {
                self.error(
                    "E0102",
                    "function parameters require a type",
                    p.span.clone(),
                );
            }
            self.valid_type(&p.ty, &f.generics, p.span.clone());
            for path in &p.projections {
                if self.world.field(&p.ty, path).is_none() {
                    self.error(
                        "E0300",
                        format!("invalid projection '{}.{}'", p.name, path.join(".")),
                        p.span.clone(),
                    );
                }
            }
            for (i, a) in p.projections.iter().enumerate() {
                for b in &p.projections[i + 1..] {
                    if a.starts_with(b) || b.starts_with(a) {
                        self.error("E0301", "overlapping parameter projections", p.span.clone());
                    }
                }
            }
            if env
                .bindings
                .insert(
                    p.name.clone(),
                    Binding {
                        ty: bind_signature_capabilities(&p.ty, f),
                        pass: p.pass,
                        projections: p.projections.clone(),
                        moved: vec![],
                        move_causes: vec![],
                        span: p.span.clone(),
                        mutation: Arc::new(()),
                        frozen: false,
                        integer: None,
                        nonzero: false,
                        range: None,
                    },
                )
                .is_some()
            {
                self.error("E0100", "duplicate parameter", p.span.clone());
            }
        }
        self.valid_effects(&f.effects, &f.generics, f.span.clone());
        if let Some(body) = &f.body {
            let outer = self.enter_function(&f.result);
            let static_text = std::mem::replace(
                &mut self.static_text,
                crate::strings::is_str_type(&f.result),
            );
            let actual = self.expr_expected(body, &mut env, Usage::Own, &f.result);
            self.static_text = static_text;
            let actual = self.leave_function(outer, actual, body.span.clone());
            self.expect_type(&f.result, &actual, body.span.clone());
            self.results
                .insert(f.name.clone(), refine_annotation(&f.result, &actual));
            self.exhaust(
                &env,
                env.bindings.keys().cloned().collect(),
                body.span.clone(),
            );
        } else {
            self.error(
                "E0103",
                "a function outside an effect declaration requires a body",
                f.span.clone(),
            );
        }
    }
    fn join_types(&mut self, a: &Type, b: &Type, span: Span) -> Type {
        join_types(a, b, &span, &mut self.diagnostics)
    }
    fn expect_type(&mut self, expected: &Type, actual: &Type, span: Span) {
        if !compatible(expected, actual) {
            if let Some(d) = crate::regions::mismatch(expected, actual, span.clone()) {
                self.diagnostics.push(d);
                return;
            }
            self.error(
                "E0200",
                format!("expected {expected}, found {actual}"),
                span,
            );
        }
    }
    fn exhaust(&mut self, env: &Env, names: Vec<String>, span: Span) {
        for n in names {
            if let Some(b) = env.bindings.get(&n)
                && b.pass == Pass::Sink
                && self.mode(&b.ty) == Mode::Linear
                && !b.moved.iter().any(Vec::is_empty)
            {
                self.error("E0302",format!("linear value '{n}' must be consumed exactly once; destructure it or pass it to a sink parameter"),span.clone());
                self.related(b.span.clone(), "linear binding introduced here");
                self.help("Every owned linear binding must be consumed exactly once on every returning path.");
            }
        }
    }
    /// The type at `path` inside a value of type `ty`: fields, and the
    /// elements of sequences (`[..]` segments, [`Place::element`]).
    fn place_type(&self, ty: &Type, path: &[String]) -> Option<Type> {
        let mut ty = ty.clone();
        let mut fields: Vec<String> = vec![];
        for segment in path {
            if is_element_segment(segment) {
                if !fields.is_empty() {
                    ty = self.world.field(&ty, &fields)?;
                    fields.clear();
                }
                ty = crate::seq::elem(&ty)?.clone();
            } else {
                fields.push(segment.clone());
            }
        }
        if fields.is_empty() {
            Some(ty)
        } else {
            self.world.field(&ty, &fields)
        }
    }
    fn place(&mut self, p: &Place, env: &mut Env, usage: Usage, span: Span) -> Type {
        let Some(b) = env.bindings.get(&p.root).cloned() else {
            self.error("E0201", format!("unknown binding '{}'", p.root), span);
            return Type::Infer;
        };
        if b.frozen && matches!(usage, Usage::Borrow(Pass::Inout)) {
            self.error(
                "E0313",
                format!(
                    "cannot mutate '{}': it is bound by 'let' without 'mut'",
                    p.root
                ),
                span.clone(),
            );
            self.related(b.span.clone(), "binding introduced here");
            self.help("Declare the binding with 'let mut' to assign to it or pass it 'inout'.");
        }
        let mut transfer_allowed = true;
        if b.moved
            .iter()
            .any(|path| path.starts_with(&p.path) || p.path.starts_with(path))
        {
            transfer_allowed = false;
            self.error(
                "E0303",
                format!("use of moved place '{}'", p.display()),
                span.clone(),
            );
            self.related(b.span.clone(), "binding introduced here");
            for cause in &b.move_causes {
                if cause.path.starts_with(&p.path) || p.path.starts_with(&cause.path) {
                    self.related(
                        cause.span.clone(),
                        "earlier ownership-taking use of this place",
                    );
                }
            }
            self.help("A moved non-copy place cannot be used again, including through an overlapping projection.");
        }
        if !b.projections.is_empty() && !b.projections.iter().any(|path| p.path.starts_with(path)) {
            transfer_allowed = false;
            self.error(
                "E0304",
                format!(
                    "'{}' is outside the parameter's declared projections",
                    p.display()
                ),
                span.clone(),
            );
        }
        let ty = self.place_type(&b.ty, &p.path).unwrap_or_else(|| {
            transfer_allowed = false;
            self.error(
                "E0202",
                format!("unknown field in '{}'", p.display()),
                span.clone(),
            );
            Type::Infer
        });
        for (loan, mode, origin) in self.loans.clone() {
            let reserved =
                mode == Pass::Inout && self.reserved_loans.contains(&(origin.start, origin.end));
            let exclusive = (mode != Pass::In && !reserved)
                || matches!(usage, Usage::Borrow(Pass::Inout))
                || (matches!(usage, Usage::Own) && self.mode(&ty) != Mode::Copy);
            if exclusive && loan.overlaps(p) {
                transfer_allowed = false;
                self.error(
                    "E0301",
                    format!(
                        "access to '{}' conflicts with active argument loan '{}'",
                        p.display(),
                        loan.display()
                    ),
                    span.clone(),
                );
                self.related(origin, "conflicting argument access requested here");
                self.related(b.span.clone(), "accessed binding introduced here");
                self.help("An exclusive argument loan cannot overlap another access while the call's arguments are evaluated.");
            }
        }
        if (matches!(usage, Usage::Borrow(Pass::Inout))
            || (matches!(usage, Usage::Own) && self.mode(&ty) != Mode::Copy))
            && !self.loop_borrows.is_empty()
            && !self.check_loop_borrows(p, &span)
        {
            transfer_allowed = false;
        }
        if matches!(usage, Usage::Own)
            && self.mode(&ty) != Mode::Copy
            && !self.view_sources.is_empty()
            && !self.check_view_sources(p, env, &span)
        {
            transfer_allowed = false;
        }
        let writes = matches!(usage, Usage::Borrow(Pass::Inout))
            || (matches!(usage, Usage::Own) && self.mode(&ty) != Mode::Copy);
        for (borrowed, origin, lent) in self.match_borrows.clone() {
            // An `inout` element view (§13) lends its sequence
            // exclusively: the arms cannot even read it.
            if (writes || lent == Pass::Inout) && borrowed.overlaps(p) {
                transfer_allowed = false;
                self.error(
                    "E0301",
                    format!(
                        "access to '{}' conflicts with the borrow of '{}' by an enclosing match",
                        p.display(),
                        borrowed.display()
                    ),
                    span.clone(),
                );
                self.related(origin, "this match lends the place to its arms");
                self.help("The bindings of a match on a borrowed place borrow its payloads; the place cannot be written, lent `inout` or moved until the match ends.");
            }
        }
        match usage {
            Usage::Own if self.mode(&ty) != Mode::Copy => {
                if b.pass != Pass::Sink {
                    transfer_allowed = false;
                    self.error(
                        "E0305",
                        format!("cannot move '{}' through a borrowed parameter", p.display()),
                        span.clone(),
                    );
                    self.related(b.span.clone(), "borrowed parameter declared here");
                    self.help("Borrowed parameters do not transfer ownership of non-copy values; exchange the value in place with swap(inout a, inout b) or replace(inout a, v).");
                }
                if !p.path.is_empty() && self.mode(&b.ty) == Mode::Linear {
                    transfer_allowed = false;
                    self.error(
                        "E0306",
                        "destructure a linear aggregate before moving its fields",
                        span.clone(),
                    );
                    self.related(b.span.clone(), "linear aggregate binding introduced here");
                }
                if let Some(b) = env.bindings.get_mut(&p.root) {
                    b.moved.push(p.path.clone());
                    // Keep error recovery unchanged, but failed move attempts
                    // are not witnesses of a preceding ownership transfer.
                    if transfer_allowed {
                        b.remember_move(&p.path, span.clone());
                    }
                }
            }
            Usage::Borrow(Pass::Inout) if b.pass == Pass::In => {
                self.error(
                    "E0307",
                    format!("cannot mutate shared parameter '{}'", p.root),
                    span,
                );
                self.related(b.span.clone(), "shared parameter declared here");
            }
            _ => {}
        }
        ty
    }
    fn expr(&mut self, e: &Expr, env: &mut Env, usage: Usage) -> Type {
        self.expr_expected(e, env, usage, &Type::Infer)
    }
    fn expr_expected(&mut self, e: &Expr, env: &mut Env, usage: Usage, expected: &Type) -> Type {
        let before = if matches!(e.kind, ExprKind::Handle { .. } | ExprKind::Closure { .. }) {
            Some(env.clone())
        } else {
            None
        };
        let ty = match &e.kind {
            ExprKind::Literal(l) => {
                if let Literal::String(s) = l {
                    self.string_capacity = self.string_capacity.max(s.len());
                }
                let t = literal_type(l);
                if let Type::IntLiteral(n) = t {
                    if let Some(kind) = crate::numeric::IntegerKind::from_type(expected) {
                        if !kind.fits(n) {
                            self.error(
                                "E0209",
                                format!("integer literal {n} is outside the range of {expected}"),
                                e.span.clone(),
                            );
                        }
                        kind.ty()
                    } else {
                        if !crate::numeric::IntegerKind::default_for(n).fits(n) {
                            self.error(
                                "E0209",
                                "integer literal is outside the supported ranges",
                                e.span.clone(),
                            );
                        }
                        t
                    }
                } else if matches!(l, Literal::String(_))
                    && self.static_text
                    && crate::strings::is_str_type(expected)
                {
                    // A literal is static text: a `-> Str` function returns
                    // it as a view of its static bytes.
                    expected.clone()
                } else if matches!(l, Literal::Float(_))
                    && crate::numeric::FloatKind::from_type(expected)
                        == Some(crate::numeric::FloatKind::F32)
                {
                    // An unsuffixed float literal in f32 context
                    // (`vesper.float.ieee754/1`).
                    expected.clone()
                } else {
                    t
                }
            }
            ExprKind::Name(n) => {
                if env.bindings.contains_key(n) {
                    let t = self.place(
                        &Place {
                            root: n.clone(),
                            path: vec![],
                        },
                        env,
                        usage,
                        e.span.clone(),
                    );
                    self.literal_use(n, &e.span, env, expected, t)
                } else if let Some(f) = self.world.functions.get(n) {
                    function_value_type(f, self.world.function_results.get(&f.name))
                } else if let Some((a, v)) = self.world.constructor(n) {
                    if v.fields.is_empty() {
                        if let Type::Named(name, _) = expected
                            && name == &a.name
                        {
                            expected.clone()
                        } else {
                            Type::Named(
                                a.name.clone(),
                                a.generics.iter().map(|_| Type::Infer).collect(),
                            )
                        }
                    } else {
                        self.error("E0203", "constructor requires arguments", e.span.clone());
                        Type::Infer
                    }
                } else if n.starts_with("self::") || n.starts_with("super::") || n.starts_with('@')
                {
                    // Qualified paths resolve only through the package
                    // loader (`crate::packages`), like `use` imports.
                    self.error(
                        crate::packages::USE_OUTSIDE_PACKAGE,
                        format!("the qualified path '{n}' needs a package; check the package directory instead of one file"),
                        e.span.clone(),
                    );
                    Type::Infer
                } else {
                    self.error("E0201", format!("unknown name '{n}'"), e.span.clone());
                    Type::Infer
                }
            }
            ExprKind::Tuple(es) => Type::Tuple(
                es.iter()
                    .enumerate()
                    .map(|(i, e)| {
                        // Float elements, also in nested tuples, take their
                        // kind from the context (`vesper.float.ieee754/1`);
                        // integer literals
                        // settle later (`constrain_literals`).
                        let goal = match expected {
                            Type::Tuple(ts)
                                if ts.len() == es.len()
                                    && (crate::numeric::FloatKind::from_type(&ts[i]).is_some()
                                        || matches!(ts[i], Type::Tuple(_))) =>
                            {
                                ts[i].clone()
                            }
                            _ => Type::Infer,
                        };
                        self.expr_expected(e, env, Usage::Own, &goal)
                    })
                    .collect(),
            ),
            ExprKind::Struct { name, fields } => self.construct(
                name,
                fields.iter().map(|(n, e)| (n.clone(), e)).collect(),
                env,
                e.span.clone(),
                expected,
            ),
            ExprKind::Field(_, _) => {
                let ExprKind::Field(base, f) = &e.kind else {
                    unreachable!()
                };
                if crate::tiled::is_seq_type(&self.peek_type(base, env)) {
                    self.error(
                        "E0202",
                        format!(
                            "cannot access TiledSeq column '{f}' directly; use col_get[{f}], col_set[{f}], or load_tile_vec[{f}]"
                        ),
                        e.span.clone(),
                    );
                    self.expr(base, env, usage);
                    Type::Infer
                } else if let Some(p) = Place::from_expr(e) {
                    self.place(&p, env, usage, e.span.clone())
                } else {
                    let t = self.expr(base, env, Usage::Own);
                    if self.mode(&t) == Mode::Linear {
                        self.error(
                            "E0306",
                            "destructure a temporary linear aggregate before projecting its fields",
                            e.span.clone(),
                        );
                    }
                    self.world
                        .field(&t, std::slice::from_ref(f))
                        .unwrap_or_else(|| {
                            self.error("E0202", "unknown field", e.span.clone());
                            Type::Infer
                        })
                }
            }
            ExprKind::Index(..) if let Some(t) = self.string_interval(e, env) => t,
            ExprKind::Index(base, index) if self.seq_index_applies(e, base, env) => self
                .seq_index(e, base, index, env, usage)
                .unwrap_or(Type::Infer),
            ExprKind::Index(base, index) => {
                let t = self.expr(base, env, Usage::Borrow(Pass::In));
                let i = self.expr(index, env, Usage::Own);
                if !integer(&i) {
                    self.error("E0200", "index must be an integer", index.span.clone());
                }
                match t {
                    Type::Tuple(ts) => {
                        if ts.is_empty() {
                            self.error("E0200", "cannot index an empty tuple", e.span.clone());
                        }
                        if !self
                            .integer_fact(index, env)
                            .is_some_and(|n| n >= 0 && n < ts.len() as i128)
                            && !self.array_index_total(index, env, ts.len())
                        {
                            self.primitive_failure(env, e.span.clone());
                        }
                        let mut ty = Type::Infer;
                        for t in &ts {
                            ty = self.join_types(&ty, t, e.span.clone());
                        }
                        if ts.iter().any(|t| !compatible(&ty, t)) || self.mode(&ty) != Mode::Copy {
                            self.error("E0311", "dynamic indexing requires homogeneous copy elements; destructure owned aggregates", e.span.clone());
                        }
                        for t in &ts {
                            self.expect_type(&ty, t, e.span.clone());
                        }
                        self.constrain_literals(base, &Type::Tuple(vec![ty.clone(); ts.len()]));
                        self.integer_fact(index, env)
                            .and_then(|n| usize::try_from(n).ok())
                            .and_then(|n| ts.get(n))
                            .cloned()
                            .unwrap_or(ty)
                    }
                    _ => {
                        self.error("E0200", "indexing requires a tuple", base.span.clone());
                        Type::Infer
                    }
                }
            }
            ExprKind::Block { statements, tail } => {
                let original = env.clone();
                self.shadowed.push(BTreeMap::new());
                let mut result = Type::unit();
                for s in statements {
                    if result.name() == "Never" {
                        break;
                    }
                    match s {
                        Statement::Let {
                            pat,
                            ty,
                            value,
                            mutable,
                            ..
                        } => {
                            let hinted = if *ty == Type::Infer {
                                self.literal_annotation(pat)
                            } else {
                                None
                            };
                            let ty = &self.region_annotation(hinted.as_ref().unwrap_or(ty));
                            self.valid_type(ty, &self.generics.clone(), value.span.clone());
                            let bound_view = !*mutable
                                && matches!(pat.kind, PatternKind::Bind(_))
                                && self.bind_view(value, env);
                            let actual = self.expr_expected(value, env, Usage::Own, ty);
                            self.expect_type(ty, &actual, value.span.clone());
                            let t = if *ty == Type::Infer {
                                crate::numeric::default_type(&actual)
                            } else if hinted.is_some() {
                                let t =
                                    crate::numeric::default_type(&refine_annotation(ty, &actual));
                                // The value has the binding's type (later passes
                                // annotate unannotated lets from it).
                                self.types
                                    .insert((value.span.start, value.span.end), t.clone());
                                t
                            } else {
                                refine_annotation(ty, &actual)
                            };
                            if *ty == Type::Infer {
                                self.open_literal_bindings(pat, &actual);
                            }
                            if unresolved(&t) {
                                self.error("E0208", "cannot infer an ownership-relevant type; add a type annotation", value.span.clone());
                            }
                            if !self.irrefutable(pat, &t) {
                                self.error(
                                    "E0210",
                                    "let pattern must be irrefutable; use match",
                                    pat.span.clone(),
                                );
                            }
                            for n in pattern_names(pat) {
                                if let (Some(before), Some(current)) =
                                    (original.bindings.get(&n), env.bindings.get(&n))
                                    && before.span == current.span
                                    && let Some(shadowed) = self.shadowed.last_mut()
                                {
                                    shadowed.insert(n, current.clone());
                                }
                            }
                            let fact = self.integer_fact(value, env);
                            let nonzero = self.nonzero(value, env);
                            let bounds = self.integer_bounds(value, env);
                            self.bind(pat, &t, env);
                            if let PatternKind::Bind(n) = &pat.kind {
                                let binding = env.bindings.get_mut(n).unwrap();
                                if bound_view {
                                    // A bound view shares its source.
                                    binding.pass = Pass::In;
                                }
                                binding.integer = fact;
                                binding.nonzero = nonzero;
                                binding.range = bounds.map(|(lo, hi)| IntRange {
                                    lo,
                                    hi,
                                    at: Arc::clone(&binding.mutation),
                                    looped: false,
                                });
                                if bound_view {
                                    let span = binding.span.clone();
                                    self.record_view_source(value, n, span, env);
                                }
                            }
                            for n in pattern_names(pat) {
                                if let Some(binding) = env.bindings.get_mut(&n) {
                                    binding.frozen = !*mutable;
                                }
                            }
                        }
                        Statement::Expr(e) => {
                            let t = self.expr(e, env, Usage::Own);
                            if self.mode(&t) == Mode::Linear {
                                self.error(
                                    "E0302",
                                    "unused linear result must be consumed",
                                    e.span.clone(),
                                );
                            }
                            if t.name() == "Never" {
                                result = t;
                            }
                        }
                    }
                }
                if result.name() != "Never"
                    && let Some(tail) = tail
                {
                    result = self.expr_expected(tail, env, Usage::Own, expected);
                }
                let locals = env
                    .bindings
                    .iter()
                    .filter(|(n, b)| {
                        original
                            .bindings
                            .get(*n)
                            .is_none_or(|old| old.span != b.span)
                    })
                    .map(|(n, _)| n.clone())
                    .collect::<Vec<_>>();
                self.exhaust(env, locals.clone(), e.span.clone());
                for n in locals {
                    env.bindings.remove(&n);
                }
                for (n, b) in self.shadowed.pop().unwrap_or_default() {
                    env.bindings.insert(n, b);
                }
                result
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => self.call(callee, types, args, env, e.span.clone(), expected),
            ExprKind::Unary(op, v) => {
                let t = self.expr_expected(v, env, Usage::Own, expected);
                let t = crate::numeric::default_type(&t);
                if op == "~" {
                    self.check_complement(&t, e.span.clone());
                    t
                } else if op == "!" && self.integer_not(&t, e.span.clone()) {
                    t
                } else if op == "!" {
                    self.expect_type(&Type::named("bool"), &t, e.span.clone());
                    Type::named("bool")
                } else {
                    if !numeric(&t) {
                        self.error("E0200", "negation requires a number", e.span.clone());
                    }
                    if let Some(kind) = crate::numeric::IntegerKind::from_type(&t)
                        && self
                            .integer_fact(v, env)
                            .and_then(|n| integer_widths_rules::fact_neg(kind, n))
                            .is_none()
                    {
                        self.overflow_failure(env, e.span.clone());
                    }
                    t
                }
            }
            ExprKind::Binary(op, l, r)
                if crate::strings::is_comparison(op)
                    && (self.text_operand(l, env) || self.text_operand(r, env)) =>
            {
                self.string_comparison(l, r, env)
            }
            ExprKind::Binary(op, l, r) if crate::numeric::is_shift(op) => {
                self.shift_expr(op, l, r, env, e.span.clone(), expected)
            }
            ExprKind::Binary(op, l, r) => {
                let bit = crate::numeric::is_bit_operator(op);
                let arithmetic = bit || matches!(op.as_str(), "+" | "-" | "*" | "/" | "%");
                let goal = if matches!(op.as_str(), "&&" | "||") {
                    Type::named("bool")
                } else if arithmetic && numeric(expected) && *expected != Type::Infer {
                    expected.clone()
                } else {
                    Type::Infer
                };
                let a = self.expr_expected(l, env, Usage::Own, &goal);
                // The dividend's fact is the value already evaluated: the right
                // operand may reassign a binding the left one read.
                let left_fact = self.integer_fact(l, env);
                let left_bounds = self.integer_bounds(l, env);
                let left_index = Self::loop_index(l, env);
                let left_operand = self.total_operand_bounds(l, env);
                let right_goal = if goal != Type::Infer {
                    goal
                } else if float_math_rules::float_literal_tree(l) {
                    // `2.0 * x`: the right operand decides the float kind.
                    Type::Infer
                } else if !matches!(a, Type::IntLiteral(_)) {
                    a.clone()
                } else {
                    Type::Infer
                };
                let b = if matches!(op.as_str(), "&&" | "||") {
                    let before = env.clone();
                    let dead = matches!(
                        (&l.kind, op.as_str()),
                        (ExprKind::Literal(Literal::Bool(false)), "&&")
                            | (ExprKind::Literal(Literal::Bool(true)), "||")
                    );
                    self.dead += usize::from(dead);
                    let b = self.expr_expected(r, env, Usage::Own, &right_goal);
                    self.dead -= usize::from(dead);
                    let after = env.clone();
                    if dead {
                        *env = before;
                    } else {
                        self.merge(env, &before, &after, e.span.clone());
                    }
                    b
                } else if matches!(op.as_str(), "/" | "%") {
                    self.divisor(r, env, &right_goal)
                } else {
                    self.expr_expected(r, env, Usage::Own, &right_goal)
                };
                // A float literal left operand takes an f32 right operand's
                // kind (`2.0 * x`).
                let a = self.retype_float_literal(l, env, &a, &b);
                let common = match (&a, &b) {
                    (Type::IntLiteral(x), Type::IntLiteral(y)) => {
                        crate::numeric::IntegerKind::default_for((*x).max(*y)).ty()
                    }
                    (Type::IntLiteral(_), t) => t.clone(),
                    (t, _) => t.clone(),
                };
                self.expect_type(&common, &a, l.span.clone());
                self.expect_type(&common, &b, r.span.clone());
                if matches!(a, Type::IntLiteral(_)) {
                    self.types
                        .insert((l.span.start, l.span.end), common.clone());
                }
                if matches!(b, Type::IntLiteral(_)) {
                    self.types
                        .insert((r.span.start, r.span.end), common.clone());
                }
                if bit {
                    self.check_bit_operands(op, &common, e.span.clone());
                } else if (arithmetic || matches!(op.as_str(), "<" | "<=" | ">" | ">="))
                    && !numeric(&common)
                {
                    self.error(
                        "E0200",
                        "this operator requires numeric operands",
                        e.span.clone(),
                    );
                }
                if matches!(op.as_str(), "==" | "!=") && self.mode(&common) == Mode::Linear {
                    self.error("E0302","equality cannot discharge linear obligations; compare copy fields and explicitly consume their owners",e.span.clone());
                } else if matches!(op.as_str(), "==" | "!=") && self.extern_handle(&common) {
                    self.error(
                        "E0200",
                        "extern type handles are opaque and cannot be compared; ask their library",
                        e.span.clone(),
                    );
                }
                if matches!(op.as_str(), "/" | "%")
                    && integer(&common)
                    && !self.nonzero(r, env)
                    && !self.nonzero_divisor(r, env)
                {
                    self.primitive_failure(env, e.span.clone());
                }
                if let Some(kind) = crate::numeric::IntegerKind::from_type(&common)
                    && crate::numeric::may_overflow(op, kind)
                    && !self.in_range(op, kind, left_fact, self.integer_fact(r, env))
                    && !Self::quotient_in_range(op, kind, left_bounds, self.integer_bounds(r, env))
                    && !self.total_operation(op, kind, left_operand, r, env)
                    && !((left_index || Self::loop_index(r, env))
                        && Self::bounds_in_range(
                            op,
                            kind,
                            left_bounds,
                            self.integer_bounds(r, env),
                        ))
                {
                    self.overflow_failure(env, e.span.clone());
                }
                if matches!(
                    op.as_str(),
                    "&&" | "||" | "==" | "!=" | "<" | "<=" | ">" | ">="
                ) {
                    Type::named("bool")
                } else {
                    common
                }
            }
            ExprKind::Assign(op, l, r) if let Some(t) = self.array_assign(e, op, l, r, env) => t,
            ExprKind::Assign(op, l, r) => {
                let Some(p) = Place::from_expr(l) else {
                    // A field path through a sequence element is rewritten
                    // to an element view (`crate::seq::element_paths`),
                    // which needs the sequence's type.
                    self.note_element_bases(l, env);
                    self.error("E0300", "assignment requires a place", l.span.clone());
                    return Type::Infer;
                };
                if let ExprKind::Field(base, f) = &l.kind
                    && crate::tiled::is_seq_type(&self.peek_type(base, env))
                {
                    self.error(
                        "E0202",
                        format!(
                            "cannot assign TiledSeq column '{f}' directly; use col_set[{f}](index, value)"
                        ),
                        l.span.clone(),
                    );
                    self.expr(r, env, Usage::Own);
                    return Type::unit();
                }
                let expected = self.place(&p, env, Usage::Borrow(Pass::Inout), l.span.clone());
                self.types
                    .insert((l.span.start, l.span.end), expected.clone());
                let target_fact = |env: &Env| {
                    env.bindings
                        .get(&p.root)
                        .filter(|_| p.path.is_empty())
                        .and_then(|b| b.integer)
                };
                let before = target_fact(env);
                let target_state = |env: &Env| {
                    env.bindings
                        .get(&p.root)
                        .filter(|_| p.path.is_empty())
                        .map(|b| Arc::clone(&b.mutation))
                };
                let state_before = target_state(env);
                let bounds_before = state_before
                    .as_ref()
                    .and_then(|_| self.integer_bounds(l, env));
                let goal = self.literal_assignment(&p, r, env, &expected);
                let actual = self.expr_expected(r, env, Usage::Own, &goal);
                self.expect_type(&expected, &actual, r.span.clone());
                if bound_capabilities(&expected) != bound_capabilities(&actual) {
                    self.error(
                        "E0403",
                        "assignment cannot change a place's lexical capability scope",
                        r.span.clone(),
                    );
                }
                if self.mode(&expected) == Mode::Linear {
                    self.error(
                        "E0302",
                        "assignment cannot discard an existing linear value",
                        l.span.clone(),
                    );
                }
                if op != "=" && !numeric(&expected) {
                    self.error(
                        "E0200",
                        "compound assignment requires numeric operands",
                        l.span.clone(),
                    );
                }
                if matches!(op.as_str(), "/=" | "%=") && integer(&expected) && !self.nonzero(r, env)
                {
                    self.primitive_failure(env, e.span.clone());
                }
                // If the right side reassigns the target, either value may be
                // the one combined, so only an unchanged fact is trusted.
                let old = target_fact(env).filter(|_| target_fact(env) == before);
                let value = self.integer_fact(r, env);
                let value_bounds = self.integer_bounds(r, env);
                let old_bounds = bounds_before.filter(|_| {
                    matches!((&state_before, target_state(env)), (Some(a), Some(b)) if Arc::ptr_eq(a, &b))
                });
                let kind = crate::numeric::IntegerKind::from_type(&expected);
                if let Some(kind) = kind
                    && op != "="
                    && crate::numeric::may_overflow(op, kind)
                    && !self.in_range(&op[..1], kind, old, value)
                    && !Self::quotient_in_range(&op[..1], kind, old_bounds, value_bounds)
                {
                    self.overflow_failure(env, e.span.clone());
                }
                let fact = if op == "=" {
                    value
                } else {
                    kind.zip(old).zip(value).and_then(|((k, a), b)| {
                        integer_widths_rules::fact_checked(k, &op[..1], a, b).ok()
                    })
                };
                let nonzero = fact.is_some_and(|n| n != 0) || (op == "=" && self.nonzero(r, env));
                // A write to a field never establishes a fact about its root.
                let range = value_bounds.filter(|_| op == "=" && p.path.is_empty());
                if let Some(binding) = env.bindings.get_mut(&p.root) {
                    binding.mutation = Arc::new(());
                    binding.integer = fact.filter(|_| p.path.is_empty());
                    binding.nonzero = nonzero && p.path.is_empty();
                    binding.range = range.map(|(lo, hi)| IntRange {
                        lo,
                        hi,
                        at: Arc::clone(&binding.mutation),
                        looped: false,
                    });
                }
                Type::unit()
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let t = self.expr(condition, env, Usage::Own);
                self.expect_type(&Type::named("bool"), &t, condition.span.clone());
                let original = env.clone();
                let mut left = env.clone();
                self.refine_condition(condition, &mut left, true);
                let a = self.expr_expected(then, &mut left, Usage::Own, expected);
                let mut right = env.clone();
                self.refine_condition(condition, &mut right, false);
                let b = otherwise
                    .as_ref()
                    .map(|e| self.expr_expected(e, &mut right, Usage::Own, expected))
                    .unwrap_or_else(Type::unit);
                let result = self.join_types(&a, &b, e.span.clone());
                self.expect_type(&result, &a, then.span.clone());
                self.expect_type(&result, &b, e.span.clone());
                self.constrain_literals(then, &result);
                if let Some(otherwise) = otherwise {
                    self.constrain_literals(otherwise, &result);
                }
                if a.name() == "Never" {
                    *env = right;
                } else if b.name() == "Never" {
                    *env = left;
                } else {
                    self.merge(env, &left, &right, e.span.clone());
                    for (name, binding) in &mut env.bindings {
                        if binding.integer.is_none()
                            && let (Some(before), Some(a), Some(b)) = (
                                original.bindings.get(name),
                                left.bindings.get(name),
                                right.bindings.get(name),
                            )
                            && binding.span == before.span
                            && a.span == before.span
                            && b.span == before.span
                            && Arc::ptr_eq(&before.mutation, &a.mutation)
                            && Arc::ptr_eq(&before.mutation, &b.mutation)
                        {
                            binding.integer = before.integer;
                        }
                    }
                }
                result
            }
            ExprKind::Match { value, arms } => {
                // Matching a place of a borrowed (`in`/`inout`) binding whose
                // type is not copy borrows it: the pattern's bindings are
                // `in` borrows of its payloads, `Box` contents included, so a
                // borrowed tree can be walked any number of times (§4.3).
                let view = self.element_view_scrutinee(value, arms, env);
                let borrowed = view
                    .as_ref()
                    .map(|(pass, _)| *pass)
                    .or_else(|| self.borrowed_scrutinee(value, env));
                let usage = borrowed.map_or(Usage::Own, |_| Usage::Borrow(Pass::In));
                self.element_view = view.as_ref().map(|(pass, _)| *pass);
                let t = self.expr(value, env, usage);
                self.element_view = None;
                self.coverage(arms, &t, e.span.clone());
                let borrow_base = self.match_borrows.len();
                match view {
                    // An element view (§13) borrows its sequence for the
                    // whole match: `in` lets the arms read it, `inout`
                    // lends it exclusively.
                    Some((pass, p)) => self.match_borrows.push((p, value.span.clone(), pass)),
                    None => {
                        if borrowed.is_some()
                            && let Some(p) = Place::from_expr(value)
                        {
                            self.match_borrows.push((p, value.span.clone(), Pass::In));
                        }
                    }
                }
                let original = env.clone();
                let mut branches = vec![];
                let mut result = Type::named("Never");
                // An or-pattern arm `p | q => e` arrives as consecutive arms
                // sharing one body expression (the same span). Each
                // alternative must bind the same names at the same types.
                let mut group: Option<(Span, Vec<(String, Type)>)> = None;
                for (pat, body) in arms {
                    let mut arm = original.clone();
                    self.bind_mode(pat, &t, &mut arm, borrowed.unwrap_or(Pass::Sink));
                    let repeat = group.as_ref().is_some_and(|(s, _)| *s == body.span);
                    let before = self.diagnostics.len();
                    let arm_type = self.expr_expected(body, &mut arm, Usage::Own, expected);
                    let names = arm
                        .bindings
                        .iter()
                        .filter(|(n, b)| {
                            original
                                .bindings
                                .get(*n)
                                .is_none_or(|old| old.span != b.span)
                        })
                        .map(|(n, _)| n.clone())
                        .collect::<Vec<_>>();
                    let typed = names
                        .iter()
                        .map(|n| (n.clone(), arm.bindings[n].ty.clone()))
                        .collect::<Vec<_>>();
                    if repeat {
                        // The shared body was checked for an earlier
                        // alternative: keep only diagnostics that are new.
                        let fresh = self.diagnostics.split_off(before);
                        for d in fresh {
                            if !self.diagnostics[..before].iter().any(|o| {
                                o.code == d.code && o.span == d.span && o.message == d.message
                            }) {
                                self.diagnostics.push(d);
                            }
                        }
                        if group.as_ref().is_some_and(|(_, first)| *first != typed) {
                            self.error(
                                "E0214",
                                "every alternative of an or-pattern must bind the same names at the same types",
                                pat.span.clone(),
                            );
                        }
                    } else {
                        group = Some((body.span.clone(), typed));
                    }
                    self.exhaust(&arm, names.clone(), body.span.clone());
                    for n in names {
                        arm.bindings.remove(&n);
                        if let Some(old) = original.bindings.get(&n) {
                            arm.bindings.insert(n, old.clone());
                        }
                    }
                    if result.name() == "Never" {
                        result = arm_type.clone();
                    } else {
                        let common = self.join_types(&result, &arm_type, body.span.clone());
                        self.expect_type(&common, &result, body.span.clone());
                        self.expect_type(&common, &arm_type, body.span.clone());
                        result = common;
                    }
                    if arm_type.name() != "Never" {
                        branches.push(arm);
                    }
                }
                self.match_borrows.truncate(borrow_base);
                for (_, body) in arms {
                    self.constrain_literals(body, &result);
                }
                if let Some(first) = branches.first() {
                    *env = first.clone();
                    for branch in branches.iter().skip(1) {
                        let prev = env.clone();
                        self.merge(env, &prev, branch, e.span.clone());
                    }
                }
                result
            }
            ExprKind::Try(value) => {
                let target = if *expected != Type::Infer {
                    Type::Named("Result".into(), vec![expected.clone(), Type::Infer])
                } else {
                    Type::Infer
                };
                let t = self.expr_expected(value, env, Usage::Own, &target);
                if let Type::Named(n, args) = &t
                    && n == "Option"
                    && args.len() == 1
                {
                    return self.try_option(e, &args[0]);
                }
                if let Type::Named(n, args) = t {
                    if n == "Result" && args.len() == 2 {
                        self.require_effect(
                            &Effect {
                                label: None,
                                name: "Fail".into(),
                                args: vec![args[1].clone()],
                            },
                            env,
                            e.span.clone(),
                        );
                        if args[0] == Type::Infer {
                            // `Err(e)?` with nothing to infer its `Ok` type
                            // from: the operand never succeeds, so its `Ok`
                            // payload is `Unit` and every backend has a
                            // layout for it (SH5 lane 6, #1329).
                            let ok = Type::unit();
                            self.constrain_literals(
                                value,
                                &Type::Named("Result".into(), vec![ok.clone(), args[1].clone()]),
                            );
                            ok
                        } else {
                            args[0].clone()
                        }
                    } else {
                        self.error("E0200", TRY_OPERAND, e.span.clone());
                        Type::Infer
                    }
                } else {
                    self.error("E0200", TRY_OPERAND, e.span.clone());
                    Type::Infer
                }
            }
            ExprKind::Closure {
                params,
                mode,
                result,
                effects,
                infer_effects,
                body,
            } => {
                let mut params = params.clone();
                let mut result = result.clone();
                if let Type::Function {
                    params: expected_params,
                    result: expected_result,
                    ..
                } = expected
                {
                    if params.len() != expected_params.len() {
                        self.error(
                            "E0207",
                            "closure parameter count differs from expected function type",
                            e.span.clone(),
                        );
                    }
                    for (p, (pass, t)) in params.iter_mut().zip(expected_params) {
                        if p.pass != *pass {
                            self.error("E0308","closure parameter passing mode differs from expected function type",p.span.clone());
                        }
                        if p.ty == Type::Infer {
                            p.ty = t.clone();
                        }
                    }
                    if result == Type::Infer {
                        result = *expected_result.clone();
                    }
                }
                self.closure(
                    &params,
                    *mode,
                    &result,
                    effects,
                    *infer_effects,
                    body,
                    env,
                    e.span.clone(),
                )
            }
            ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_) => self
                .control_expr(e, env, expected)
                .expect("loop-family expression"),
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => self.handle(effect, state, body, ret, ops, env, e.span.clone(), expected),
        };
        let ty = instantiate_latent_effects(&ty, expected);
        if *expected != Type::Infer {
            self.expect_type(expected, &ty, e.span.clone());
            // A literal in an aggregate takes its 128-bit field's kind, whose
            // layout is two words (SH2 lane 2, #1174). Only types holding
            // `i128` or `u128` change.
            if crate::integer_widths::holds_wide(expected) {
                self.constrain_literals(e, expected);
            }
        }
        if let Some(before) = before {
            let mut moved = vec![];
            for (n, b) in &env.bindings {
                if let Some(old) = before.bindings.get(n)
                    && b.span == old.span
                {
                    for path in &b.moved {
                        if !old.moved.contains(path) {
                            moved.push(Place {
                                root: n.clone(),
                                path: path.clone(),
                            });
                        }
                    }
                }
            }
            self.moves.insert((e.span.start, e.span.end), moved);
        }
        self.types.insert((e.span.start, e.span.end), ty.clone());
        ty
    }
    fn construct(
        &mut self,
        name: &str,
        fields: Vec<(String, &Expr)>,
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        let Some((a, v)) = self.world.constructor(name) else {
            self.error("E0201", format!("unknown constructor '{name}'"), span);
            return Type::Infer;
        };
        if crate::fip::active(self.world) && a.name == crate::fip::BOX_TYPE {
            self.error(
                "E0800",
                "Box[T] must be constructed with Box::new or Box::new_in_caller",
                span.clone(),
            );
        }
        let mut vars = BTreeMap::new();
        if *expected != Type::Infer {
            self.valid_type(expected, &self.generics.clone(), span.clone());
        }
        if let Type::Named(n, args) = expected
            && n == &a.name
        {
            vars.extend(
                a.generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone())),
            );
        }
        let mut seen = BTreeSet::new();
        for (n, e) in fields {
            let mut context_vars = vars.clone();
            for g in &a.generics {
                context_vars.entry(g.name.clone()).or_insert(Type::Infer);
            }
            let target = v
                .fields
                .iter()
                .find(|f| f.name == n)
                .map(|f| substitute(&f.ty, &context_vars))
                .unwrap_or(Type::Infer);
            let t = self.expr_expected(e, env, Usage::Own, &target);
            if !seen.insert(n.clone()) {
                self.error("E0204", "duplicate constructor field", e.span.clone());
            }
            if let Some(f) = v.fields.iter().find(|f| f.name == n) {
                infer(&f.ty, &t, &a.generics, &mut vars);
                self.expect_type(&substitute(&f.ty, &vars), &t, e.span.clone());
            } else {
                self.error("E0202", format!("unknown field '{n}'"), e.span.clone());
            }
        }
        if seen.len() != v.fields.len() {
            self.error(
                "E0204",
                format!("constructor '{name}' requires {} fields", v.fields.len()),
                span.clone(),
            );
        }
        self.generic_bounds(&a.generics, &vars, span);
        Type::Named(
            a.name.clone(),
            a.generics
                .iter()
                .map(|g| vars.get(&g.name).cloned().unwrap_or(Type::Infer))
                .collect(),
        )
    }
    fn generic_bounds(&mut self, params: &[Generic], vars: &BTreeMap<String, Type>, span: Span) {
        for g in params {
            if let Some(t) = vars.get(&g.name)
                && *t != Type::Infer
                && g.row != matches!(t, Type::Row(_))
            {
                self.error(
                    "E0205",
                    "generic type and effect-row arguments have different kinds",
                    span.clone(),
                );
            }
            if let Some(t) = vars.get(&g.name)
                && !g.row
                && self.mode(t) > g.bound
            {
                self.error(
                    "E0205",
                    format!(
                        "type {t} has mode {:?}, exceeding {:?} bound for {}",
                        self.mode(t),
                        g.bound,
                        g.name
                    ),
                    span.clone(),
                );
            }
        }
    }
    fn call(
        &mut self,
        callee: &Expr,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        if let ExprKind::Name(name) = &callee.kind
            && !env.bindings.contains_key(name)
        {
            if crate::fip::active(self.world)
                && matches!(
                    name.as_str(),
                    crate::fip::BOX_NEW | crate::fip::BOX_NEW_IN_CALLER
                )
            {
                return self.fip_box_call(name, types, args, env, span, expected);
            }
            if self.world.constructor(name).is_some() {
                let t = self.construct(
                    name,
                    args.iter()
                        .enumerate()
                        .map(|(i, a)| (i.to_string(), &a.expr))
                        .collect(),
                    env,
                    span.clone(),
                    &if types.is_empty() {
                        expected.clone()
                    } else {
                        Type::Named(
                            self.world.constructor(name).unwrap().0.name.clone(),
                            types.to_vec(),
                        )
                    },
                );
                if !types.is_empty()
                    && let Type::Named(n, actual) = &t
                {
                    let expected = Type::Named(n.clone(), types.to_vec());
                    self.expect_type(&expected, &Type::Named(n.clone(), actual.clone()), span);
                    return expected;
                }
                return t;
            }
            if crate::numeric::wrapping_intrinsic(name).is_some() {
                return self.wrapping_call(name, types, args, env, span, expected);
            }
            if !self.world.functions.contains_key(name)
                && let Some(t) = self.vector_call(name, types, args, env, span.clone(), expected)
            {
                return t;
            }
            if !self.world.functions.contains_key(name)
                && let Some(t) = self.tiled_call(name, types, args, env, span.clone(), expected)
            {
                return t;
            }
            if !self.world.functions.contains_key(name)
                && let Some(t) = self.seq_call(name, types, args, env, span.clone(), expected)
            {
                return t;
            }
            if let Some(f) = self.world.functions.get(name) {
                return self.apply(f, types, args, None, env, span, expected);
            }
            // Bitwise intrinsics behind the built-in secret operations
            // (`capability_effects`); user functions take precedence.
            if crate::capability_effects::bit_intrinsic(name).is_some() {
                return self.wrapping_call(name, types, args, env, span, expected);
            }
            // Integer conversions and rotations (`vesper.arith.checked/1`);
            // user functions take precedence.
            if let Some(conversion) = crate::numeric::conversion(name) {
                return self.conversion_call(name, conversion, types, args, env, span);
            }
            if crate::numeric::rotate_intrinsic(name).is_some() {
                return self.rotate_call(name, types, args, env, span, expected);
            }
            // Float intrinsics and conversions (`vesper.float.ieee754/1`);
            // user functions take precedence.
            if let Some(op) = crate::numeric::float::math_intrinsic(name) {
                return self.math_call(op, types, args, env, span, expected);
            }
            if let Some(c) = crate::numeric::float::float_conversion(name) {
                return self.float_conversion_call(name, c, types, args, env, span);
            }
            if let Some(k) = crate::numeric::float::saturating_conversion(name) {
                return self.saturating_call(name, k, types, args, env, span);
            }
            if let Some(b) = crate::numeric::float::bits_intrinsic(name) {
                return self.bits_call(name, b, types, args, env, span);
            }
            if name
                .split_once("::")
                .is_some_and(|(e, _)| e == crate::regions::REGION_EFFECT)
                && crate::regions::active(self.world)
            {
                self.error(
                    "E0790",
                    "region operations are called through their capability, as in `r.box(value)`",
                    span,
                );
                for a in args {
                    self.expr(&a.expr, env, Usage::Own);
                }
                return Type::Infer;
            }
            if let Some((eff, op)) = name.split_once("::")
                && let Some(decl) = self.world.effects.get(eff)
                && let Some(f) = decl.ops.iter().find(|f| f.name == op)
            {
                let f = f.clone();
                let mut vars = BTreeMap::new();
                let mut f = f;
                f.generics = decl.generics.clone();
                let context = self
                    .active_effect(&Effect {
                        label: None,
                        name: eff.into(),
                        args: vec![],
                    })
                    .or_else(|| {
                        self.allowed
                            .instances
                            .iter()
                            .rev()
                            .find(|e| e.name == eff)
                            .cloned()
                    });
                let inferred_types = if types.is_empty() {
                    context.map(|e| e.args).unwrap_or_default()
                } else {
                    types.to_vec()
                };
                let result = self.apply_args(
                    &f,
                    &inferred_types,
                    args,
                    None,
                    env,
                    span.clone(),
                    &mut vars,
                );
                let effect = Effect {
                    label: None,
                    name: eff.into(),
                    args: decl
                        .generics
                        .iter()
                        .map(|g| vars.get(&g.name).cloned().unwrap_or(Type::Infer))
                        .collect(),
                };
                self.require_effect(&effect, env, span);
                return result;
            }
            if name.contains("::")
                && let Some(f) =
                    self.trait_call(name, true, args.first().map(|a| &a.expr), env, &span)
            {
                return match f {
                    Some(f) => self.apply(&f, types, args, None, env, span, expected),
                    None => Type::Infer,
                };
            }
        }
        if let ExprKind::Field(base, method) = &callee.kind {
            if let ExprKind::Name(label) = &base.kind
                && !env.bindings.contains_key(label)
                && let Some(effect) = self.region_capability(label)
            {
                return self.region_op(&effect, method, args, env, span, expected);
            }
            if let ExprKind::Name(label) = &base.kind
                && !env.bindings.contains_key(label)
                && let Some(effect) = self
                    .caps
                    .iter()
                    .rev()
                    .chain(self.allowed.instances.iter())
                    .find(|e| {
                        e.label.as_ref() == Some(self.cap_aliases.get(label).unwrap_or(label))
                    })
                    .cloned()
                && let Some(decl) = self.world.effects.get(&effect.name)
                && let Some(f) = decl.ops.iter().find(|f| &f.name == method)
            {
                let mut f = f.clone();
                f.generics = decl.generics.clone();
                let mut vars = BTreeMap::new();
                let result =
                    self.apply_args(&f, &effect.args, args, None, env, span.clone(), &mut vars);
                self.require_effect(&effect, env, span);
                return result;
            }
            if let Some(t) =
                self.tiled_method_call(base, method, types, args, env, span.clone(), expected)
            {
                return t;
            }
            if let Some(t) = self.string_method_call(base, method, types, args, env, span.clone()) {
                return t;
            }
            if let Some(t) = self.seq_method_call(base, method, types, args, env, span.clone()) {
                return t;
            }
            if let Some(t) = self.array_method_call(base, method, args, env, span.clone()) {
                return t;
            }
            let mut t = self.peek_type(base, env);
            if t == Type::Infer && crate::check::Place::from_expr(base).is_none() {
                // A literal or call receiver (`"a".concat(b)`).
                t = self.receiver_type(base, env);
            }
            let name = format!("{}::{method}", t.name());
            if let Some(f) = self.world.functions.get(&name) {
                return self.apply(f, types, args, Some(base), env, span, expected);
            }
            if let Some(f) = self.trait_call(method, false, Some(base), env, &span) {
                return match f {
                    Some(f) => self.apply(&f, types, args, Some(base), env, span, expected),
                    None => Type::Infer,
                };
            }
        }
        // Calling a lent function parameter (`f: fn(..)` passed `in` with a
        // function type that is not `copy`, §4.2) reads it: it may be
        // called any number of times and is never consumed.
        let lent_callee = matches!(&callee.kind, ExprKind::Name(n)
            if env.bindings.get(n).is_some_and(|b| b.pass != Pass::Sink
                && b.projections.is_empty()
                && matches!(&b.ty, Type::Function { mode, .. } if *mode != Mode::Copy)));
        let t = self.expr(
            callee,
            env,
            if lent_callee {
                Usage::Borrow(Pass::In)
            } else {
                Usage::Own
            },
        );
        if let Type::Function {
            params,
            result,
            effects,
            mode,
        } = t
        {
            let f = Function {
                name: "<closure>".into(),
                fip: false,
                generics: vec![],
                params: params
                    .iter()
                    .enumerate()
                    .map(|(i, (pass, ty))| Param {
                        name: i.to_string(),
                        pass: *pass,
                        ty: ty.clone(),
                        projections: vec![],
                        span: span.clone(),
                    })
                    .collect(),
                result: *result,
                effects,
                body: None,
                span: span.clone(),
            };
            let result = self.apply(&f, types, args, None, env, span, expected);
            if mode != Mode::Copy {
                env.invalidate_borrowed_facts();
            }
            result
        } else {
            self.error("E0206", format!("{t} is not callable"), span);
            Type::Infer
        }
    }
    fn fip_box_call(
        &mut self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        if types.len() > 1 {
            self.error(
                "E0207",
                format!(
                    "'{name}' expects at most 1 type argument, found {}",
                    types.len()
                ),
                span.clone(),
            );
        }
        if args.len() != 1 {
            self.error(
                "E0207",
                format!("'{name}' expects 1 argument, found {}", args.len()),
                span.clone(),
            );
            for a in args {
                self.expr(&a.expr, env, Usage::Own);
            }
            return Type::Named(crate::fip::BOX_TYPE.into(), vec![Type::Infer]);
        }
        let arg = &args[0];
        if arg.explicit && arg.pass != Pass::Sink {
            self.error(
                "E0308",
                format!("argument mode {:?} does not match Sink parameter", arg.pass),
                arg.expr.span.clone(),
            );
        }
        let inner_expected = if let Some(t) = types.first() {
            self.valid_type(t, &self.generics.clone(), span.clone());
            t.clone()
        } else if let Type::Named(n, a) = expected
            && n == crate::fip::BOX_TYPE
            && a.len() == 1
        {
            a[0].clone()
        } else {
            Type::Infer
        };
        let boxing_object =
            crate::dyn_trait::is_object(&inner_expected) && name == crate::fip::BOX_NEW;
        let value_expected = if boxing_object {
            Type::Infer
        } else {
            inner_expected.clone()
        };
        let actual = self.expr_expected(&arg.expr, env, Usage::Own, &value_expected);
        if boxing_object {
            // A boxed trait object (§7.3): the box holds the concrete value,
            // whose conformance the frontend selects at this call.
            let actual = crate::numeric::default_type(&actual);
            if crate::dyn_trait::is_object(&actual)
                || matches!(&actual, Type::Named(g, a) if a.is_empty() && crate::dyn_trait::is_dyn_generic(g))
            {
                self.error(
                    "E0631",
                    format!("cannot box a `dyn` value as {inner_expected}: box the concrete value"),
                    arg.expr.span.clone(),
                );
            } else if self.mode(&actual) > Mode::Affine {
                self.error(
                    "E0205",
                    format!("cannot box the linear type {actual} as {inner_expected}: a trait object may be dropped, so its type must be at most affine"),
                    arg.expr.span.clone(),
                );
            }
            return Type::Named(crate::fip::BOX_TYPE.into(), vec![inner_expected]);
        }
        let inner = if inner_expected != Type::Infer {
            self.expect_type(&inner_expected, &actual, arg.expr.span.clone());
            refine_annotation(&inner_expected, &actual)
        } else {
            crate::numeric::default_type(&actual)
        };
        self.constrain_literals(&arg.expr, &inner);
        Type::Named(crate::fip::BOX_TYPE.into(), vec![inner])
    }
    /// The place an `inout` boxed object (§7.3) held by a sequence
    /// element `s[i]` lends: the whole sequence `s`. The element's box
    /// itself is only read; the call mutates the object it points to.
    fn boxed_element_place(&mut self, e: &Expr, env: &Env, param: &Param) -> Option<Place> {
        let ExprKind::Index(base, index) = &e.kind else {
            return None;
        };
        if matches!(index.kind, ExprKind::Range { .. }) {
            return None;
        }
        let element = self.receiver_type(e, env);
        if !crate::dyn_trait::is_boxed(&element) {
            // An element whose type is not copy is lent in place (an
            // element view, §13), and so is any element lent `inout`: the
            // call borrows the element's place in the sequence, which
            // overlaps the whole sequence and every element that may be the
            // same one ([`Place::element`]).
            if element == Type::Infer
                || (self.mode(&element) == Mode::Copy
                    && (param.pass != Pass::Inout
                        || !crate::seq::is_seq_or_slice(&self.peek_type(base, env))))
            {
                return None;
            }
            return Place::element(base, index);
        }
        // An `inout` lend may only reach the object: a vtable stub's
        // receiver or a `dyn` parameter. A callee taking the box itself
        // `inout` could replace it, and the element is not a place.
        if param.pass == Pass::Inout
            && !matches!(&param.ty, Type::Named(g, a)
                if a.is_empty() && (g == "$Self" || crate::dyn_trait::is_dyn_generic(g)))
        {
            return None;
        }
        Place::element(base, index)
    }
    /// Resolves a trait call through the frontend. `None`: not a trait call.
    /// `Some(None)`: a trait call that failed (diagnosed). `Some(Some(f))`:
    /// the selected generated function, recorded for elaboration.
    /// The static type of a trait-call receiver, determined without
    /// checking the expression (its ownership and effects are checked once,
    /// when the call is applied). Chained method calls are typed through
    /// the inherent method or the trait conformance their own receiver
    /// selects.
    fn receiver_type(&mut self, e: &Expr, env: &Env) -> Type {
        if let ExprKind::Literal(l) = &e.kind {
            return crate::numeric::default_type(&literal_type(l));
        }
        let t = self.peek_type(e, env);
        if t != Type::Infer {
            return t;
        }
        if let Some(t) = self.world.types.get(&(e.span.start, e.span.end))
            && *t != Type::Infer
        {
            return t.clone();
        }
        // Receivers that are not places: calls and struct literals have a
        // declared type, and a sequence element its sequence's.
        match &e.kind {
            ExprKind::Index(base, index) if !matches!(index.kind, ExprKind::Range { .. }) => {
                match self.receiver_type(base, env) {
                    Type::Named(n, args)
                        if (n == crate::seq::CHUNK_SEQ_TYPE || n == crate::seq::SLICE_TYPE)
                            && args.len() == 1 =>
                    {
                        args[0].clone()
                    }
                    _ => Type::Infer,
                }
            }
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Name(n) if !env.bindings.contains_key(n) => self
                    .world
                    .functions
                    .get(n)
                    .filter(|f| f.generics.is_empty())
                    .map(|f| f.result.clone())
                    .unwrap_or(Type::Infer),
                ExprKind::Field(base, method) => {
                    let base_ty = self.receiver_type(base, env);
                    if base_ty == Type::Infer {
                        return Type::Infer;
                    }
                    let inherent = format!("{}::{method}", base_ty.name());
                    let f = match self.world.functions.get(&inherent) {
                        Some(f) => Some(f.clone()),
                        None => self.traits.and_then(|traits| {
                            let generics = self.generics.clone();
                            let current = self.current.clone();
                            let call = TraitCall {
                                callee: method,
                                path: false,
                                receiver: base_ty.clone(),
                                generics: &generics,
                                function: &current,
                                span: e.span.clone(),
                            };
                            match traits.resolve(&call)? {
                                Ok(name) => {
                                    self.world.functions.get(&name).cloned().map(|mut f| {
                                        crate::pact_frontend::specialize_boxed_stub(
                                            &mut f, &base_ty,
                                        );
                                        f
                                    })
                                }
                                Err(_) => None,
                            }
                        }),
                    };
                    let Some(f) = f else {
                        return Type::Infer;
                    };
                    let Some(first) = f.params.first() else {
                        return Type::Infer;
                    };
                    let mut vars = BTreeMap::new();
                    infer(&first.ty, &base_ty, &f.generics, &mut vars);
                    if f.generics.iter().any(|g| !vars.contains_key(&g.name)) {
                        return Type::Infer;
                    }
                    substitute(&f.result, &vars)
                }
                _ => Type::Infer,
            },
            ExprKind::Struct { name, .. } => self
                .world
                .adts
                .get(name)
                .filter(|a| a.generics.is_empty() && !a.is_enum)
                .map(|a| Type::named(&a.name))
                .unwrap_or(Type::Infer),
            _ => Type::Infer,
        }
    }
    fn trait_call(
        &mut self,
        callee: &str,
        path: bool,
        receiver: Option<&Expr>,
        env: &Env,
        span: &Span,
    ) -> Option<Option<Function>> {
        let traits = self.traits?;
        let receiver = match receiver {
            Some(e) => self.receiver_type(e, env),
            None => Type::Infer,
        };
        let generics = self.generics.clone();
        let current = self.current.clone();
        let call = TraitCall {
            callee,
            path,
            receiver: receiver.clone(),
            generics: &generics,
            function: &current,
            span: span.clone(),
        };
        match traits.resolve(&call)? {
            Ok(name) => match self.world.functions.get(&name) {
                Some(f) => {
                    self.resolved.insert((span.start, span.end), name);
                    let mut f = f.clone();
                    crate::pact_frontend::specialize_boxed_stub(&mut f, &receiver);
                    Some(Some(f))
                }
                None => {
                    self.error(
                        "E0610",
                        format!("trait call resolved to missing function '{name}'"),
                        span.clone(),
                    );
                    Some(None)
                }
            },
            Err(d) => {
                self.diagnostics.push(d);
                Some(None)
            }
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the call typing judgment"
    )]
    fn apply(
        &mut self,
        f: &Function,
        types: &[Type],
        args: &[Arg],
        receiver: Option<&Expr>,
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        let mut vars = BTreeMap::new();
        if *expected != Type::Infer {
            infer(&f.result, expected, &f.generics, &mut vars);
        }
        self.apply_args(f, types, args, receiver, env, span.clone(), &mut vars);
        let mut labels = BTreeMap::new();
        for (i, e) in f.effects.instances.iter().enumerate() {
            let mut e = e.clone();
            e.args = e.args.iter().map(|t| substitute(t, &vars)).collect();
            let bound = self.resolve_effect(&e);
            self.require_effect(&e, env, span.clone());
            let target = bound.and_then(|e| e.label);
            labels.insert(function_capability(&f.name, i), target.clone());
            if let Some(label) = e.label
                && label.starts_with("$latent:")
            {
                labels.insert(label, target);
            }
        }
        if let Some(row) = &f.effects.tail {
            if let Some(Type::Row(effects)) = vars.get(row).cloned() {
                let mut resolved = effects.clone();
                for e in &mut resolved.instances {
                    let old = e.label.clone();
                    let bound = self.resolve_effect(e);
                    self.require_effect(e, env, span.clone());
                    if let Some(bound) = bound {
                        e.label = bound.label;
                    }
                    if let Some(label) = old
                        && label.starts_with("$latent:")
                    {
                        labels.insert(label, e.label.clone());
                    }
                }
                if effects.tail.is_some() {
                    self.observe(&Effects {
                        instances: vec![],
                        tail: effects.tail.clone(),
                    });
                    self.check_abort_frame(env, row, span.clone());
                    if effects.tail != self.allowed.tail {
                        self.error(
                            "E0406",
                            "unresolved effect row cannot escape this call",
                            span.clone(),
                        );
                    }
                }
                vars.insert(row.clone(), Type::Row(resolved));
            } else if self.allowed.tail.as_ref() == Some(row) {
                self.observe(&Effects {
                    instances: vec![],
                    tail: Some(row.clone()),
                });
                self.check_abort_frame(env, row, span.clone());
            } else {
                self.error(
                    "E0406",
                    format!("cannot infer effect row '{row}'"),
                    span.clone(),
                );
            }
        }
        let result = self
            .world
            .function_results
            .get(&f.name)
            .unwrap_or(&f.result);
        rename_capabilities(&substitute(result, &vars), &labels)
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the call typing judgment"
    )]
    fn apply_args(
        &mut self,
        f: &Function,
        types: &[Type],
        args: &[Arg],
        receiver: Option<&Expr>,
        env: &mut Env,
        span: Span,
        vars: &mut BTreeMap<String, Type>,
    ) -> Type {
        let loan_base = self.loans.len();
        let mut fixed_rows = BTreeMap::new();
        let mut explicit = types.iter().peekable();
        for (index, g) in f.generics.iter().enumerate() {
            let Some(t) = explicit.next() else {
                break;
            };
            let t = if g.row {
                let mut row = match t {
                    Type::Row(e) => e.clone(),
                    Type::Named(n, args) if self.world.effects.contains_key(n) => Effects {
                        instances: vec![Effect {
                            label: None,
                            name: n.clone(),
                            args: args.clone(),
                        }],
                        tail: None,
                    },
                    _ => {
                        self.error(
                            "E0205",
                            "effect-row argument requires effects or a row variable",
                            span.clone(),
                        );
                        Effects::default()
                    }
                };
                // Effect and data names are resolved using the formal kind.
                // A sequence of effects belongs to this row until the next
                // explicitly typed formal parameter or row argument.
                while explicit.len() > f.generics.len() - index - 1 {
                    let Some(next) = explicit.peek() else {
                        break;
                    };
                    if let Type::Named(n, args) = next
                        && self.world.effects.contains_key(n)
                    {
                        row.instances.push(Effect {
                            label: None,
                            name: n.clone(),
                            args: args.clone(),
                        });
                        explicit.next();
                        continue;
                    }
                    break;
                }
                for e in &mut row.instances {
                    if let Some(bound) = self.resolve_effect(e) {
                        *e = bound;
                    }
                }
                fixed_rows.insert(g.name.clone(), row.clone());
                Type::Row(row)
            } else {
                t.clone()
            };
            self.valid_type(&t, &self.generics.clone(), span.clone());
            vars.insert(g.name.clone(), t);
        }
        if explicit.next().is_some() {
            self.error("E0205", "too many type arguments", span.clone());
        }
        let mut actuals = vec![];
        if let Some(e) = receiver {
            actuals.push((e, None));
        }
        actuals.extend(
            args.iter()
                .map(|a| (&a.expr, if a.explicit { Some(a.pass) } else { None })),
        );
        if actuals.len() != f.params.len() {
            self.error(
                "E0207",
                format!(
                    "'{}' expects {} arguments, found {}",
                    f.name,
                    f.params.len(),
                    actuals.len()
                ),
                span.clone(),
            );
        }
        let mut loans: Vec<(Place, Pass, Span)> = vec![];
        let mut constraints = vec![];
        self.seq_call_slices(
            &actuals
                .iter()
                .zip(&f.params)
                .map(|((e, _), p)| (*e, p.pass))
                .collect::<Vec<_>>(),
            env,
        );
        for ((expr, explicit), param) in actuals.into_iter().zip(&f.params) {
            // Two-phase argument loans (§4.3): every `inout` argument, not
            // only the receiver, is reserved while the call's other
            // arguments are evaluated.
            if param.pass == Pass::Inout {
                self.reserved_loans.insert((expr.span.start, expr.span.end));
            }
            if let Some(pass) = explicit
                && pass != param.pass
            {
                self.error(
                    "E0308",
                    format!(
                        "argument mode {pass:?} does not match {:?} parameter",
                        param.pass
                    ),
                    expr.span.clone(),
                );
            }
            let place = Place::from_expr(expr)
                .or_else(|| self.seq_view_place(expr))
                .or_else(|| self.boxed_element_place(expr, env, param));
            if param.pass == Pass::Inout && place.is_none() {
                self.error(
                    "E0300",
                    "inout argument requires a place",
                    expr.span.clone(),
                );
            }
            if let Some(p) = &place {
                let paths = if param.projections.is_empty() {
                    vec![p.clone()]
                } else {
                    param
                        .projections
                        .iter()
                        .map(|path| {
                            let mut p = p.clone();
                            p.path.extend(path.clone());
                            p
                        })
                        .collect()
                };
                // A scalar passed to an `in` or by-value parameter is read
                // into a word before the call, so it may read the place of a
                // reserved `inout` argument loan (two-phase borrows, §4.3).
                // Copy aggregates may be lent by address and stay refused.
                let copied = param.pass != Pass::Inout
                    && param.projections.is_empty()
                    && is_scalar_type(&self.peek_type(expr, env));
                for p in paths {
                    for (old, mode, origin) in &loans {
                        let reserved = *mode == Pass::Inout
                            && copied
                            && self.reserved_loans.contains(&(origin.start, origin.end));
                        if p.overlaps(old)
                            && !reserved
                            && !(param.pass == Pass::In && *mode == Pass::In)
                        {
                            self.error("E0301",format!("overlapping arguments '{}' and '{}' violate alias XOR mutability",old.display(),p.display()),expr.span.clone());
                            self.related(
                                origin.clone(),
                                if origin == &expr.span {
                                    "overlapping projection of this argument introduced here"
                                } else {
                                    "earlier overlapping argument evaluated here"
                                },
                            );
                            if (param.pass == Pass::In || *mode == Pass::In)
                                && p.path.len() != old.path.len()
                                && !p.through_element()
                                && !old.through_element()
                            {
                                // A field of a lent receiver (§13): the
                                // shared borrow reaches every field.
                                let (whole, part) = if p.path.len() < old.path.len() {
                                    (p.display(), old.display())
                                } else {
                                    (old.display(), p.display())
                                };
                                if let Some(d) = self.diagnostics.last_mut() {
                                    d.help = Some(format!(
                                        "An 'in' borrow of '{whole}' reaches every field, including '{part}', so the callee could read it while it is written. Pass the fields the callee reads instead of '{whole}'."
                                    ));
                                }
                            } else {
                                self.help("Overlapping argument places are allowed only when both arguments use shared in passing.");
                            }
                        }
                    }
                    loans.push((p, param.pass, expr.span.clone()));
                }
            }
            let usage = if param.pass == Pass::Sink {
                Usage::Own
            } else {
                Usage::Borrow(param.pass)
            };
            let mut context_vars = vars.clone();
            for g in &f.generics {
                context_vars.entry(g.name.clone()).or_insert(Type::Infer);
            }
            let mut cap_labels = BTreeMap::new();
            for (i, e) in f.effects.instances.iter().enumerate() {
                let mut e = e.clone();
                e.args = e
                    .args
                    .iter()
                    .map(|t| substitute(t, &context_vars))
                    .collect();
                cap_labels.insert(
                    function_capability(&f.name, i),
                    self.resolve_effect(&e).and_then(|e| e.label),
                );
            }
            let target = rename_capabilities(
                &substitute(&bind_signature_capabilities(&param.ty, f), &context_vars),
                &cap_labels,
            );
            // A closure literal for a lent function parameter is a lent
            // closure (§4.2); any other argument must be a copy function
            // value or a lent function parameter passed on.
            let lending = param.pass == Pass::In
                && param.projections.is_empty()
                && matches!(&target, Type::Function { mode, .. } if *mode != Mode::Copy);
            let lent_closure = lending && matches!(expr.kind, ExprKind::Closure { .. });
            if lent_closure {
                self.lent_closures.insert((expr.span.start, expr.span.end));
            }
            let captures_base = self.lent_captures.len();
            let mut actual = if !param.projections.is_empty() {
                if let Some(p) = &place {
                    for path in &param.projections {
                        let mut q = p.clone();
                        q.path.extend(path.clone());
                        self.place(&q, env, usage, expr.span.clone());
                    }
                    self.peek_type(expr, env)
                } else {
                    self.expr_expected(expr, env, usage, &target)
                }
            } else {
                if param.pass == Pass::Inout {
                    self.expr(expr, env, usage)
                } else if crate::strings::is_str_type(&target) {
                    // A String argument is coerced to its view below.
                    self.expr(expr, env, usage)
                } else {
                    self.expr_expected(expr, env, usage, &target)
                }
            };
            if lent_closure {
                self.lent_closures.remove(&(expr.span.start, expr.span.end));
                let captured = self.lent_captures.split_off(captures_base);
                for (p, origin) in captured {
                    for (old, mode, old_origin) in &loans {
                        // Unreserved loans were checked when the closure
                        // borrowed the name; a reserved (two-phase) `inout`
                        // loan stays active for the whole call.
                        let reserved = *mode == Pass::Inout
                            && self
                                .reserved_loans
                                .contains(&(old_origin.start, old_origin.end));
                        if p.overlaps(old) && reserved {
                            self.error(
                                "E0301",
                                format!(
                                    "lent closure borrows '{}', which overlaps the '{}' argument loan",
                                    p.display(),
                                    old.display()
                                ),
                                origin.clone(),
                            );
                            self.related(old_origin.clone(), "overlapping argument passed here");
                            self.help("A lent closure borrows the names it uses `in` for the whole call, so they cannot also be lent `inout` or moved by it.");
                        }
                    }
                    loans.push((p, Pass::In, origin));
                }
                if let Type::Function { mode, .. } = &target
                    && let Type::Function { mode: m, .. } = &mut actual
                {
                    *m = *mode;
                }
            } else if lending
                && matches!(&actual, Type::Function { mode, .. } if *mode != Mode::Copy)
                && !matches!(&expr.kind, ExprKind::Name(n)
                    if env.bindings.get(n).is_some_and(|b| b.pass != Pass::Sink))
            {
                self.error(
                    "E0310",
                    "a function parameter that is not `copy` is lent: pass a closure literal, a `copy` function or a lent function parameter",
                    expr.span.clone(),
                );
            }
            // A lent closure owns nothing (§4.2): its mode is the
            // parameter's, so it is no linear temporary.
            if param.pass != Pass::Sink
                && place.is_none()
                && !lent_closure
                && self.mode(&actual) == Mode::Linear
            {
                self.error(
                    "E0302",
                    "bind a temporary linear value before borrowing it, then consume it explicitly",
                    expr.span.clone(),
                );
            }
            self.loans.truncate(loan_base);
            self.loans.extend(
                loans
                    .iter()
                    .filter(|(_, mode, _)| *mode != Pass::Sink)
                    .cloned(),
            );
            if param.pass == Pass::Inout {
                if let Some(place) = &place
                    && let Some(binding) = env.bindings.get_mut(&place.root)
                {
                    binding.mutation = Arc::new(());
                    binding.integer = None;
                    binding.nonzero = false;
                }
                if bound_capabilities(&actual) != bound_capabilities(&target)
                    || !compatible(&actual, &target)
                {
                    self.error(
                        "E0403",
                        "inout arguments must preserve the place's capability scope and value type",
                        expr.span.clone(),
                    );
                }
            } else {
                actual = instantiate_latent_effects(&actual, &target);
            }
            // A String passes to an `in` Str parameter as its whole view.
            if param.pass == Pass::In
                && crate::strings::is_str_type(&target)
                && crate::strings::is_string_type(&actual)
            {
                actual = target.clone();
            }
            infer(&param.ty, &actual, &f.generics, vars);
            for (row, fixed) in &fixed_rows {
                if let Some(Type::Row(inferred)) = vars.get(row) {
                    let closed = Effects {
                        instances: fixed.instances.clone(),
                        tail: None,
                    };
                    if !effects_include(
                        &closed,
                        &Effects {
                            instances: inferred.instances.clone(),
                            tail: None,
                        },
                    ) || inferred.tail.is_some() && inferred.tail != fixed.tail
                    {
                        self.error(
                            "E0406",
                            "argument effects exceed the explicitly supplied effect row",
                            expr.span.clone(),
                        );
                    }
                }
                vars.insert(row.clone(), Type::Row(fixed.clone()));
            }
            constraints.push((expr, param, actual.clone()));
            self.expect_type(
                &rename_capabilities(
                    &substitute(&bind_signature_capabilities(&param.ty, f), vars),
                    &cap_labels,
                ),
                &actual,
                expr.span.clone(),
            );
            if let (
                Type::Function {
                    effects: expected, ..
                },
                Type::Function {
                    effects: actual, ..
                },
            ) = (&target, &actual)
                && expected.tail.is_none()
            {
                for e in &actual.instances {
                    if !expected
                        .instances
                        .iter()
                        .any(|x| x.name == e.name && (x.label.is_none() || x.label == e.label))
                    {
                        self.error(
                            "E0401",
                            "function argument has unpermitted effects",
                            expr.span.clone(),
                        );
                    }
                }
            }
        }
        self.loans.truncate(loan_base);
        // A unique argument may contain a suspended continuation. Resuming it
        // can change the caller places aliased by this frame's borrowed inputs.
        if constraints
            .iter()
            .any(|(_, _, ty)| self.mode(ty) != Mode::Copy)
        {
            env.invalidate_borrowed_facts();
        }
        for ty in vars.values_mut() {
            *ty = crate::numeric::default_type(ty);
        }
        for (expr, param, actual) in constraints {
            let target = substitute(&param.ty, vars);
            let typed_target = bind_signature_capabilities(&target, f);
            let mut allowed_caps = bound_capabilities(&typed_target);
            // Region arguments of parameter types (`RegionBox[r, T]`) name the
            // callee's capabilities; they are instantiated like its row.
            let mut region_labels = BTreeMap::new();
            // Function-row binders are instantiated to the caller's evidence.
            for (index, effect) in f.effects.instances.iter().enumerate() {
                allowed_caps.remove(&function_capability(&f.name, index));
                let resolved = self.resolve_effect(effect).and_then(|e| e.label);
                if let Some(label) = &resolved {
                    allowed_caps.insert(label.clone());
                }
                region_labels.insert(function_capability(&f.name, index), resolved);
            }
            let target = rename_capabilities(&target, &region_labels);
            if !bound_capabilities(&actual).is_subset(&allowed_caps) {
                self.error("E0403","this parameter would erase a captured lexical capability; preserve it with an effect row or generic value type",expr.span.clone());
            }
            self.expect_type(&target, &actual, expr.span.clone());
            self.constrain_literals(expr, &target);
        }
        self.calls.insert((span.start, span.end), vars.clone());
        self.generic_bounds(&f.generics, vars, span);
        substitute(&f.result, vars)
    }
    fn constrain_literals(&mut self, e: &Expr, target: &Type) {
        let key = (e.span.start, e.span.end);
        if let Some(actual) = self.types.get(&key).cloned() {
            if matches!(actual, Type::IntLiteral(_))
                && crate::numeric::IntegerKind::from_type(target).is_some()
            {
                self.types.insert(key, target.clone());
            } else if matches!(actual, Type::Named(..) | Type::Tuple(..))
                && compatible(target, &actual)
            {
                self.types.insert(key, refine_annotation(target, &actual));
            }
        }
        match &e.kind {
            ExprKind::Tuple(es) => {
                if let Type::Tuple(ts) = target {
                    for (e, t) in es.iter().zip(ts) {
                        self.constrain_literals(e, t);
                    }
                }
            }
            ExprKind::Struct { name, fields } => {
                if let Some((a, v)) = self.world.constructor(name) {
                    let vars = if let Type::Named(_, ts) = target {
                        a.generics
                            .iter()
                            .zip(ts)
                            .map(|(g, t)| (g.name.clone(), t.clone()))
                            .collect()
                    } else {
                        BTreeMap::new()
                    };
                    for (n, e) in fields {
                        if let Some(f) = v.fields.iter().find(|f| &f.name == n) {
                            self.constrain_literals(e, &substitute(&f.ty, &vars));
                        }
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                if let ExprKind::Name(n) = &callee.kind {
                    if crate::fip::active(self.world)
                        && matches!(
                            n.as_str(),
                            crate::fip::BOX_NEW | crate::fip::BOX_NEW_IN_CALLER
                        )
                        && let Type::Named(_, ts) = target
                        && let (Some(arg), Some(inner_t)) = (args.first(), ts.first())
                    {
                        self.constrain_literals(&arg.expr, inner_t);
                    } else if let Some((a, v)) = self.world.constructor(n) {
                        let vars = if let Type::Named(_, ts) = target {
                            a.generics
                                .iter()
                                .zip(ts)
                                .map(|(g, t)| (g.name.clone(), t.clone()))
                                .collect()
                        } else {
                            BTreeMap::new()
                        };
                        for (a, f) in args.iter().zip(&v.fields) {
                            self.constrain_literals(&a.expr, &substitute(&f.ty, &vars));
                        }
                    }
                }
            }
            ExprKind::Block { tail: Some(t), .. } => self.constrain_literals(t, target),
            ExprKind::If {
                then, otherwise, ..
            } => {
                self.constrain_literals(then, target);
                if let Some(e) = otherwise {
                    self.constrain_literals(e, target);
                }
            }
            ExprKind::Match { arms, .. } => {
                for (_, e) in arms {
                    self.constrain_literals(e, target);
                }
            }
            _ => {}
        }
    }
    fn peek_type(&self, e: &Expr, env: &Env) -> Type {
        if let Some(p) = Place::from_expr(e) {
            env.bindings
                .get(&p.root)
                .and_then(|b| self.world.field(&b.ty, &p.path))
                .unwrap_or(Type::Infer)
        } else {
            self.types
                .get(&(e.span.start, e.span.end))
                .cloned()
                .unwrap_or(Type::Infer)
        }
    }
    fn active_effect(&self, effect: &Effect) -> Option<Effect> {
        let request = required_label(&effect.label);
        let label = request
            .as_ref()
            .map(|l| self.cap_aliases.get(l).unwrap_or(l));
        let mut candidates =
            self.caps.iter().rev().filter(|e| {
                e.name == effect.name && (label.is_none() || e.label.as_ref() == label)
            });
        // Typed instance selection: an unlabeled request with concrete
        // arguments prefers the innermost capability with exactly those
        // arguments. Every backend mirrors this with the same instance key.
        if label.is_none()
            && let Some(key) = crate::numeric::instance_key(&effect.name, &effect.args)
            && let Some(cap) = candidates
                .clone()
                .find(|c| crate::numeric::instance_key(&c.name, &c.args).as_ref() == Some(&key))
        {
            return Some(cap.clone());
        }
        candidates.next().cloned()
    }
    fn resolve_effect(&self, effect: &Effect) -> Option<Effect> {
        if let Some(cap) = self.active_effect(effect) {
            return Some(cap);
        }
        let label = required_label(&effect.label);
        if label.as_ref().is_some_and(|l| l.starts_with('$')) {
            return None;
        }
        if let Some((infer, _, _, prefix)) = self.effect_inference.last() {
            let allowed = self.allowed.instances.iter().any(|e| {
                e.name == effect.name
                    && required_label(&e.label) == label
                    && e.args.len() == effect.args.len()
                    && e.args
                        .iter()
                        .zip(&effect.args)
                        .all(|(a, b)| compatible(a, b))
            });
            if *infer || allowed {
                let mut e = effect.clone();
                e.label = label;
                e.label = Some(latent_capability(prefix, &e));
                return Some(e);
            }
        }
        None
    }
    fn observe(&mut self, row: &Effects) {
        let depth = self.effect_inference.len();
        if let Some((boundary, observed)) = self.observations.last_mut()
            && *boundary == depth
        {
            union_effects(observed, row);
        }
    }
    fn start_observing(&mut self) {
        self.observations
            .push((self.effect_inference.len(), Effects::default()));
    }
    fn finish_observing(&mut self) -> Effects {
        self.observations.pop().unwrap().1
    }
    fn require_effect(&mut self, effect: &Effect, env: &Env, span: Span) {
        // Built-in capability effects constrain callees, not callers: they are
        // never requested from the caller's row (see `capability_effects`).
        if self.dead != 0 || crate::capability_effects::is_capability_effect(&effect.name) {
            return;
        }
        let resolved = self.resolve_effect(effect);
        self.observe(&Effects {
            instances: vec![resolved.clone().unwrap_or_else(|| effect.clone())],
            tail: None,
        });
        let cap_index = self.caps.iter().rposition(|c| Some(c) == resolved.as_ref());
        let infer = self
            .effect_inference
            .last()
            .is_some_and(|(infer, _, _, _)| *infer);
        if let Some((_, boundary, observed, _)) = self.effect_inference.last_mut()
            && cap_index.is_none_or(|i| i < *boundary)
        {
            let effect = resolved.clone().unwrap_or_else(|| effect.clone());
            if !observed.instances.contains(&effect) {
                observed.instances.push(effect);
            }
        }
        if let Some(cap) = &resolved
            && (cap.args.len() != effect.args.len()
                || cap.args.iter().zip(&effect.args).any(|(a, b)| {
                    !compatible(a, b) || bound_capabilities(a) != bound_capabilities(b)
                }))
        {
            self.error(
                "E0400",
                format!(
                    "effect '{}' type arguments do not match its lexical capability",
                    effect.name
                ),
                span.clone(),
            );
        }
        if !infer
            && resolved.is_none()
            && !self.allowed.instances.iter().any(|e| {
                e.name == effect.name
                    && e.label == effect.label
                    && e.args.len() == effect.args.len()
                    && e.args
                        .iter()
                        .zip(&effect.args)
                        .all(|(a, b)| compatible(a, b))
            })
        {
            self.error("E0401",format!("unhandled effect '{}'{}; add it to the effect row or install a lexical handler",effect.name,effect.label.as_ref().map(|l|format!(" at '{l}'")).unwrap_or_default()),span.clone());
        }
        if self
            .world
            .effects
            .get(&effect.name)
            .is_some_and(|decl| !decl.linear)
        {
            self.check_abort_frame_for(env, &effect.name, span, cap_index);
        }
    }
    fn check_abort_frame(&mut self, env: &Env, effect: &str, span: Span) {
        self.check_abort_frame_for(env, effect, span, None);
    }
    fn check_abort_frame_for(
        &mut self,
        env: &Env,
        effect: &str,
        span: Span,
        target: Option<usize>,
    ) {
        for (index, depth, state, origin) in self.state_guards.clone() {
            if depth == self.effect_inference.len() && target.is_none_or(|target| index > target) {
                self.error(
                    "E0402",
                    format!("abortable effect '{effect}' may discard linear handler state {state}"),
                    span.clone(),
                );
                self.related(origin, "linear handler state initializer");
                self.help("An abortable checkpoint may discard the suspended continuation and its owned linear state.");
            }
        }
        // Keep-writes (manuscript §4.3, `PostCall_abort`): an abortable effect
        // may cross a stationary `inout` loan or parameter. The borrowed place
        // stays in the caller and holds the last value written before the
        // abort. A sibling argument that aborts before the callee is entered
        // (`f(inout x, g()?)`) has written nothing yet. What stays rejected is
        // a borrowed place with an open move-out hole: unwinding would hand
        // the caller a torn value. Every `inout` in this checker is
        // stationary (no `~>` typestate transitions exist yet), so the hole
        // is the only condition left to check here.
        let torn = self
            .loans
            .iter()
            .filter(|(place, mode, _)| {
                *mode == Pass::Inout
                    && env.bindings.get(&place.root).is_some_and(|b| {
                        b.moved
                            .iter()
                            .any(|m| m.starts_with(&place.path) || place.path.starts_with(m))
                    })
            })
            .take(8)
            .map(|(_, _, origin)| origin.clone())
            .collect::<Vec<_>>();
        if !torn.is_empty() {
            self.error(
                "E0402",
                "abortable effect crosses an inout argument loan with a moved-out part",
                span.clone(),
            );
            for origin in torn {
                self.related(origin, "inout argument access requested here");
            }
            self.help("Reinitialize the moved part before an abortable checkpoint: an abort keeps the borrowed place, so it must be whole.");
        }
        // An abort caught by a lexical handler in this frame does not unwind
        // bindings declared outside that handler: control continues after
        // the `handle` expression with them still in scope. The handler
        // checks afterwards that the body did not consume them.
        let catcher = target.and_then(|t| self.handler_frames.iter().rposition(|h| h.cap == t));
        for (n, b) in &env.bindings {
            // An abort skips the drop of an affine `.vsys` handle with a
            // drop import (§16.3), so such a handle is kept off abortable
            // checkpoints as a linear value is.
            let droppable = b.pass == Pass::Sink
                && !b.moved.iter().any(Vec::is_empty)
                && self.droppable_remaining(&b.ty, &mut vec![], &b.moved);
            if b.pass == Pass::Sink
                && (self.mode(&b.ty) == Mode::Linear || droppable)
                && !b.moved.iter().any(Vec::is_empty)
                && let Some(frame) = catcher
                && self.handler_frames[frame].outer.get(n) == Some(&b.span)
            {
                self.handler_frames[frame]
                    .kept
                    .entry(n.clone())
                    .or_insert_with(|| span.clone());
                continue;
            }
            if b.pass == Pass::Sink
                && self.mode(&b.ty) == Mode::Linear
                && !b.moved.iter().any(Vec::is_empty)
            {
                self.error("E0402",format!("potentially abortable effect '{effect}' may discard live linear value '{n}'"),span.clone());
                self.related(b.span.clone(), "live linear binding introduced here");
                self.help("An abortable checkpoint may discard a continuation that still owns this linear binding.");
            }
            if droppable && self.mode(&b.ty) != Mode::Linear {
                self.error("E0402",format!("potentially abortable effect '{effect}' may skip the drop import of live handle '{n}'"),span.clone());
                self.related(b.span.clone(), "live handle binding introduced here");
                self.help("A handle whose type declares a drop import is released when it is dropped; an abort would skip that call. Finalize or move the handle before the checkpoint, or catch the abort in this function.");
            }
            if b.pass == Pass::Inout && !b.moved.is_empty() {
                self.error("E0402",format!("potentially abortable effect '{effect}' crosses inout parameter '{n}' while part of it is moved out"),span.clone());
                self.related(b.span.clone(), "inout parameter declared here");
                self.help(
                    "An abort keeps the caller's place (keep-writes), so every inout parameter must be whole at an abortable checkpoint.",
                );
            }
        }
    }
    fn merge(&mut self, env: &mut Env, a: &Env, b: &Env, span: Span) {
        for (n, binding) in &mut env.bindings {
            if let (Some(x), Some(y)) = (a.bindings.get(n), b.bindings.get(n)) {
                if self.mode(&binding.ty) == Mode::Linear && x.moved != y.moved {
                    self.error(
                        "E0309",
                        format!("linear value '{n}' is consumed on only some control-flow paths"),
                        span.clone(),
                    );
                    self.related(binding.span.clone(), "linear binding introduced here");
                    for branch in [x, y] {
                        if branch.span == binding.span {
                            for cause in &branch.move_causes {
                                // Incoming history is shared by both branches;
                                // it is not a branch-specific transfer witness.
                                if !binding.move_causes.iter().any(|before| {
                                    before.path == cause.path && before.span == cause.span
                                }) {
                                    self.related(
                                        cause.span.clone(),
                                        "ownership-taking use on one control-flow path",
                                    );
                                }
                            }
                        }
                    }
                    self.help(
                        "Returning control-flow paths must agree on linear ownership state; borrowed parameters do not transfer ownership.",
                    );
                }
                binding.moved = x.moved.iter().chain(&y.moved).cloned().collect();
                binding.moved.sort();
                binding.moved.dedup();
                binding.move_causes.clear();
                for branch in [x, y] {
                    if branch.span == binding.span {
                        for cause in &branch.move_causes {
                            binding.remember_move(&cause.path, cause.span.clone());
                        }
                    }
                }
                binding.mutation = if x.span == y.span && Arc::ptr_eq(&x.mutation, &y.mutation) {
                    Arc::clone(&x.mutation)
                } else {
                    Arc::new(())
                };
                // Each branch's bounds hold where it ends, so their hull
                // holds after the join, under the joined identity.
                let looped = Self::loop_range(x) && Self::loop_range(y);
                binding.range = (x.span == y.span)
                    .then(|| Self::binding_bounds(x).zip(Self::binding_bounds(y)))
                    .flatten()
                    .map(|((a, b), (c, d))| IntRange {
                        lo: a.min(c),
                        hi: b.max(d),
                        at: Arc::clone(&binding.mutation),
                        looped,
                    });
                binding.integer = x.integer.filter(|n| Some(*n) == y.integer);
                binding.nonzero = x.nonzero && y.nonzero;
            } else {
                // A missing branch binding cannot certify an unchanged value.
                binding.mutation = Arc::new(());
            }
        }
    }
    fn primitive_failure(&mut self, env: &Env, span: Span) {
        if self.dead != 0 {
            return;
        }
        self.partial_ops.insert((span.start, span.end));
        self.require_effect(
            &Effect {
                label: None,
                name: "Fail".into(),
                args: vec![Type::named("RuntimeError")],
            },
            env,
            span,
        );
    }
    /// Record a checked operation that may raise Fail[ArithOverflow].
    fn overflow_failure(&mut self, env: &Env, span: Span) {
        if self.dead != 0 {
            return;
        }
        self.overflow_ops.insert((span.start, span.end));
        self.require_effect(
            &Effect {
                label: None,
                name: "Fail".into(),
                args: vec![Type::named(crate::numeric::OVERFLOW_TYPE)],
            },
            env,
            span,
        );
    }
    /// Constant facts which prove a checked operation cannot overflow. A
    /// known divisor other than -1 (or dividend other than MIN) also proves
    /// signed division in range; zero divisors are RuntimeError's concern.
    fn in_range(
        &self,
        op: &str,
        kind: crate::numeric::IntegerKind,
        a: Option<i128>,
        b: Option<i128>,
    ) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => {
                integer_widths_rules::fact_checked(kind, op, a, b)
                    != Err(crate::numeric::ArithFailure::Overflow)
            }
            _ if op == "/" => b.is_some_and(|b| b != -1) || a.is_some_and(|a| a != kind.min()),
            _ => false,
        }
    }
    /// Bounds which prove `+`, `-` or `*` in range: every corner of the
    /// operands' bounds fits the kind, such as `i + 1` for a `for i in 0..n`
    /// index with a constant `n` (#1091). The caller applies it only where
    /// an operand is a loop index: the Lean reference checker of
    /// tests/lockstep.rs has no loops, so loop-free programs keep its
    /// exact-fact overflow rule.
    fn bounds_in_range(
        op: &str,
        kind: crate::numeric::IntegerKind,
        a: Option<(i128, i128)>,
        b: Option<(i128, i128)>,
    ) -> bool {
        let (Some((alo, ahi)), Some((blo, bhi))) = (a, b) else {
            return false;
        };
        let f = match op {
            "+" => i128::checked_add,
            "-" => i128::checked_sub,
            "*" => i128::checked_mul,
            _ => return false,
        };
        [(alo, blo), (alo, bhi), (ahi, blo), (ahi, bhi)]
            .into_iter()
            .all(|(x, y)| f(x, y).is_some_and(|v| v >= kind.min() && v <= kind.max()))
    }
    /// Typecheck `wrapping_add`, `wrapping_sub` or `wrapping_mul`: two
    /// by-value operands of one integer kind, with contextual literal typing.
    fn wrapping_call(
        &mut self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        if !types.is_empty() {
            self.error(
                "E0207",
                format!("'{name}' does not take type arguments"),
                span.clone(),
            );
        }
        if args.len() != 2 {
            self.error(
                "E0207",
                format!("'{name}' expects 2 arguments, found {}", args.len()),
                span.clone(),
            );
        }
        for a in args {
            if a.pass != Pass::In || a.explicit {
                self.error(
                    "E0308",
                    format!("'{name}' takes its operands by value"),
                    a.expr.span.clone(),
                );
            }
        }
        let goal = if crate::numeric::IntegerKind::from_type(expected).is_some()
            && !matches!(expected, Type::IntLiteral(_))
        {
            expected.clone()
        } else {
            Type::Infer
        };
        let mut common = goal.clone();
        let mut actual = vec![];
        for a in args {
            let goal = if matches!(common, Type::IntLiteral(_)) {
                Type::Infer
            } else {
                common.clone()
            };
            let t = self.expr_expected(&a.expr, env, Usage::Own, &goal);
            if matches!(common, Type::Infer | Type::IntLiteral(_)) {
                common = match (&common, &t) {
                    (Type::IntLiteral(x), Type::IntLiteral(y)) => Type::IntLiteral((*x).max(*y)),
                    _ => t.clone(),
                };
            }
            actual.push(t);
        }
        let common = crate::numeric::default_type(&common);
        if crate::numeric::IntegerKind::from_type(&common).is_none() {
            self.error(
                "E0200",
                format!("'{name}' requires integer operands of one kind, found {common}"),
                span.clone(),
            );
            return Type::Infer;
        }
        for (a, t) in args.iter().zip(&actual) {
            self.expect_type(&common, t, a.expr.span.clone());
            if matches!(t, Type::IntLiteral(_)) {
                self.types
                    .insert((a.expr.span.start, a.expr.span.end), common.clone());
            }
        }
        common
    }
    fn nonzero(&self, e: &Expr, env: &Env) -> bool {
        self.integer_fact(e, env).is_some_and(|n| n != 0)
            || matches!(&e.kind, ExprKind::Name(n) if env.bindings.get(n).is_some_and(|b| b.nonzero))
            || self
                .integer_bounds(e, env)
                .is_some_and(|(lo, hi)| lo > 0 || hi < 0)
    }
    /// The exact value of a pure integer expression, under checked semantics:
    /// an operation that would overflow or fail has no fact.
    fn integer_fact(&self, e: &Expr, env: &Env) -> Option<i128> {
        let kind = |e: &Expr| {
            self.types
                .get(&(e.span.start, e.span.end))
                .and_then(crate::numeric::IntegerKind::from_type)
        };
        let n = match &e.kind {
            ExprKind::Literal(Literal::Int(n)) => *n,
            ExprKind::Name(n) => env.bindings.get(n)?.integer?,
            ExprKind::Unary(op, v) if op == "-" => {
                let k = kind(v).or_else(|| kind(e))?;
                integer_widths_rules::fact_neg(k, self.integer_fact(v, env)?)?
            }
            ExprKind::Binary(op, a, b) => {
                let k = kind(a).or_else(|| kind(e))?;
                let (a, b) = (self.integer_fact(a, env)?, self.integer_fact(b, env)?);
                integer_widths_rules::fact_checked(k, op, a, b).ok()?
            }
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Name(name) = &callee.kind else {
                    return None;
                };
                // A module constant whose initializer is an integer literal
                // (manuscript §2.2) has that value.
                if args.is_empty()
                    && crate::arrays::is_const_function(name)
                    && !env.bindings.contains_key(name)
                {
                    let mut body = self.world.functions.get(name)?.body.as_ref()?;
                    while let ExprKind::Block {
                        statements,
                        tail: Some(tail),
                    } = &body.kind
                        && statements.is_empty()
                    {
                        body = tail;
                    }
                    return match body.kind {
                        ExprKind::Literal(Literal::Int(n)) => Some(n),
                        _ => None,
                    };
                }
                let op = crate::numeric::wrapping_intrinsic(name)?;
                if env.bindings.contains_key(name) || args.len() != 2 {
                    return None;
                }
                let (a, b) = (
                    self.integer_fact(&args[0].expr, env)?,
                    self.integer_fact(&args[1].expr, env)?,
                );
                kind(e)?.wrapping(op, a, b)
            }
            _ => return None,
        };
        match kind(e) {
            Some(k) if !integer_widths_rules::fact_fits(k, n) => None,
            _ => Some(n),
        }
    }
    fn refine_condition(&self, e: &Expr, env: &mut Env, truth: bool) {
        self.refine_bounds(e, env, truth);
        if let ExprKind::Binary(op, a, b) = &e.kind {
            let n = match (&a.kind, &b.kind) {
                (ExprKind::Name(n), ExprKind::Literal(Literal::Int(0)))
                | (ExprKind::Literal(Literal::Int(0)), ExprKind::Name(n)) => n,
                _ => return,
            };
            if let Some(binding) = env.bindings.get_mut(n) {
                match (op.as_str(), truth) {
                    ("!=", true) | ("==", false) => binding.nonzero = true,
                    ("==", true) | ("!=", false) => {
                        binding.integer = Some(0);
                        binding.nonzero = false;
                    }
                    _ => (),
                }
            }
        }
    }
    /// Narrow the bounds of the integer names a branch condition compares,
    /// on the side where the condition is `truth`. Only a condition built
    /// from comparisons of pure operands, `&&`, `||` and `!` refines: no part
    /// of it can write a name another part read.
    fn refine_bounds(&self, e: &Expr, env: &mut Env, truth: bool) {
        if Self::pure_condition(e) {
            self.refine_pure(e, env, truth);
        }
    }
    fn pure_condition(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Literal(Literal::Bool(_)) => true,
            ExprKind::Unary(op, v) if op == "!" => Self::pure_condition(v),
            ExprKind::Binary(op, a, b) if matches!(op.as_str(), "&&" | "||") => {
                Self::pure_condition(a) && Self::pure_condition(b)
            }
            ExprKind::Binary(op, a, b) if crate::strings::is_comparison(op) => {
                Self::pure_integer(a) && Self::pure_integer(b)
            }
            _ => false,
        }
    }
    /// An integer expression that cannot write a name.
    fn pure_integer(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Literal(Literal::Int(_)) | ExprKind::Name(_) => true,
            ExprKind::Unary(op, v) if matches!(op.as_str(), "-" | "~") => Self::pure_integer(v),
            ExprKind::Binary(_, a, b) => Self::pure_integer(a) && Self::pure_integer(b),
            _ => false,
        }
    }
    fn refine_pure(&self, e: &Expr, env: &mut Env, truth: bool) {
        let ExprKind::Binary(op, a, b) = &e.kind else {
            if let ExprKind::Unary(op, v) = &e.kind
                && op == "!"
            {
                self.refine_pure(v, env, !truth);
            }
            return;
        };
        match (op.as_str(), truth) {
            ("&&", true) | ("||", false) => {
                self.refine_pure(a, env, truth);
                self.refine_pure(b, env, truth);
                return;
            }
            ("&&" | "||", _) => return,
            _ => (),
        }
        // `a op b` holds; normalize to `name op' bound`.
        let op = if truth {
            op.as_str()
        } else {
            match op.as_str() {
                "<" => ">=",
                "<=" => ">",
                ">" => "<=",
                ">=" => "<",
                "==" => "!=",
                "!=" => "==",
                _ => return,
            }
        };
        let flipped = match op {
            "<" => ">",
            "<=" => ">=",
            ">" => "<",
            ">=" => "<=",
            other => other,
        };
        let (left, right) = (self.integer_bounds(a, env), self.integer_bounds(b, env));
        for (name, op, other) in [(a, op, right), (b, flipped, left)] {
            let (ExprKind::Name(n), Some((olo, ohi))) = (&name.kind, other) else {
                continue;
            };
            let Some((mut lo, mut hi)) = self.integer_bounds(name, env) else {
                continue;
            };
            match op {
                "<" => hi = hi.min(ohi.saturating_sub(1)),
                "<=" => hi = hi.min(ohi),
                ">" => lo = lo.max(olo.saturating_add(1)),
                ">=" => lo = lo.max(olo),
                "==" => (lo, hi) = (lo.max(olo), hi.min(ohi)),
                "!=" if olo == ohi => {
                    if lo == olo {
                        lo = lo.saturating_add(1);
                    }
                    if hi == olo {
                        hi = hi.saturating_sub(1);
                    }
                }
                _ => continue,
            }
            if let Some(binding) = env.bindings.get_mut(n) {
                let looped = Self::loop_range(binding);
                binding.range = Some(IntRange {
                    lo,
                    hi,
                    at: Arc::clone(&binding.mutation),
                    looped,
                });
            }
        }
    }
    /// The bounds a binding's range and kind give it, while the range is
    /// current.
    /// Whether `b` holds a `for` loop index's current range.
    fn loop_range(b: &Binding) -> bool {
        b.range
            .as_ref()
            .is_some_and(|r| r.looped && Arc::ptr_eq(&r.at, &b.mutation))
    }
    /// Whether `e` names a `for` loop index in its range.
    fn loop_index(e: &Expr, env: &Env) -> bool {
        matches!(&e.kind, ExprKind::Name(n) if env.bindings.get(n).is_some_and(Self::loop_range))
    }
    fn binding_bounds(b: &Binding) -> Option<(i128, i128)> {
        let (mut lo, mut hi) = match &b.ty {
            Type::IntLiteral(_) => (i128::MIN, i128::MAX),
            t => {
                let k = crate::numeric::IntegerKind::from_type(t)?;
                // `u128` values pass the exact domain's `i128::MAX`: no
                // bounds (SH2 lane 2, #1174).
                if k == crate::numeric::IntegerKind::U128 {
                    return None;
                }
                (k.min(), k.max())
            }
        };
        if let Some(r) = b.range.as_ref().filter(|r| Arc::ptr_eq(&r.at, &b.mutation)) {
            lo = lo.max(r.lo);
            hi = hi.min(r.hi);
        }
        Some((lo, hi))
    }
    /// Inclusive bounds of an integer expression: its exact fact, a name's
    /// kind and current range, `x & m` with `m >= 0` in `0..=m`, a
    /// nonnegative `x % m` with `m > 0` in `0..m`, and a call's result range.
    fn integer_bounds(&self, e: &Expr, env: &Env) -> Option<(i128, i128)> {
        if let Some(n) = self.integer_fact(e, env) {
            // A `u128` fact above `i128::MAX` is a bit pattern, not a bound.
            return (n >= 0 || !self.u128_expr(e, env)).then_some((n, n));
        }
        match &e.kind {
            ExprKind::Name(n) => Self::binding_bounds(env.bindings.get(n)?),
            ExprKind::Binary(op, a, b) if op == "&" && Self::pure_integer(e) => {
                let mask = |x: &Expr| {
                    self.integer_bounds(x, env)
                        .filter(|(lo, _)| *lo >= 0)
                        .map(|(_, hi)| hi)
                };
                let m = match (mask(a), mask(b)) {
                    (Some(x), Some(y)) => x.min(y),
                    (x, y) => x.or(y)?,
                };
                Some((0, m))
            }
            ExprKind::Binary(op, a, b) if op == "%" && Self::pure_integer(e) => {
                let (alo, ahi) = self.integer_bounds(a, env)?;
                let (blo, bhi) = self.integer_bounds(b, env)?;
                (alo >= 0 && blo > 0).then(|| (0, ahi.min(bhi - 1)))
            }
            // A call of a function whose returns are bounded ([`call_facts`]).
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Name(n) if !env.bindings.contains_key(n) => self.call_range(n)?,
                _ => None,
            },
            _ => None,
        }
    }
    /// Bounds which prove a signed quotient in range: a divisor that cannot
    /// be -1 or a dividend that cannot be the kind's minimum.
    fn quotient_in_range(
        op: &str,
        kind: crate::numeric::IntegerKind,
        a: Option<(i128, i128)>,
        b: Option<(i128, i128)>,
    ) -> bool {
        op == "/"
            && (b.is_some_and(|(lo, hi)| lo > -1 || hi < -1)
                || a.is_some_and(|(lo, _)| lo > kind.min()))
    }
    /// `Some(Pass::In)` when `value` is a place of an `in` or `inout`
    /// binding and its type is not copy: `match` then borrows the place
    /// instead of moving it, and binds its payloads as `in` borrows.
    fn borrowed_scrutinee(&self, value: &Expr, env: &Env) -> Option<Pass> {
        let p = Place::from_expr(value)?;
        let b = env.bindings.get(&p.root)?;
        if b.pass == Pass::Sink {
            return None;
        }
        let ty = self.world.field(&b.ty, &p.path)?;
        (self.mode(&ty) != Mode::Copy).then_some(Pass::In)
    }
    fn bind(&mut self, p: &Pattern, ty: &Type, env: &mut Env) {
        self.bind_mode(p, ty, env, Pass::Sink);
    }
    fn bind_mode(&mut self, p: &Pattern, ty: &Type, env: &mut Env, pass: Pass) {
        match &p.kind {
            PatternKind::Wild => {
                if pass == Pass::Sink && self.mode(ty) == Mode::Linear {
                    self.error(
                        "E0302",
                        "wildcard cannot discard a linear value",
                        p.span.clone(),
                    );
                }
            }
            PatternKind::Bind(n) => {
                if let Some(old) = env.bindings.get(n)
                    && old.pass == Pass::Sink
                    && self.mode(&old.ty) == Mode::Linear
                    && old.moved.is_empty()
                {
                    self.error(
                        "E0302",
                        format!("shadowing would discard linear value '{n}'"),
                        p.span.clone(),
                    );
                    self.related(old.span.clone(), "unconsumed linear binding being shadowed");
                    self.help(
                        "A new binding does not consume the linear value with the same name.",
                    );
                }
                env.bindings.insert(
                    n.clone(),
                    Binding {
                        ty: ty.clone(),
                        pass,
                        projections: vec![],
                        moved: vec![],
                        move_causes: vec![],
                        span: p.span.clone(),
                        mutation: Arc::new(()),
                        frozen: false,
                        integer: None,
                        nonzero: false,
                        range: None,
                    },
                );
            }
            PatternKind::Literal(l) => {
                if let Literal::String(s) = l {
                    self.string_capacity = self.string_capacity.max(s.len());
                }
                let lt = if matches!(l, Literal::Float(_))
                    && crate::numeric::FloatKind::from_type(ty)
                        == Some(crate::numeric::FloatKind::F32)
                {
                    ty.clone()
                } else {
                    literal_type(l)
                };
                self.expect_type(ty, &lt, p.span.clone());
            }
            PatternKind::Tuple(ps) => {
                if let Type::Tuple(ts) = ty {
                    if ps.len() != ts.len() {
                        self.error("E0211", "tuple pattern arity mismatch", p.span.clone());
                    }
                    for (p, t) in ps.iter().zip(ts) {
                        self.bind_mode(p, t, env, pass);
                    }
                } else {
                    self.error("E0211", "tuple pattern requires tuple type", p.span.clone());
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                if crate::dyn_trait::is_boxed(ty) {
                    self.error(
                        "E0631",
                        format!("cannot unpack the boxed object {ty}: its value's type is known only to its vtable; call its methods or lend it to a `dyn` parameter"),
                        p.span.clone(),
                    );
                    return;
                }
                let Some((a, v)) = self.world.constructor(name) else {
                    self.error(
                        "E0211",
                        format!("unknown pattern constructor '{name}'"),
                        p.span.clone(),
                    );
                    return;
                };
                let args = if let Type::Named(n, args) = ty {
                    if n != &a.name {
                        self.error(
                            "E0211",
                            format!("pattern '{name}' does not match {ty}"),
                            p.span.clone(),
                        );
                    }
                    args.clone()
                } else {
                    self.error(
                        "E0211",
                        "constructor pattern requires an algebraic data type",
                        p.span.clone(),
                    );
                    vec![]
                };
                let vars = a
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t))
                    .collect();
                let mut seen = BTreeSet::new();
                for (n, p) in fields {
                    if !seen.insert(n) {
                        self.error("E0211", "duplicate pattern field", p.span.clone());
                    }
                    if let Some(f) = v.fields.iter().find(|f| &f.name == n) {
                        self.bind_mode(p, &substitute(&f.ty, &vars), env, pass);
                    } else {
                        self.error("E0211", "unknown pattern field", p.span.clone());
                    }
                }
                if seen.len() != v.fields.len() {
                    self.error("E0211", "pattern must name every field", p.span.clone());
                }
            }
        }
    }
    fn irrefutable(&self, p: &Pattern, t: &Type) -> bool {
        matrix_covered(self.world, &[vec![p.clone()]], std::slice::from_ref(t), 0)
    }
    fn coverage(&mut self, arms: &[(Pattern, Expr)], t: &Type, span: Span) {
        let rows = arms
            .iter()
            .map(|(p, _)| vec![p.clone()])
            .collect::<Vec<_>>();
        if !matrix_covered(self.world, &rows, std::slice::from_ref(t), 0) {
            self.error(
                "E0212",
                "non-exhaustive match; cover every variant and payload or add a wildcard",
                span,
            );
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the closure typing judgment"
    )]
    fn closure(
        &mut self,
        params: &[Param],
        mode: Option<Mode>,
        result: &Type,
        effects: &Effects,
        infer_effects: bool,
        body: &Expr,
        env: &mut Env,
        span: Span,
    ) -> Type {
        self.valid_effects(effects, &self.generics.clone(), span.clone());
        let mut free = free_names(body);
        for p in params {
            free.remove(&p.name);
        }
        let mut captured = Env::default();
        let mut inferred = Mode::Copy;
        let lent = self.lent_closures.remove(&(span.start, span.end));
        if lent {
            self.lent_bodies
                .entry(crate::ir::nested_name("closure", &body.span))
                .or_default();
        }
        for name in free {
            if let Some(b) = env.bindings.get(&name).cloned() {
                if lent {
                    // A lent closure (§4.2) borrows what it uses `in` for
                    // the call it is lent to, as an `in` argument would:
                    // nothing is moved, a borrowed parameter may be used,
                    // and inside the body the name is read-only.
                    let place = Place {
                        root: name.clone(),
                        path: vec![],
                    };
                    self.place(&place, env, Usage::Borrow(Pass::In), span.clone());
                    self.lent_captures.push((place, span.clone()));
                    let mut inner = b;
                    if inner.pass != Pass::In {
                        inner.pass = Pass::In;
                        inner.projections = vec![];
                    }
                    self.lent_bodies
                        .entry(crate::ir::nested_name("closure", &body.span))
                        .or_default()
                        .insert(name.clone());
                    captured.bindings.insert(name, inner);
                    continue;
                }
                // A copy value lent `in` is captured as a copy (§4.2).
                if b.pass != Pass::Sink && !(b.pass == Pass::In && self.mode(&b.ty) == Mode::Copy) {
                    self.error(
                        "E0310",
                        format!("closure cannot capture borrowed parameter '{name}'"),
                        span.clone(),
                    );
                }
                inferred = inferred.join(self.mode(&b.ty));
                self.place(
                    &Place {
                        root: name.clone(),
                        path: vec![],
                    },
                    env,
                    Usage::Own,
                    span.clone(),
                );
                captured.bindings.insert(name, b);
            }
        }
        let mode = mode.unwrap_or(inferred);
        if inferred > mode {
            self.error(
                "E0310",
                "closure mode cannot weaken its captured values",
                span.clone(),
            );
        }
        let result = &self.region_annotation(result);
        let param_types = params
            .iter()
            .map(|p| self.region_annotation(&p.ty))
            .collect::<Vec<_>>();
        self.valid_type(result, &self.generics.clone(), span.clone());
        for (p, ty) in params.iter().zip(&param_types) {
            self.valid_type(ty, &self.generics.clone(), p.span.clone());
            if unresolved(ty) {
                self.error(
                    "E0102",
                    "closure parameters need a type annotation when no expected type is available",
                    p.span.clone(),
                );
            }
            captured.bindings.insert(
                p.name.clone(),
                Binding {
                    ty: ty.clone(),
                    pass: p.pass,
                    projections: p.projections.clone(),
                    moved: vec![],
                    move_causes: vec![],
                    span: p.span.clone(),
                    mutation: Arc::new(()),
                    frozen: false,
                    integer: None,
                    nonzero: false,
                    range: None,
                },
            );
        }
        let prefix = format!("c{}", span.start);
        let mut resolved_effects = effects.clone();
        for e in &mut resolved_effects.instances {
            e.label = self
                .active_effect(e)
                .and_then(|e| e.label)
                .or_else(|| Some(latent_capability(&prefix, e)));
        }
        let allowed = std::mem::replace(&mut self.allowed, effects.clone());
        let cap_base = self.caps.len();
        self.caps.extend(resolved_effects.instances.iter().cloned());
        self.effect_inference
            .push((infer_effects, cap_base, Effects::default(), prefix));
        let outer = self.enter_function(result);
        let actual = self.expr_expected(body, &mut captured, Usage::Own, result);
        let actual = self.leave_function(outer, actual, body.span.clone());
        let (_, _, observed, _) = self.effect_inference.pop().unwrap();
        self.caps.truncate(cap_base);
        if infer_effects {
            resolved_effects = observed;
        } else if !effects_include(&resolved_effects, &observed) {
            self.error(
                "E0401",
                "closure body uses a capability absent from its declared effect row",
                body.span.clone(),
            );
        }
        self.allowed = allowed;
        self.expect_type(result, &actual, body.span.clone());
        self.exhaust(&captured, captured.bindings.keys().cloned().collect(), span);
        Type::Function {
            params: params
                .iter()
                .zip(param_types)
                .map(|(p, ty)| (p.pass, ty))
                .collect(),
            result: Box::new(if *result == Type::Infer {
                crate::numeric::default_type(&actual)
            } else {
                refine_annotation(result, &actual)
            }),
            effects: resolved_effects,
            mode,
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the stateful handler typing judgment"
    )]
    fn handle(
        &mut self,
        effect: &Effect,
        state: &Option<(Pattern, Box<Expr>)>,
        body: &Expr,
        ret: &Clause,
        ops: &[Clause],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        self.valid_effects(
            &Effects {
                instances: vec![effect.clone()],
                tail: None,
            },
            &self.generics.clone(),
            span.clone(),
        );
        if crate::capability_effects::is_capability_effect(&effect.name) {
            self.diagnostics
                .push(crate::capability_effects::handler_rejected(effect, span));
            return Type::Infer;
        }
        let Ok(region) = self.region_block(effect, ret, span.clone()) else {
            return Type::Infer;
        };
        let Some(decl) = self.world.effects.get(&effect.name) else {
            self.error("E0400", "unknown handled effect", span);
            return Type::Infer;
        };
        if effect.args.len() != decl.generics.len() {
            self.error(
                "E0400",
                format!(
                    "handler for '{}' requires {} type arguments",
                    effect.name,
                    decl.generics.len()
                ),
                span.clone(),
            );
        }
        let state_type = state
            .as_ref()
            .map(|(_, e)| self.expr(e, env, Usage::Own))
            .unwrap_or_else(Type::unit);
        let outer = env.clone();
        let mut cap = effect.clone();
        cap.label = Some(format!("$cap{}", span.start));
        let aliases = self.cap_aliases.clone();
        if let Some(label) = &effect.label {
            self.cap_aliases
                .insert(label.clone(), cap.label.clone().unwrap());
        }
        self.caps.push(cap.clone());
        self.handler_frames.push(HandlerFrame {
            cap: self.caps.len() - 1,
            outer: env
                .bindings
                .iter()
                .map(|(n, b)| (n.clone(), b.span.clone()))
                .collect(),
            kept: BTreeMap::new(),
        });
        let guarded = self.mode(&state_type) == Mode::Linear;
        if guarded {
            self.state_guards.push((
                self.caps.len() - 1,
                self.effect_inference.len(),
                state_type.clone(),
                state
                    .as_ref()
                    .map_or_else(|| span.clone(), |(_, e)| e.span.clone()),
            ));
        }
        self.start_observing();
        self.enter_control(loop_rules::ControlFrame::HandledBody {
            region: region.is_some(),
        });
        let body_type = self.expr(body, env, Usage::Own);
        self.leave_control();
        let mut outgoing = self.finish_observing();
        outgoing.instances.retain(|e| e.label != cap.label);
        if guarded {
            self.state_guards.pop();
        }
        // The catch edge keeps an outer linear value as it was at the
        // checkpoint; the normal edge sees what the body did with it. They
        // agree only if the body left it as it found it.
        let frame = self.handler_frames.pop().expect("handler frame");
        for (n, checkpoint) in frame.kept {
            if let (Some(before), Some(after)) = (outer.bindings.get(&n), env.bindings.get(&n))
                && before.moved != after.moved
            {
                self.error(
                    "E0402",
                    format!(
                        "{} '{n}' is consumed inside a handled body that may abort before consuming it",
                        if self.mode(&before.ty) == Mode::Linear { "linear value" } else { "handle" }
                    ),
                    checkpoint,
                );
                self.related(before.span.clone(), "live linear binding introduced here");
                self.help("Consume the linear value after the handle expression, where the normal and the abort paths join.");
            }
        }
        self.caps.pop();
        self.cap_aliases = aliases;
        if contains_capability(&body_type, &cap) {
            if region.is_some() {
                self.diagnostics.push(
                    Diagnostic::new(
                        "E0791",
                        format!(
                            "a reference into region '{}' escapes its block as {}",
                            effect.label.as_deref().unwrap_or("r"),
                            crate::regions::display(&body_type)
                        ),
                        span.clone(),
                    )
                    .with_help("[T-RegionScope] requires r not to occur in the block's result type; copy the payload out with get or take"),
                );
            } else {
                self.error(
                    "E0403",
                    "lexical effect capability escapes its handler",
                    span.clone(),
                );
            }
        }
        // Handler clauses capture snapshots taken at handler entry, so their
        // facts stay valid. After an abortable handler, though, the code that
        // follows joins the normal edge with the catch edge, and an abort may
        // skip writes the body made after its checkpoint (or keep a write an
        // `inout` callee made before it): forget the body's facts about every
        // place the body wrote.
        if !decl.linear {
            for (n, b) in env.bindings.iter_mut() {
                if outer
                    .bindings
                    .get(n)
                    .is_some_and(|o| !Arc::ptr_eq(&o.mutation, &b.mutation))
                {
                    b.integer = None;
                    b.nonzero = false;
                    b.range = None;
                }
            }
        }
        let mut clause_env = Env::default();
        let mut free = clause_free_names(ret);
        for op in ops {
            free.extend(clause_free_names(op));
        }
        for (name, b) in &outer.bindings {
            if free.contains(name) {
                // A `copy` value read through an `in` borrow is stable for
                // the whole handler, so its snapshot is a plain copy
                // (§ handler state threading; SH5 lane 6, #1329).
                if self.mode(&b.ty) != Mode::Copy || !matches!(b.pass, Pass::Sink | Pass::In) {
                    self.error("E0404",format!("handler clause capture '{name}' must be owned copy; thread unique resources through state"),span.clone());
                }
                clause_env.bindings.insert(name.clone(), b.clone());
            }
        }
        let mut ret_env = clause_env.clone();
        let return_ty = if state.is_some() {
            Type::Tuple(vec![body_type, state_type.clone()])
        } else {
            body_type
        };
        if !self.irrefutable(&ret.pats[0], &return_ty) {
            self.error(
                "E0210",
                "handler return pattern must be irrefutable",
                ret.pats[0].span.clone(),
            );
        }
        self.bind(&ret.pats[0], &return_ty, &mut ret_env);
        self.start_observing();
        self.enter_control(loop_rules::ControlFrame::Clause);
        let returned = self.expr_expected(&ret.body, &mut ret_env, Usage::Own, expected);
        self.leave_control();
        union_effects(&mut outgoing, &self.finish_observing());
        let mut resume_effects = outgoing.clone();
        if let Some(previous) = self.world.handler_effects.get(&(span.start, span.end)) {
            union_effects(&mut resume_effects, previous);
        }
        let out = if returned.name() == "Never" && *expected != Type::Infer {
            expected.clone()
        } else {
            returned
        };
        self.exhaust(
            &ret_env,
            ret_env.bindings.keys().cloned().collect(),
            ret.body.span.clone(),
        );
        let vars = decl
            .generics
            .iter()
            .zip(&effect.args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        for op in ops {
            if !seen.insert(&op.name) {
                self.error("E0405", "duplicate handler clause", op.body.span.clone());
            }
            let Some(sig) = decl.ops.iter().find(|f| f.name == op.name) else {
                self.error(
                    "E0405",
                    format!("unknown operation '{}'", op.name),
                    op.body.span.clone(),
                );
                continue;
            };
            let mut local = clause_env.clone();
            let mut ts = sig
                .params
                .iter()
                .map(|p| substitute(&p.ty, &vars))
                .collect::<Vec<_>>();
            if state.is_some() {
                ts.push(state_type.clone());
            }
            let mut resume_params = vec![(Pass::Sink, substitute(&sig.result, &vars))];
            if state.is_some() {
                resume_params.push((Pass::Sink, state_type.clone()));
            }
            ts.push(Type::Function {
                params: resume_params,
                result: Box::new(out.clone()),
                effects: resume_effects.clone(),
                mode: if decl.linear {
                    Mode::Linear
                } else {
                    Mode::Affine
                },
            });
            if op.pats.len() != ts.len() {
                self.error("E0405",format!("operation '{}' clause expects {} patterns (arguments, optional state, continuation)",op.name,ts.len()),op.body.span.clone());
            }
            for (i, (p, t)) in op.pats.iter().zip(ts).enumerate() {
                let pass = sig.params.get(i).map_or(Pass::Sink, |p| p.pass);
                if !self.irrefutable(p, &t) {
                    self.error(
                        "E0210",
                        "handler operation patterns must be irrefutable",
                        p.span.clone(),
                    );
                }
                self.bind_mode(p, &t, &mut local, pass);
                if let Some(param) = sig.params.get(i) {
                    if let PatternKind::Bind(name) = &p.kind {
                        if let Some(b) = local.bindings.get_mut(name) {
                            b.projections = param.projections.clone();
                        }
                    } else if !param.projections.is_empty() {
                        self.error("E0300", "bind a projected operation argument before destructuring its accessible fields", p.span.clone());
                    }
                }
            }
            self.start_observing();
            self.enter_control(loop_rules::ControlFrame::Clause);
            let actual = self.expr_expected(&op.body, &mut local, Usage::Own, &out);
            self.leave_control();
            union_effects(&mut outgoing, &self.finish_observing());
            self.expect_type(&out, &actual, op.body.span.clone());
            if crate::integer_widths::holds_wide(&out) {
                self.constrain_literals(&op.body, &out);
            }
            self.exhaust(
                &local,
                local.bindings.keys().cloned().collect(),
                op.body.span.clone(),
            );
        }
        for op in decl.ops.iter().filter(|_| region.is_none()) {
            if !seen.contains(&op.name) {
                self.error(
                    "E0405",
                    format!("missing handler clause '{}'", op.name),
                    span.clone(),
                );
            }
        }
        outgoing
            .instances
            .sort_by(|a, b| (&a.name, &a.label).cmp(&(&b.name, &b.label)));
        self.observe(&outgoing);
        self.handler_effects
            .insert((span.start, span.end), outgoing);
        out
    }
}
pub fn literal_type(l: &Literal) -> Type {
    match l {
        Literal::Int(n) => Type::IntLiteral(*n),
        // Above `i128::MAX` only `u128` holds the literal.
        Literal::WideInt(_) => Type::named("u128"),
        Literal::Float(_) => Type::named("f64"),
        Literal::Bool(_) => Type::named("bool"),
        Literal::String(_) => Type::named("String"),
    }
}
pub fn function_type(f: &Function) -> Type {
    Type::Function {
        params: f.params.iter().map(|p| (p.pass, p.ty.clone())).collect(),
        result: Box::new(f.result.clone()),
        effects: f.effects.clone(),
        mode: Mode::Copy,
    }
}
pub fn integer(t: &Type) -> bool {
    crate::numeric::is_integer_name(t.name()) || matches!(t, Type::IntLiteral(_) | Type::Infer)
}
pub fn numeric(t: &Type) -> bool {
    integer(t) || crate::numeric::is_float_name(t.name())
}
pub fn compatible(a: &Type, b: &Type) -> bool {
    if *a == Type::Infer || *b == Type::Infer || b.name() == "Never" {
        return true;
    }
    match (a, b) {
        (Type::IntLiteral(_), Type::IntLiteral(_)) => true,
        (Type::IntLiteral(n), t) | (t, Type::IntLiteral(n)) => {
            crate::numeric::IntegerKind::from_type(t).is_some_and(|k| k.fits(*n))
        }
        (Type::Named(a, aa), Type::Named(b, bb)) => {
            (a == b) && aa.len() == bb.len() && aa.iter().zip(bb).all(|(a, b)| compatible(a, b))
        }
        (Type::Tuple(a), Type::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| compatible(a, b))
        }
        (Type::Named(n, _), Type::Tuple(t)) | (Type::Tuple(t), Type::Named(n, _)) => {
            n == "Unit" && t.is_empty()
        }
        // Capability arguments of region types (`RegionBox[r, T]`).
        (Type::Row(a), Type::Row(b)) => a == b,
        (
            Type::Function {
                params: a,
                result: ar,
                mode: am,
                effects: ae,
            },
            Type::Function {
                params: b,
                result: br,
                mode: bm,
                effects: be,
            },
        ) => {
            effects_include(ae, be)
                && bm <= am
                && a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|((ap, a), (bp, b))| ap == bp && compatible(b, a))
                && compatible(ar, br)
        }
        _ => false,
    }
}
fn infer(expected: &Type, actual: &Type, generics: &[Generic], vars: &mut BTreeMap<String, Type>) {
    match (expected, actual) {
        (Type::Named(n, a), _) if a.is_empty() && generics.iter().any(|g| &g.name == n) => {
            if !vars.contains_key(n) || vars[n] == Type::Infer {
                vars.insert(n.clone(), actual.clone());
            } else if let Type::IntLiteral(previous) = vars[n] {
                if let Type::IntLiteral(next) = actual {
                    let representative = if *next > i64::MAX as i128 || previous > i64::MAX as i128
                    {
                        previous.max(*next)
                    } else if previous < 0 || *next < 0 {
                        previous.min(*next)
                    } else {
                        previous.max(*next)
                    };
                    vars.insert(n.clone(), Type::IntLiteral(representative));
                } else if compatible(&vars[n], actual) {
                    vars.insert(n.clone(), actual.clone());
                }
            } else if compatible(&vars[n], actual) {
                vars.insert(n.clone(), refine_annotation(&vars[n], actual));
            }
        }
        (Type::Named(_, a), Type::Named(_, b)) | (Type::Tuple(a), Type::Tuple(b)) => {
            for (a, b) in a.iter().zip(b) {
                infer(a, b, generics, vars)
            }
        }
        (
            Type::Function {
                params: a,
                result: ar,
                effects: ae,
                ..
            },
            Type::Function {
                params: b,
                result: br,
                effects: be,
                ..
            },
        ) => {
            for ((_, a), (_, b)) in a.iter().zip(b) {
                infer(a, b, generics, vars);
            }
            infer(ar, br, generics, vars);
            if let Some(row) = &ae.tail {
                let remainder = Effects {
                    instances: be
                        .instances
                        .iter()
                        .filter(|e| {
                            !ae.instances
                                .iter()
                                .any(|x| x.name == e.name && x.label == e.label)
                        })
                        .cloned()
                        .collect(),
                    tail: be.tail.clone(),
                };
                if let Some(Type::Row(previous)) = vars.get_mut(row) {
                    for e in remainder.instances {
                        if !previous.instances.contains(&e) {
                            previous.instances.push(e);
                        }
                    }
                    if previous.tail.is_none() {
                        previous.tail = remainder.tail;
                    }
                } else {
                    vars.insert(row.clone(), Type::Row(remainder));
                }
            }
        }
        _ => {}
    }
}

pub fn free_names(e: &Expr) -> BTreeSet<String> {
    fn pat(p: &Pattern, out: &mut BTreeSet<String>) {
        match &p.kind {
            PatternKind::Bind(n) => {
                out.insert(n.clone());
            }
            PatternKind::Tuple(ps) => {
                for p in ps {
                    pat(p, out)
                }
            }
            PatternKind::Constructor { fields, .. } => {
                for (_, p) in fields {
                    pat(p, out)
                }
            }
            _ => {}
        }
    }
    fn walk(e: &Expr, out: &mut BTreeSet<String>) {
        match &e.kind {
            ExprKind::Name(n) => {
                out.insert(n.clone());
            }
            ExprKind::Tuple(es) => {
                for e in es {
                    walk(e, out)
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, e) in fields {
                    walk(e, out)
                }
            }
            ExprKind::Block { statements, tail } => {
                let mut local = BTreeSet::new();
                for s in statements {
                    match s {
                        Statement::Let { pat: p, value, .. } => {
                            let mut used = BTreeSet::new();
                            walk(value, &mut used);
                            out.extend(used.difference(&local).cloned());
                            pat(p, &mut local)
                        }
                        Statement::Expr(e) => {
                            let mut used = BTreeSet::new();
                            walk(e, &mut used);
                            out.extend(used.difference(&local).cloned());
                        }
                    }
                }
                if let Some(e) = tail {
                    let mut used = BTreeSet::new();
                    walk(e, &mut used);
                    out.extend(used.difference(&local).cloned());
                }
            }
            ExprKind::Call { callee, args, .. } => {
                walk(callee, out);
                for a in args {
                    walk(&a.expr, out)
                }
            }
            ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => walk(e, out),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                walk(a, out);
                walk(b, out)
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                walk(condition, out);
                walk(then, out);
                if let Some(e) = otherwise {
                    walk(e, out)
                }
            }
            ExprKind::Match { value, arms } => {
                walk(value, out);
                for (p, e) in arms {
                    let mut used = BTreeSet::new();
                    walk(e, &mut used);
                    let mut bound = BTreeSet::new();
                    pat(p, &mut bound);
                    out.extend(used.difference(&bound).cloned());
                }
            }
            ExprKind::Closure { params, body, .. } => {
                let mut used = BTreeSet::new();
                walk(body, &mut used);
                for p in params {
                    used.remove(&p.name);
                }
                out.extend(used);
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((_, s)) = state {
                    walk(s, out);
                }
                walk(body, out);
                for c in std::iter::once(ret.as_ref()).chain(ops) {
                    let mut used = BTreeSet::new();
                    walk(&c.body, &mut used);
                    let mut bound = BTreeSet::new();
                    for p in &c.pats {
                        pat(p, &mut bound);
                    }
                    out.extend(used.difference(&bound).cloned());
                }
            }
            ExprKind::For { pat: p, iter, body } => {
                walk(iter, out);
                let mut used = BTreeSet::new();
                walk(body, &mut used);
                let mut bound = BTreeSet::new();
                pat(p, &mut bound);
                out.extend(used.difference(&bound).cloned());
            }
            k @ (ExprKind::Loop(_)
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_)) => {
                for c in k.control_children().into_iter().flatten() {
                    walk(c, out)
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(e, &mut out);
    out
}

pub(crate) fn pattern_names(p: &Pattern) -> BTreeSet<String> {
    match &p.kind {
        PatternKind::Bind(n) => BTreeSet::from([n.clone()]),
        PatternKind::Tuple(ps) => ps.iter().flat_map(pattern_names).collect(),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().flat_map(|(_, p)| pattern_names(p)).collect()
        }
        _ => BTreeSet::new(),
    }
}
// Constructor-specialization coverage matrix, including nested enum payloads.
fn is_uninhabited(world: &World, ty: &Type, seen: &mut BTreeSet<String>) -> bool {
    match ty {
        Type::Named(n, _) if n == "Never" => true,
        Type::Tuple(ts) => ts.iter().any(|t| is_uninhabited(world, t, seen)),
        Type::Named(n, args) => {
            let Some(a) = world.adts.get(n) else {
                return false;
            };
            if a.variants.is_empty() {
                return true;
            }
            if !seen.insert(n.clone()) {
                return false;
            }
            let vars: BTreeMap<String, Type> = a
                .generics
                .iter()
                .zip(args)
                .map(|(g, t)| (g.name.clone(), t.clone()))
                .collect();
            let uninhabited = a.variants.iter().all(|v| {
                v.fields
                    .iter()
                    .any(|f| is_uninhabited(world, &substitute(&f.ty, &vars), seen))
            });
            seen.remove(n);
            uninhabited
        }
        _ => false,
    }
}
fn matrix_covered(world: &World, rows: &[Vec<Pattern>], types: &[Type], depth: usize) -> bool {
    if types.is_empty() {
        return !rows.is_empty();
    }
    if rows.is_empty() {
        return types
            .iter()
            .any(|t| is_uninhabited(world, t, &mut BTreeSet::new()));
    }
    if depth > 64 {
        return false;
    }
    let wildcard = |p: &Pattern| matches!(p.kind, PatternKind::Wild | PatternKind::Bind(_));
    if rows
        .iter()
        .any(|r| r.len() == types.len() && r.iter().all(wildcard))
    {
        return true;
    }
    type Constructors = Vec<(String, Vec<(String, Type)>)>;
    let constructors: Option<Constructors> = match &types[0] {
        Type::Named(n, _) if n == "bool" => {
            Some(vec![("true".into(), vec![]), ("false".into(), vec![])])
        }
        Type::Named(n, args) => world.adts.get(n).map(|a| {
            let vars = a
                .generics
                .iter()
                .zip(args)
                .map(|(g, t)| (g.name.clone(), t.clone()))
                .collect();
            a.variants
                .iter()
                .map(|v| {
                    (
                        v.name.clone(),
                        v.fields
                            .iter()
                            .map(|f| (f.name.clone(), substitute(&f.ty, &vars)))
                            .collect(),
                    )
                })
                .collect()
        }),
        Type::Tuple(ts) => Some(vec![(
            "tuple".into(),
            ts.iter()
                .enumerate()
                .map(|(i, t)| (i.to_string(), t.clone()))
                .collect(),
        )]),
        _ => None,
    };
    let Some(constructors) = constructors else {
        let rest = rows
            .iter()
            .filter(|r| r.first().is_some_and(wildcard))
            .map(|r| r[1..].to_vec())
            .collect::<Vec<_>>();
        return matrix_covered(world, &rest, &types[1..], depth + 1);
    };
    constructors.into_iter().all(|(tag, fields)| {
        let mut specialized = vec![];
        for row in rows {
            let Some(p) = row.first() else {
                continue;
            };
            let mut ps = if wildcard(p) {
                fields
                    .iter()
                    .map(|_| Pattern {
                        kind: PatternKind::Wild,
                        span: p.span.clone(),
                    })
                    .collect::<Vec<_>>()
            } else {
                match &p.kind {
                    PatternKind::Constructor {
                        name, fields: ps, ..
                    } if name.rsplit("::").next() == Some(tag.as_str()) => {
                        let mut result = vec![];
                        for (n, _) in &fields {
                            if let Some((_, p)) = ps.iter().find(|(f, _)| f == n) {
                                result.push(p.clone());
                            } else {
                                return false;
                            }
                        }
                        result
                    }
                    PatternKind::Tuple(ps) if tag == "tuple" && ps.len() == fields.len() => {
                        ps.clone()
                    }
                    PatternKind::Literal(Literal::Bool(b)) if tag == b.to_string() => vec![],
                    _ => continue,
                }
            };
            ps.extend_from_slice(&row[1..]);
            specialized.push(ps);
        }
        let mut ts = fields.into_iter().map(|(_, t)| t).collect::<Vec<_>>();
        ts.extend_from_slice(&types[1..]);
        matrix_covered(world, &specialized, &ts, depth + 1)
    })
}

/// The common type of two branches; an `E0200` for integer literals no
/// integer kind holds both of (`Checker::join_types`).
fn join_types(a: &Type, b: &Type, span: &Span, diagnostics: &mut Vec<Diagnostic>) -> Type {
    if a.name() == "Never" || *a == Type::Infer {
        return b.clone();
    }
    if b.name() == "Never" || *b == Type::Infer {
        return a.clone();
    }
    match (a, b) {
        (Type::IntLiteral(x), Type::IntLiteral(y)) => {
            let n = if (*x).max(*y) > i64::MAX as i128 {
                (*x).max(*y)
            } else if (*x).min(*y) < 0 {
                (*x).min(*y)
            } else {
                (*x).max(*y)
            };
            let kind = crate::numeric::IntegerKind::default_for(n);
            if !kind.fits(*x) || !kind.fits(*y) {
                diagnostics.push(Diagnostic::new(
                    "E0200",
                    "branches have no common integer type",
                    span.clone(),
                ));
            }
            Type::IntLiteral(n)
        }
        (Type::IntLiteral(_), _) => b.clone(),
        (_, Type::IntLiteral(_)) => a.clone(),
        (Type::Named(n, aa), Type::Named(m, bb)) if n == m && aa.len() == bb.len() => Type::Named(
            n.clone(),
            aa.iter()
                .zip(bb)
                .map(|(a, b)| join_types(a, b, span, diagnostics))
                .collect(),
        ),
        (Type::Tuple(aa), Type::Tuple(bb)) if aa.len() == bb.len() => Type::Tuple(
            aa.iter()
                .zip(bb)
                .map(|(a, b)| join_types(a, b, span, diagnostics))
                .collect(),
        ),
        (
            Type::Function {
                params,
                result,
                effects,
                mode,
            },
            Type::Function {
                result: br,
                effects: be,
                mode: bm,
                ..
            },
        ) => {
            let mut row = effects.clone();
            union_effects(&mut row, be);
            Type::Function {
                params: params.clone(),
                result: Box::new(join_types(result, br, span, diagnostics)),
                effects: row,
                mode: mode.join(*bm),
            }
        }
        _ => a.clone(),
    }
}
fn unresolved(t: &Type) -> bool {
    match t {
        Type::Infer => true,
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(unresolved),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| unresolved(t)) || unresolved(result)
        }
        _ => false,
    }
}
fn union_effects(into: &mut Effects, from: &Effects) {
    for e in &from.instances {
        if !into.instances.contains(e) {
            into.instances.push(e.clone());
        }
    }
    if into.tail.is_none() {
        into.tail = from.tail.clone();
    }
}
fn bound_capabilities(t: &Type) -> BTreeSet<String> {
    fn collect(t: &Type, caps: &mut BTreeSet<String>) {
        match t {
            Type::Named(_, ts) | Type::Tuple(ts) => {
                for t in ts {
                    collect(t, caps);
                }
            }
            Type::Row(effects) => {
                for e in &effects.instances {
                    if let Some(label) = &e.label
                        && (label.starts_with("$cap") || label.starts_with("$fncap:"))
                    {
                        caps.insert(label.clone());
                    }
                    for t in &e.args {
                        collect(t, caps);
                    }
                }
            }
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                for e in &effects.instances {
                    if let Some(label) = &e.label
                        && (label.starts_with("$cap") || label.starts_with("$fncap:"))
                    {
                        caps.insert(label.clone());
                    }
                    for t in &e.args {
                        collect(t, caps);
                    }
                }
                for (_, t) in params {
                    collect(t, caps);
                }
                collect(result, caps);
            }
            _ => {}
        }
    }
    let mut caps = BTreeSet::new();
    collect(t, &mut caps);
    caps
}
fn contains_capability(t: &Type, cap: &Effect) -> bool {
    match t {
        Type::Row(effects) => effects
            .instances
            .iter()
            .any(|e| e.label == cap.label || e.args.iter().any(|t| contains_capability(t, cap))),
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(|t| contains_capability(t, cap)),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            effects.instances.iter().any(|e| e.label == cap.label)
                || params.iter().any(|(_, t)| contains_capability(t, cap))
                || contains_capability(result, cap)
        }
        _ => false,
    }
}

fn effects_include(expected: &Effects, actual: &Effects) -> bool {
    (actual.tail.is_none() || expected.tail.is_some())
        && actual.instances.iter().all(|e| {
            expected.tail.is_some()
                || expected.instances.iter().any(|x| {
                    x.name == e.name
                        && (x.label.is_none() || x.label == e.label)
                        && x.args.len() == e.args.len()
                        && x.args.iter().zip(&e.args).all(|(a, b)| compatible(a, b))
                })
        })
}
fn refine_annotation(expected: &Type, actual: &Type) -> Type {
    match (expected, actual) {
        (Type::Named(n, a), Type::Named(m, b)) if n == m => Type::Named(
            n.clone(),
            a.iter()
                .zip(b)
                .map(|(a, b)| refine_annotation(a, b))
                .collect(),
        ),
        (Type::Tuple(a), Type::Tuple(b)) => Type::Tuple(
            a.iter()
                .zip(b)
                .map(|(a, b)| refine_annotation(a, b))
                .collect(),
        ),
        (
            Type::Function {
                params,
                result,
                effects,
                mode,
            },
            Type::Function {
                result: ar,
                effects: ae,
                ..
            },
        ) => {
            let mut row = ae.clone();
            for e in &effects.instances {
                if !row.instances.iter().any(|x| x.name == e.name) {
                    row.instances.push(e.clone());
                }
            }
            if row.tail.is_none() {
                row.tail = effects.tail.clone();
            }
            Type::Function {
                params: params.clone(),
                result: Box::new(refine_annotation(result, ar)),
                effects: row,
                mode: *mode,
            }
        }
        _ => expected.clone(),
    }
}

pub fn clause_free_names(c: &Clause) -> BTreeSet<String> {
    let mut free = free_names(&c.body);
    for p in &c.pats {
        for n in pattern_names(p) {
            free.remove(&n);
        }
    }
    free
}

fn function_capability(name: &str, index: usize) -> String {
    format!("$fncap:{name}:{index}")
}
fn required_label(label: &Option<String>) -> Option<String> {
    label.as_ref().and_then(|s| {
        if s.starts_with("$latent:") {
            s.split(':')
                .nth(2)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        } else {
            Some(s.clone())
        }
    })
}
fn latent_capability(origin: &str, e: &Effect) -> String {
    format!(
        "$latent:{origin}:{}:{}",
        required_label(&e.label).unwrap_or_default(),
        e.name
    )
}
fn rename_capabilities(t: &Type, labels: &BTreeMap<String, Option<String>>) -> Type {
    match t {
        Type::Named(n, ts) => Type::Named(
            n.clone(),
            ts.iter().map(|t| rename_capabilities(t, labels)).collect(),
        ),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| rename_capabilities(t, labels)).collect()),
        Type::Row(effects) => {
            let mut effects = effects.clone();
            for e in &mut effects.instances {
                if let Some(label) = e.label.as_ref().and_then(|l| labels.get(l)) {
                    e.label = label.clone();
                }
                e.args = e
                    .args
                    .iter()
                    .map(|t| rename_capabilities(t, labels))
                    .collect();
            }
            Type::Row(effects)
        }
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => {
            let mut effects = effects.clone();
            for e in &mut effects.instances {
                if let Some(label) = e.label.as_ref().and_then(|l| labels.get(l)) {
                    e.label = label.clone();
                }
                e.args = e
                    .args
                    .iter()
                    .map(|t| rename_capabilities(t, labels))
                    .collect();
            }
            Type::Function {
                params: params
                    .iter()
                    .map(|(m, t)| (*m, rename_capabilities(t, labels)))
                    .collect(),
                result: Box::new(rename_capabilities(result, labels)),
                effects,
                mode: *mode,
            }
        }
        _ => t.clone(),
    }
}
fn bind_signature_capabilities(t: &Type, f: &Function) -> Type {
    fn collect(t: &Type, f: &Function) -> Type {
        match t {
            Type::Function {
                params,
                result,
                effects,
                mode,
            } => {
                let mut row = effects.clone();
                for e in &mut row.instances {
                    if let Some((i, _)) = f.effects.instances.iter().enumerate().find(|(_, x)| {
                        x.name == e.name
                            && x.args == e.args
                            && (e.label.is_none() || e.label == x.label)
                    }) {
                        e.label = Some(function_capability(&f.name, i));
                    }
                }
                Type::Function {
                    params: params.iter().map(|(m, t)| (*m, collect(t, f))).collect(),
                    result: Box::new(collect(result, f)),
                    effects: row,
                    mode: *mode,
                }
            }
            Type::Named(n, ts) => {
                Type::Named(n.clone(), ts.iter().map(|t| collect(t, f)).collect())
            }
            Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| collect(t, f)).collect()),
            _ => t.clone(),
        }
    }
    collect(t, f)
}
fn function_value_type(f: &Function, inferred: Option<&Type>) -> Type {
    let mut effects = f.effects.clone();
    let mut labels = BTreeMap::new();
    for (i, e) in effects.instances.iter_mut().enumerate() {
        let label = latent_capability(&format!("f{}_{}", f.name.replace(':', "_"), i), e);
        labels.insert(function_capability(&f.name, i), Some(label.clone()));
        e.label = Some(label);
    }
    Type::Function {
        params: f.params.iter().map(|p| (p.pass, p.ty.clone())).collect(),
        result: Box::new(rename_capabilities(inferred.unwrap_or(&f.result), &labels)),
        effects,
        mode: Mode::Copy,
    }
}
fn instantiate_latent_effects(actual: &Type, target: &Type) -> Type {
    let mut labels = BTreeMap::new();
    fn collect(a: &Type, b: &Type, labels: &mut BTreeMap<String, Option<String>>) {
        if let (
            Type::Function {
                params: a,
                result: ar,
                effects: ae,
                ..
            },
            Type::Function {
                params: b,
                result: br,
                effects: be,
                ..
            },
        ) = (a, b)
        {
            if be.tail.is_none() {
                for e in &ae.instances {
                    if let Some(label) = &e.label
                        && label.starts_with("$latent:")
                        && let Some(want) = be
                            .instances
                            .iter()
                            .find(|x| x.name == e.name && x.args == e.args)
                        && want.label.is_some()
                    {
                        labels.insert(label.clone(), want.label.clone());
                    }
                }
            }
            for ((_, a), (_, b)) in a.iter().zip(b) {
                collect(a, b, labels);
            }
            collect(ar, br, labels);
        }
    }
    collect(actual, target, &mut labels);
    rename_capabilities(actual, &labels)
}

/// The core-typing relations and the exhaustiveness matrix, unchanged, for
/// the differential of their Vesper port (self-hosting SH2 lane 7, #1179,
/// `tests/self_host_check_core_typing.rs`). Not an API.
#[doc(hidden)]
pub mod core_typing_probe {
    use super::*;
    pub fn matrix_covered(world: &World, rows: &[Vec<Pattern>], types: &[Type]) -> bool {
        super::matrix_covered(world, rows, types, 0)
    }
    pub fn is_uninhabited(world: &World, ty: &Type) -> bool {
        super::is_uninhabited(world, ty, &mut BTreeSet::new())
    }
    pub fn infer(
        expected: &Type,
        actual: &Type,
        generics: &[Generic],
        vars: &mut BTreeMap<String, Type>,
    ) {
        super::infer(expected, actual, generics, vars)
    }
    pub fn refine_annotation(expected: &Type, actual: &Type) -> Type {
        super::refine_annotation(expected, actual)
    }
    pub fn unresolved(t: &Type) -> bool {
        super::unresolved(t)
    }
    /// The joined type and the number of `E0200` diagnostics.
    pub fn join_types(a: &Type, b: &Type) -> (Type, usize) {
        let mut diagnostics = vec![];
        let t = super::join_types(a, b, &(0..0), &mut diagnostics);
        (t, diagnostics.len())
    }
}
#[cfg(test)]
mod isolated_item_tests {
    use super::*;
    use std::time::Instant;

    fn extract_fn(source: &str, name: &str) -> Function {
        let parsed = crate::parser::parse(source);
        assert!(
            parsed.diagnostics.is_empty(),
            "parse errors: {:?}",
            parsed.diagnostics
        );
        parsed
            .module
            .items
            .into_iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == name => Some(*f),
                _ => None,
            })
            .unwrap_or_else(|| panic!("function '{name}' not found"))
    }

    #[test]
    fn test_attrs_parse_cleanly_in_root_parser() {
        let src = r#"
@test(cases = 64)
@test_case(1, 2)
@test_case(a = 3, b = 4)
fn add_ok(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    a + b
}
"#;
        let parsed = crate::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let attrs = vesper_syntax::test_attrs::extract_test_attrs(src);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].fn_name, "add_ok");
        assert_eq!(attrs[0].test.as_ref().unwrap().cases, Some(64));
        assert_eq!(attrs[0].test_cases.len(), 2);
    }

    #[test]
    fn check_isolated_item_catches_e0302_and_e0301_with_related_spans() {
        let base_src = r#"
linear struct Tx { id: i64 }
copy struct Counter { val: i64 }

fn consume_tx(sink tx: Tx) -> i64 {
    let Tx { id } = tx;
    id
}

fn swap_counters(inout a: Counter, inout b: Counter) -> i64 {
    let t = a.val;
    a.val = b.val;
    b.val = t;
    a.val
}
"#;
        let base_parsed = crate::parser::parse(base_src);
        assert!(
            base_parsed.diagnostics.is_empty(),
            "{:?}",
            base_parsed.diagnostics
        );
        let base_checked = check(&base_parsed.module);
        assert!(
            base_checked.diagnostics.is_empty(),
            "{:?}",
            base_checked.diagnostics
        );
        let world = &base_checked.world;
        let orig_fn_count = world.functions.len();

        let neg_e0302_src = r#"
@test(expect_error = E0302, primary = "tx", related = "linear binding introduced here")
fn leak_tx() -> i64 {
    let tx = Tx { id: 7 };
    0
}
"#;
        let leak_fn = extract_fn(neg_e0302_src, "leak_tx");

        let neg_e0301_src = r#"
@test(expect_error = E0301, related = "introduced here")
fn alias_mut() -> i64 {
    let mut c = Counter { val: 1 };
    swap_counters(c, c)
}
"#;
        let alias_fn = extract_fn(neg_e0301_src, "alias_mut");

        let ok_src = r#"
@test
fn ok_tx() -> i64 {
    let tx = Tx { id: 9 };
    consume_tx(tx)
}
"#;
        let ok_fn = extract_fn(ok_src, "ok_tx");

        // Warm up once, then measure isolated checks.
        let t0 = Instant::now();
        let e0302_diags = world.check_isolated_item(&leak_fn);
        let d_e0302 = t0.elapsed();

        let t1 = Instant::now();
        let e0301_diags = world.check_isolated_item(&alias_fn);
        let d_e0301 = t1.elapsed();

        assert!(
            e0302_diags
                .iter()
                .any(|d| d.code == "E0302" && !d.related.is_empty()),
            "expected E0302 with related span, got: {e0302_diags:?}"
        );
        assert!(
            e0301_diags
                .iter()
                .any(|d| d.code == "E0301" && !d.related.is_empty()),
            "expected E0301 with related span, got: {e0301_diags:?}"
        );
        assert!(
            world.check_isolated_module_fn(&ok_fn).is_ok(),
            "expected ok_tx to pass isolated check"
        );
        assert!(
            world.check_isolated_module_fn(&leak_fn).is_err(),
            "expected leak_tx to fail isolated check"
        );
        // Parent world must remain unmodified.
        assert_eq!(world.functions.len(), orig_fn_count);
        assert!(!world.functions.contains_key("leak_tx"));
        assert!(!world.functions.contains_key("alias_mut"));
        // Sanity check latency (sub-millisecond in release, very fast in debug).
        assert!(
            d_e0302.as_millis() < 50 && d_e0301.as_millis() < 50,
            "isolated checks unexpectedly slow: e0302={d_e0302:?}, e0301={d_e0301:?}"
        );
    }
}
