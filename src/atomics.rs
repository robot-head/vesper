//! Source atomics (port wave P3a lane 2, #1037, survey feature F12).
//!
//! # Surface
//!
//! `Atomic[T]` is a shared mutable cell of one `bool` or integer kind (`u8`,
//! `u16`, `u32`, `u64`, `usize`, `i8`, `i16`, `i32`, `i64`). It is built by
//! `Atomic::new(v)` and every operation takes the cell by `in`:
//!
//! ```text
//! let hits: Atomic[u64] = Atomic::new(0);
//! hits.fetch_add(1, MemoryOrder::Relaxed);
//! let seen = hits.load(MemoryOrder::Acquire);
//! ```
//!
//! The operations are `load`, `store`, `swap`, `compare_exchange` (an
//! `Result[T, T]`: `Ok(previous)` when the cell held `current`,
//! `Err(actual)` otherwise), `fetch_add`, `fetch_sub` (integers, wrapping),
//! `fetch_and`, `fetch_or`, `fetch_xor` (integers and `bool`), and the
//! consuming `into_inner`. Every order argument is a literal
//! `MemoryOrder::{Relaxed, Acquire, Release, AcqRel, SeqCst}`; a `load` (and
//! the failure order of `compare_exchange`) refuses `Release` and `AcqRel`,
//! a `store` refuses `Acquire` and `AcqRel` ([`check_world`], `E0820`).
//! Atomic operations are total and carry no effect; `Atomic::new` allocates
//! like `ChunkSeq::new`. The manuscript rule is §15.4 "Source Atomics".
//!
//! # Representation
//!
//! The prelude ([`prelude`]) declares `struct Atomic[T] { cell: ChunkSeq[T] }`
//! (affine: never `copy`) holding exactly one element, so every copy of the
//! one-word sequence handle that an `in` borrow passes names the same heap
//! word, and the existing drop glue frees it. The cell field is private to
//! the prelude (`E0822`), so nothing grows or reads it except the
//! operations. The prelude method bodies only give the signatures to the
//! checker: they are recursive, so the inliner never copies them, and every
//! backend that runs atomics intercepts the call by name instead:
//!
//! * the evaluator (`runtime.rs`) updates the element of the shared cell;
//! * native code (the JIT, executables, exports and sessions) lowers each
//!   call to one Cranelift `atomic_load`, `atomic_store`, `atomic_rmw` or
//!   `atomic_cas` on the element, at the kind's own width (the element is
//!   at offset 0 of the sequence data, which is word aligned, and every
//!   supported target is little-endian). Cranelift atomics are sequentially
//!   consistent, which satisfies every weaker order;
//! * the verified backend refuses the cell's `ChunkSeq` type with its
//!   typed `Unsupported::Type` diagnostic.
//!
//! # Scoped workers
//!
//! `parallel_for(in data, n, body)` runs `body(in data, i)` for every `i` in
//! `0..n` and returns after the last one (fork-join). The workers share
//! `data` only through an `in` borrow, so the only state they can update
//! together is an `Atomic` cell inside it; a body that takes `data` by
//! `inout` (`E0824`), and a body that is not a named function whose effect
//! row holds only stateless capabilities (`E0823`, [`worker_row`]), are
//! refused. The scope cannot leak a worker: the call joins every iteration
//! before it returns, so no worker outlives the borrow.
//!
//! The prelude body is the sequential loop, which is a valid schedule of the
//! fork-join call, so the evaluator, executables and the verified backend's
//! refusal need nothing new. The JIT intercepts the call and hands the
//! compiled body to a process-wide Pulse pool ([`workers`]); a nested
//! `parallel_for` inside a worker runs on that worker.
use crate::{ast::*, check::World, numeric::IntegerKind};

pub mod workers;

/// The builtin fork-join loop over the Pulse pool (scoped workers).
pub const PARALLEL_FOR: &str = "parallel_for";

/// The builtin atomic cell type `Atomic[T]`.
pub const ATOMIC_TYPE: &str = "Atomic";
/// The builtin memory-order enum.
pub const MEMORY_ORDER: &str = "MemoryOrder";
/// The private element field of `Atomic[T]`.
const CELL: &str = "cell";

/// The memory orders, in declaration order.
pub const ORDERS: [&str; 5] = ["Relaxed", "Acquire", "Release", "AcqRel", "SeqCst"];

/// An atomic operation, named by the method of `Atomic[T]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicOp {
    Load,
    Store,
    Swap,
    CompareExchange,
    FetchAdd,
    FetchSub,
    FetchAnd,
    FetchOr,
    FetchXor,
}

/// The intercepted operations, by method name.
pub const OPS: [(&str, AtomicOp); 9] = [
    ("load", AtomicOp::Load),
    ("store", AtomicOp::Store),
    ("swap", AtomicOp::Swap),
    ("compare_exchange", AtomicOp::CompareExchange),
    ("fetch_add", AtomicOp::FetchAdd),
    ("fetch_sub", AtomicOp::FetchSub),
    ("fetch_and", AtomicOp::FetchAnd),
    ("fetch_or", AtomicOp::FetchOr),
    ("fetch_xor", AtomicOp::FetchXor),
];

impl AtomicOp {
    /// The operation a call of `callee` (`Atomic::fetch_add`, ...) performs.
    pub fn of_callee(callee: &str) -> Option<Self> {
        Self::of_method(callee.strip_prefix("Atomic::")?)
    }
    /// The operation of method `name`.
    pub fn of_method(name: &str) -> Option<Self> {
        OPS.iter().find(|(n, _)| *n == name).map(|(_, op)| *op)
    }
    /// Arguments after the receiver: values, then orders.
    pub fn values(self) -> usize {
        match self {
            Self::Load => 0,
            Self::CompareExchange => 2,
            _ => 1,
        }
    }
    /// Order arguments after the values.
    pub fn orders(self) -> usize {
        if self == Self::CompareExchange { 2 } else { 1 }
    }
    /// Whether the operation is defined on `bool` cells (integers have all).
    pub fn on_bool(self) -> bool {
        !matches!(self, Self::FetchAdd | Self::FetchSub)
    }
    /// The new cell contents of a read-modify-write of `old` with operand
    /// `v` in `kind` (wrapping). `None` for operations that are not one.
    pub fn rmw(self, kind: IntegerKind, old: i128, v: i128) -> Option<i128> {
        let (p, q) = (i128::from(kind.pattern(old)), i128::from(kind.pattern(v)));
        Some(kind.wrap(match self {
            Self::Swap => v,
            Self::FetchAdd => old + v,
            Self::FetchSub => old - v,
            Self::FetchAnd => p & q,
            Self::FetchOr => p | q,
            Self::FetchXor => p ^ q,
            _ => return None,
        }))
    }
}

/// The cell kind of `Atomic[T]`: `Some(None)` for `bool`, `Some(Some(k))`
/// for a one-word integer kind, `None` for any other `T` (`i128` and
/// `u128` have no single-instruction atomics on every target; SH2 lane 2,
/// #1174).
pub fn cell_kind(t: &Type) -> Option<Option<IntegerKind>> {
    let t = crate::numeric::default_type(t);
    match &t {
        Type::Named(n, a) if n == "bool" && a.is_empty() => Some(None),
        _ => IntegerKind::from_type(&t).filter(|k| !k.wide()).map(Some),
    }
}

/// `T` of `Atomic[T]`.
pub fn cell_type(t: &Type) -> Option<&Type> {
    match t {
        Type::Named(n, a) if n == ATOMIC_TYPE && a.len() == 1 => Some(&a[0]),
        _ => None,
    }
}

fn type_mentions(t: &Type) -> bool {
    match t {
        Type::Named(n, a) => n == ATOMIC_TYPE || n == MEMORY_ORDER || a.iter().any(type_mentions),
        Type::Tuple(ts) => ts.iter().any(type_mentions),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| type_mentions(t)) || type_mentions(result)
        }
        _ => false,
    }
}

fn path_mentions(n: &str) -> bool {
    n.split("::")
        .next()
        .is_some_and(|h| h == ATOMIC_TYPE || h == MEMORY_ORDER)
}

fn pattern_mentions(p: &Pattern) -> bool {
    match &p.kind {
        PatternKind::Tuple(ps) => ps.iter().any(pattern_mentions),
        PatternKind::Constructor { name, fields, .. } => {
            path_mentions(name) || fields.iter().any(|(_, p)| pattern_mentions(p))
        }
        _ => false,
    }
}

/// Every direct subexpression of `e`, with the types and patterns it names.
fn walk<'e>(e: &'e Expr, f: &mut dyn FnMut(&'e Expr)) {
    f(e);
    let mut each = |x: &'e Expr| walk(x, f);
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Tuple(es) => es.iter().for_each(each),
        ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, x)| each(x)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, .. } => each(value),
                    Statement::Expr(x) => each(x),
                }
            }
            if let Some(t) = tail {
                each(t);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            each(callee);
            for a in args {
                each(&a.expr);
            }
        }
        ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) | ExprKind::Loop(b) => {
            each(b)
        }
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            each(a);
            each(b);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            each(condition);
            each(then);
            if let Some(o) = otherwise {
                each(o);
            }
        }
        ExprKind::Match { value, arms } => {
            each(value);
            for (_, x) in arms {
                each(x);
            }
        }
        ExprKind::Closure { body, .. } => each(body),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, s)) = state {
                each(s);
            }
            each(body);
            each(&ret.body);
            for c in ops {
                each(&c.body);
            }
        }
        ExprKind::For { iter, body, .. } => {
            each(iter);
            each(body);
        }
        ExprKind::Range { start, end, .. } => {
            each(start);
            each(end);
        }
        ExprKind::Break(v) | ExprKind::Return(v) => {
            if let Some(v) = v {
                each(v);
            }
        }
        ExprKind::Continue => {}
    }
}

fn expr_mentions(e: &Expr) -> bool {
    let mut found = false;
    walk(e, &mut |x| {
        found |= match &x.kind {
            ExprKind::Name(n) | ExprKind::Struct { name: n, .. } => path_mentions(n),
            ExprKind::Call { types, .. } => types.iter().any(type_mentions),
            ExprKind::Block { statements, .. } => statements.iter().any(|s| {
                matches!(s, Statement::Let { pat, ty, .. } if type_mentions(ty) || pattern_mentions(pat))
            }),
            ExprKind::Match { arms, .. } => arms.iter().any(|(p, _)| pattern_mentions(p)),
            ExprKind::Closure { params, result, .. } => {
                type_mentions(result) || params.iter().any(|p| type_mentions(&p.ty))
            }
            _ => false,
        };
    });
    found
}

fn function_mentions(f: &Function) -> bool {
    f.params.iter().any(|p| type_mentions(&p.ty))
        || type_mentions(&f.result)
        || f.body.as_ref().is_some_and(expr_mentions)
}

/// Whether `module` uses `Atomic` or `MemoryOrder` without declaring either.
pub fn needs_prelude(module: &Module) -> bool {
    let declares = module
        .items
        .iter()
        .any(|i| matches!(i, Item::Adt(a) if a.name == ATOMIC_TYPE || a.name == MEMORY_ORDER));
    !declares
        && module.items.iter().any(|item| match item {
            Item::Function(f) => function_mentions(f),
            Item::Impl { ty, methods, .. } => {
                type_mentions(ty) || methods.iter().any(function_mentions)
            }
            Item::Adt(a) => a
                .variants
                .iter()
                .flat_map(|v| &v.fields)
                .any(|f| type_mentions(&f.ty)),
            Item::Effect(e) => e.ops.iter().any(function_mentions),
            Item::Trait(t) => t.methods.iter().any(function_mentions),
            Item::Conformance(c) => c.methods.iter().any(function_mentions),
        })
}

/// Whether `module` calls `parallel_for` without declaring it.
pub fn needs_workers(module: &Module) -> bool {
    let declares = module
        .items
        .iter()
        .any(|i| matches!(i, Item::Function(f) if f.name == PARALLEL_FOR));
    !declares
        && module.items.iter().any(|item| {
            let calls = |f: &Function| {
                f.body.as_ref().is_some_and(|b| {
                    let mut found = false;
                    walk(b, &mut |x| {
                        found |= matches!(&x.kind, ExprKind::Name(n) if n == PARALLEL_FOR);
                    });
                    found
                })
            };
            match item {
                Item::Function(f) => calls(f),
                Item::Impl { methods, .. } => methods.iter().any(calls),
                Item::Conformance(c) => c.methods.iter().any(calls),
                _ => false,
            }
        })
}

/// The scoped-worker prelude: the sequential schedule of the fork-join
/// loop. It takes a function value, so native code specializes it into its
/// callers, and the JIT intercepts the call before that ([`workers`]).
pub const WORKERS_PRELUDE: &str = "\
fn parallel_for[A, e: eff](in data: A, n: u64, body: fn @copy(in A, u64) eff [e]) eff [e] {
    for i in 0..n { body(in data, i); }
}
";

/// The prelude source: the cell type, the orders and the operations. A
/// method body only types the call. Every operation that updates the cell
/// recurses instead, so no inliner copies a body in place of the
/// intercepted call, and a backend that does not intercept it overflows its
/// stack rather than silently dropping the update.
pub const PRELUDE: &str = "\
struct Atomic[T: mod copy] { cell: ChunkSeq[T] }
copy enum MemoryOrder { Relaxed, Acquire, Release, AcqRel, SeqCst }
impl [T: mod copy] Atomic[T] {
    fn new(value: T) -> Atomic[T] {
        let mut cell: ChunkSeq[T] = ChunkSeq::with_capacity[T](1);
        cell.push(value);
        Atomic { cell: cell }
    }
    fn load(in self, order: MemoryOrder) -> T {
        match self.cell.try_get(0) { Option::Some(v) => v, Option::None => self.load(order) }
    }
    fn store(in self, value: T, order: MemoryOrder) {
        match self.cell.try_get(0) { Option::Some(_) => self.store(value, order), Option::None => () }
    }
    fn swap(in self, value: T, order: MemoryOrder) -> T {
        match self.cell.try_get(0) { Option::Some(_) => self.swap(value, order), Option::None => value }
    }
    fn compare_exchange(in self, current: T, new: T, success: MemoryOrder, failure: MemoryOrder) -> Result[T, T] {
        match self.cell.try_get(0) {
            Option::Some(_) => self.compare_exchange(current, new, success, failure),
            Option::None => Result::Err(current),
        }
    }
    fn fetch_add(in self, value: T, order: MemoryOrder) -> T {
        match self.cell.try_get(0) { Option::Some(_) => self.fetch_add(value, order), Option::None => value }
    }
    fn fetch_sub(in self, value: T, order: MemoryOrder) -> T {
        match self.cell.try_get(0) { Option::Some(_) => self.fetch_sub(value, order), Option::None => value }
    }
    fn fetch_and(in self, value: T, order: MemoryOrder) -> T {
        match self.cell.try_get(0) { Option::Some(_) => self.fetch_and(value, order), Option::None => value }
    }
    fn fetch_or(in self, value: T, order: MemoryOrder) -> T {
        match self.cell.try_get(0) { Option::Some(_) => self.fetch_or(value, order), Option::None => value }
    }
    fn fetch_xor(in self, value: T, order: MemoryOrder) -> T {
        match self.cell.try_get(0) { Option::Some(_) => self.fetch_xor(value, order), Option::None => value }
    }
    fn into_inner(sink self) -> T {
        self.load(MemoryOrder::SeqCst)
    }
}
";

/// The atomic and worker preludes for `module` (empty unless
/// [`needs_prelude`] or [`needs_workers`]), with
/// every span after `after`.
pub(crate) fn prelude(module: &Module, after: usize) -> Module {
    let source = match (needs_prelude(module), needs_workers(module)) {
        (false, false) => return Module::default(),
        (true, false) => PRELUDE.to_string(),
        (false, true) => WORKERS_PRELUDE.to_string(),
        (true, true) => format!("{PRELUDE}{WORKERS_PRELUDE}"),
    };
    let end = after.max(crate::integer_widths::module_end(module));
    let parsed = crate::integer_widths::parse_at(&source, end + 1);
    debug_assert!(
        parsed.diagnostics.is_empty(),
        "atomic prelude: {:?}",
        parsed.diagnostics
    );
    parsed.module
}

/// Whether `name` is a function of the atomic prelude in `world`.
pub fn is_prelude_function(world: &World, name: &str) -> bool {
    (world.workers_prelude && name == PARALLEL_FOR)
        || world.atomic_prelude
            && name.strip_prefix("Atomic::").is_some_and(|m| {
                matches!(m, "new" | "into_inner") || AtomicOp::of_method(m).is_some()
            })
}

/// Whether the call `callee` in `world` is an intercepted atomic operation.
pub fn intercepted(world: &World, callee: &str) -> Option<AtomicOp> {
    if world.atomic_prelude {
        AtomicOp::of_callee(callee)
    } else {
        None
    }
}

fn diagnostic(code: &str, message: String, span: &Span) -> Diagnostic {
    Diagnostic::new(code, message, span.clone())
}

fn order_name(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Name(n) => n
            .strip_prefix("MemoryOrder::")
            .filter(|o| ORDERS.contains(o)),
        _ => None,
    }
}

/// The source rules of atomics: cell kinds, literal and valid orders, and
/// the private cell (module docs).
pub fn check_world(world: &World) -> Vec<Diagnostic> {
    let mut out = vec![];
    if !world.atomic_prelude && !world.workers_prelude {
        return out;
    }
    for f in world.functions.values() {
        if is_prelude_function(world, &f.name) {
            continue;
        }
        if let Some(body) = &f.body {
            walk(body, &mut |e| {
                if world.atomic_prelude {
                    check_expr(&world.types, e, &mut out);
                }
                if world.workers_prelude {
                    check_workers(world, e, &mut out);
                }
            });
        }
    }
    out
}

/// Whether an effect row may run on a `parallel_for` worker (§15.4): it is
/// closed and holds only stateless capabilities, which need no handler,
/// capture or per-caller state: the built-in capability effects
/// (`Realtime`, `ConstTime`, ...), foreign libraries (`Ffi[Lib]`; the
/// Bridge frontend refuses isolated ones) and an `Alloc` declared without
/// operations. The empty row is one.
pub fn worker_row(world: &World, effects: &Effects) -> bool {
    effects.tail.is_none()
        && effects.instances.iter().all(|e| {
            e.label.is_none()
                && (crate::capability_effects::is_capability_effect(&e.name)
                    || crate::ffi_lowering::is_foreign_effect(&e.name)
                    || (e.name == crate::fip::ALLOC_EFFECT
                        && world.effects.get(&e.name).is_some_and(|d| d.ops.is_empty())))
        })
}

/// The named function a `parallel_for` call runs on its workers, if `body`
/// is one the pool can run: a top-level, non-generic function whose effect
/// row is a [`worker_row`] (`E0823` otherwise).
pub fn worker_body<'w>(world: &'w World, body: &Expr) -> Option<&'w Function> {
    let ExprKind::Name(n) = &body.kind else {
        return None;
    };
    world.functions.get(n).filter(|f| {
        f.generics.is_empty()
            && worker_row(world, &f.effects)
            && !is_prelude_function(world, &f.name)
    })
}

/// Types whose JIT runtime state is per thread (`TiledSeq` handles, `Box`
/// and region allocations): a worker on another thread would not find the
/// caller's values.
const THREAD_AFFINE: [&str; 4] = [
    crate::tiled::TILED_SEQ_TYPE,
    crate::fip::BOX_TYPE,
    crate::regions::BOX_TYPE,
    crate::regions::WATERMARK_TYPE,
];

fn affine_type(world: &World, t: &Type, seen: &mut std::collections::BTreeSet<String>) -> bool {
    match t {
        Type::Tuple(ts) => ts.iter().any(|t| affine_type(world, t, seen)),
        Type::Function { .. } => false,
        Type::Named(n, args) => {
            if THREAD_AFFINE.contains(&n.as_str()) {
                return true;
            }
            if args.iter().any(|t| affine_type(world, t, seen)) {
                return true;
            }
            if !seen.insert(n.clone()) {
                return false;
            }
            world.adts.get(n).is_some_and(|a| {
                a.variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .any(|f| affine_type(world, &f.ty, seen))
            })
        }
        _ => false,
    }
}

/// Whether a `parallel_for` over `data` and `body` touches state the JIT
/// runtime keeps per thread: the data type, or any type in a function the
/// body reaches by name or method call. The JIT then runs the loop on the
/// calling thread.
pub fn thread_affine(world: &World, body: &str, data: &Type) -> bool {
    let mut seen_types = std::collections::BTreeSet::new();
    if affine_type(world, data, &mut seen_types) {
        return true;
    }
    let mut stack = vec![body.to_string()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(f) = world.functions.get(&name) else {
            continue;
        };
        let affine = |t: &Type| affine_type(world, t, &mut std::collections::BTreeSet::new());
        if f.params.iter().any(|p| affine(&p.ty)) || affine(&f.result) {
            return true;
        }
        let Some(b) = &f.body else {
            continue;
        };
        let (lo, hi) = (b.span.start, b.span.end);
        if world
            .types
            .range((lo, 0)..(hi + 1, 0))
            .any(|((_, end), t)| *end <= hi && affine(t))
        {
            return true;
        }
        walk(b, &mut |e| match &e.kind {
            ExprKind::Name(n) if world.functions.contains_key(n) => stack.push(n.clone()),
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Field(base, m) = &callee.kind
                    && let Some(Type::Named(head, _)) =
                        world.types.get(&(base.span.start, base.span.end))
                {
                    let method = format!("{head}::{m}");
                    if world.functions.contains_key(&method) {
                        stack.push(method);
                    }
                }
            }
            _ => {}
        });
    }
    false
}

/// Every `parallel_for` body the program names, with the span of the body
/// argument: the Bridge frontend refuses bodies that reach an isolated
/// library ([`worker_row`]).
pub fn worker_bodies(world: &World) -> Vec<(Span, String)> {
    let mut out = vec![];
    if !world.workers_prelude {
        return out;
    }
    for f in world.functions.values() {
        if is_prelude_function(world, &f.name) {
            continue;
        }
        if let Some(body) = &f.body {
            walk(body, &mut |e| {
                if let ExprKind::Call { callee, args, .. } = &e.kind
                    && matches!(&callee.kind, ExprKind::Name(n) if n == PARALLEL_FOR)
                    && args.len() == 3
                    && let ExprKind::Name(n) = &args[2].expr.kind
                {
                    out.push((args[2].expr.span.clone(), n.clone()));
                }
            });
        }
    }
    out
}

/// The scoped-worker rules (module docs): workers share `data` only by
/// `in`, and each body is a named function with a worker row.
fn check_workers(world: &World, e: &Expr, out: &mut Vec<Diagnostic>) {
    let ExprKind::Call { callee, args, .. } = &e.kind else {
        return;
    };
    if !matches!(&callee.kind, ExprKind::Name(n) if n == PARALLEL_FOR) || args.len() != 3 {
        return;
    }
    let shared_inout = args[0].pass == Pass::Inout;
    let body = &args[2].expr;
    let body_inout = match &body.kind {
        ExprKind::Name(n) => world
            .functions
            .get(n)
            .and_then(|f| f.params.first())
            .is_some_and(|p| p.pass != Pass::In),
        _ => false,
    };
    if shared_inout || body_inout {
        let at = if shared_inout {
            &args[0].expr.span
        } else {
            &body.span
        };
        out.push(
            diagnostic(
                "E0824",
                "workers of 'parallel_for' share their data only by 'in'".into(),
                at,
            )
            .with_help(
                "pass the data as `in data` and update shared state through an `Atomic` cell inside it",
            ),
        );
        return;
    }
    if worker_body(world, body).is_none() {
        out.push(
            diagnostic(
                "E0823",
                "the body of 'parallel_for' must be a named function whose effects need no handler"
                    .into(),
                &body.span,
            )
            .with_help(
                "declare `fn body(in data: A, i: u64)` whose row holds only `Ffi[Lib]`, `Alloc` or capability effects, and pass `body`",
            ),
        );
    }
}

fn check_expr(types: &TypeMap, e: &Expr, out: &mut Vec<Diagnostic>) {
    let ty_of = |x: &Expr| types.get(&(x.span.start, x.span.end));
    match &e.kind {
        ExprKind::Struct { name, .. } if name == ATOMIC_TYPE => out.push(
            diagnostic(
                "E0822",
                "an 'Atomic' cell is built by 'Atomic::new'".into(),
                &e.span,
            )
            .with_help("write `Atomic::new(value)`"),
        ),
        ExprKind::Call { callee, args, .. } => {
            // `c.op(...)` or `Atomic::op(in c, ...)`.
            let (method, receiver, rest) = match &callee.kind {
                ExprKind::Field(base, m) if ty_of(base).and_then(cell_type).is_some() => {
                    (m.as_str(), ty_of(base), &args[..])
                }
                ExprKind::Name(n) if n.starts_with("Atomic::") => {
                    let m = &n["Atomic::".len()..];
                    if m == "new" {
                        (m, ty_of(e), &args[..])
                    } else {
                        (
                            m,
                            args.first().and_then(|a| ty_of(&a.expr)),
                            args.get(1..).unwrap_or(&[]),
                        )
                    }
                }
                _ => return,
            };
            let Some(cell) = receiver.and_then(cell_type) else {
                return;
            };
            let Some(kind) = cell_kind(cell) else {
                out.push(
                    diagnostic(
                        "E0821",
                        format!("'Atomic[{cell}]' has no atomic operations"),
                        &e.span,
                    )
                    .with_help("an atomic cell holds a `bool` or an integer kind"),
                );
                return;
            };
            let Some(op) = AtomicOp::of_method(method) else {
                return;
            };
            if kind.is_none() && !op.on_bool() {
                out.push(diagnostic(
                    "E0821",
                    format!("'{method}' is not defined on 'Atomic[bool]'"),
                    &e.span,
                ));
            }
            for (i, arg) in rest.iter().enumerate().skip(op.values()) {
                let Some(order) = order_name(&arg.expr) else {
                    out.push(
                        diagnostic(
                            "E0820",
                            format!("the memory order of '{method}' must be a literal 'MemoryOrder'"),
                            &arg.expr.span,
                        )
                        .with_help("write one of `MemoryOrder::Relaxed`, `Acquire`, `Release`, `AcqRel`, `SeqCst`"),
                    );
                    continue;
                };
                let loads = op == AtomicOp::Load
                    || (op == AtomicOp::CompareExchange && i == op.values() + 1);
                let refused = (loads && matches!(order, "Release" | "AcqRel"))
                    || (op == AtomicOp::Store && matches!(order, "Acquire" | "AcqRel"));
                if refused {
                    let what = if op == AtomicOp::CompareExchange {
                        "the failure order of 'compare_exchange'".to_string()
                    } else {
                        format!("'{method}'")
                    };
                    out.push(diagnostic(
                        "E0820",
                        format!("{what} cannot use 'MemoryOrder::{order}'"),
                        &arg.expr.span,
                    ));
                }
            }
        }
        _ => {}
    }
}

/// The private cell on Place IR, where every read and write names its
/// root's type: no function outside the prelude reads, writes or projects
/// `cell` of an `Atomic[T]` (`E0822`), so the cell always holds the one
/// element the native lowering dereferences.
pub fn check_program(world: &World, program: &crate::ir::Program) -> Vec<Diagnostic> {
    use crate::ir::Op;
    let mut out = vec![];
    if !world.atomic_prelude {
        return out;
    }
    for f in program.functions.values() {
        if is_prelude_function(world, &f.name) {
            continue;
        }
        let root_type = |root: &str| {
            f.locals
                .get(root)
                .or_else(|| f.params.iter().find(|p| p.name == root).map(|p| &p.ty))
        };
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            let private = match &i.op {
                Op::Read { root, path, .. } => {
                    path.first().is_some_and(|p| p == CELL)
                        && root_type(root).and_then(cell_type).is_some()
                }
                Op::Write { place, .. } => {
                    let mut path = place.split('.');
                    let root = path.next().unwrap_or_default();
                    path.next() == Some(CELL) && root_type(root).and_then(cell_type).is_some()
                }
                Op::Project(v, field) => {
                    field == CELL && f.values.get(v).and_then(cell_type).is_some()
                }
                _ => false,
            };
            if private {
                out.push(
                    diagnostic(
                        "E0822",
                        "the cell of an 'Atomic' is private to its operations".into(),
                        &i.span,
                    )
                    .with_help(
                        "use `load`, `store`, `swap`, `compare_exchange` or a `fetch_*` operation",
                    ),
                );
            }
        }
    }
    out
}
