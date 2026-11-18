//! Port gaps lane 6 (#989): inliner reach. Handled callees, unbounded
//! generic callees, `in Str` callees and the cost of already-inlined bodies.
//!
//! Every program runs through the reference evaluator (optimized and not),
//! the JIT with `optimize` on and off, and object emission for x86-64 and
//! AArch64 (see `tests/native_inline.rs`). IR-level checks name which calls
//! the inliner replaced, and why the others stayed calls.
use vesper::{
    check,
    ir_inline::{self, Budget, Refusal},
    native, parser,
    runtime::Runtime,
};

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

fn evaluate(world: &check::World, optimize: bool) -> String {
    let mut runtime = Runtime::new(world);
    runtime.optimize = optimize;
    runtime.run("main", vec![]).unwrap().to_string()
}

/// Every backend computes `expected` for `main`.
fn agree(source: &str, expected: &str) {
    let w = world(source);
    assert_eq!(evaluate(&w, true), expected, "evaluator");
    assert_eq!(evaluate(&w, false), expected, "unoptimized evaluator");
    for optimize in [true, false] {
        assert_eq!(
            native::jit_with_options(&w, "main", optimize)
                .unwrap_or_else(|e| panic!("JIT optimize={optimize}: {e}"))
                .to_string(),
            expected,
            "JIT optimize={optimize}"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for optimize in [true, false] {
            let (obj, _) = native::object_with_options(&w, "main", Some(target), optimize)
                .unwrap_or_else(|e| panic!("{target} optimize={optimize}: {e}"));
            assert_eq!(&obj[..4], b"\x7fELF");
        }
    }
}

/// Lower, optimize and inline `source` with the default budget, check the
/// token evidence, and return the report and program.
fn inlined(source: &str) -> (ir_inline::Report, vesper::ir::Program) {
    let w = world(source);
    let mut program = vesper::ir::lower_world(&w);
    vesper::ir_opt::optimize(&w, &mut program).unwrap();
    let report = ir_inline::inline(&w, &mut program, &Budget::default());
    let evidence = vesper::ir_check::evidence(&program);
    assert!(evidence.is_empty(), "{evidence:?}");
    (report, program)
}

fn calls(program: &vesper::ir::Program, caller: &str, callee: &str) -> usize {
    program.functions[caller]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(&i.op, vesper::ir::Op::Call { callee: c, .. } if c == callee))
        .count()
}

/// BBR's `div` and `pow2` before #950 rewrote them without handlers, and
/// Quarry's `util::div_rem`.
const HANDLED: &str = "fn div(a: u64, b: u64) -> u64 { handle Fail[RuntimeError] in { a / b } with { return v => v, raise(e, k) => 0, } } \
    fn pow2(n: u64) -> u64 { let one: u64 = 1; handle Fail[ArithOverflow] in { one << n } with { return v => v, raise(e, k) => 0, } } \
    fn div_rem(v: u64, d: u64) -> (u64, u64) { if d == 0 { return (0, 0); } handle Fail[RuntimeError] in { (v / d, v % d) } with { return q => q, raise(e, k) => (0, 0), } } \
    fn mix(a: u64, b: u64) -> u64 { let (q, r) = div_rem(a, b); wrapping_add(wrapping_add(div(a, b), pow2(b)), wrapping_add(q, r)) } \
    fn main() -> u64 { wrapping_add(wrapping_add(mix(100, 7), mix(5, 0)), wrapping_add(mix(9, 70), div(pow2(3), 0))) }";

#[test]
fn handled_callees_inline() {
    // mix(100, 7) = 14 + 128 + 14 + 2 = 158; mix(5, 0) = 0 + 1 + 0 = 1;
    // mix(9, 70) = 0 + 0 (shift overflow) + 0 + 9 = 9; div(8, 0) = 0.
    agree(HANDLED, "168");
    let (report, program) = inlined(HANDLED);
    for callee in ["div", "pow2", "div_rem"] {
        assert_eq!(calls(&program, "mix", callee), 0, "{callee}: {report:?}");
        assert!(!report.refused.contains_key(callee), "{report:?}");
    }
    // Three copies of `mix` would put nine handles into `main`, over
    // `Budget::caller_handles`.
    assert!(calls(&program, "main", "mix") > 0, "{report:?}");
    assert!(
        report.refused["mix"].contains_key(&Refusal::Growth),
        "{report:?}"
    );
}

/// A handler that captures the callee's parameters and keeps state, and a
/// handled callee inlined twice into one caller, each copy with its own
/// locals.
#[test]
fn handler_captures_and_state() {
    let source = "linear effect Tick { fn tick(n: i64) -> Unit; } \
        fn count(base: i64, k: i64) -> i64 { \
            handle Tick state t = base in { Tick::tick(k); Tick::tick(base); 3 } with { \
                return (v, t) => wrapping_add(wrapping_mul(v, 1000), t), tick(t, n, c) => c((), wrapping_add(t, n)), } } \
        fn main() -> i64 { wrapping_add(count(10, 1), count(20, 2)) }";
    // count(10, 1) = 3000 + 21; count(20, 2) = 3000 + 42.
    agree(source, "6063");
    let (report, program) = inlined(source);
    assert_eq!(calls(&program, "main", "count"), 0, "{report:?}");
}

/// A handled callee that borrows the caller's variable `inout`.
#[test]
fn handled_callee_with_inout_parameter() {
    let source = "fn bump(inout x: u64, d: u64) { x = wrapping_add(x, handle Fail[RuntimeError] in { 100 / d } with { return v => v, raise(e, k) => 7, }); } \
        fn main() -> u64 { let mut x: u64 = 1; bump(inout x, 3); bump(inout x, 0); x }";
    agree(source, "41");
    let (report, program) = inlined(source);
    assert_eq!(calls(&program, "main", "bump"), 0, "{report:?}");
}

/// `return` inside a handled body leaves the enclosing function, which an
/// inlined copy is not, so such a callee stays a call.
#[test]
fn return_inside_a_handler_stays_a_call() {
    let source = "fn first(a: u64, b: u64) -> u64 { handle Fail[RuntimeError] in { if b == 1 { return 5; } a / b } with { return v => v, raise(e, k) => 0, } } \
        fn main() -> u64 { wrapping_add(wrapping_add(first(9, 1), first(9, 3)), first(9, 0)) }";
    agree(source, "8");
    let (report, program) = inlined(source);
    assert_eq!(calls(&program, "main", "first"), 3);
    assert_eq!(
        report.refused["first"],
        [(Refusal::Callee("return inside a handler"), 3)].into()
    );
}

/// Unbounded generic callees inline as copies typed with the call's type
/// arguments: scalars, aggregates, sequences of `T` and a generic caller.
#[test]
fn unbounded_generic_callees_inline() {
    let source = "copy struct P { a: u64, b: u64 } \
        fn id[T](sink x: T) -> T { x } \
        fn pick[T: mod copy](c: bool, sink x: T, sink y: T) -> T { if c { x } else { y } } \
        fn some[T](sink x: T) -> Option[T] { Option::Some(x) } \
        fn first[T: mod copy](in s: Slice[T], sink d: T) -> T { match s.try_get(0) { Option::Some(v) => v, Option::None => d } } \
        fn twice[T: mod copy](sink x: T) -> (T, T) { (x, x) } \
        fn main() -> u64 { \
            let p = pick(false, P { a: 1, b: 2 }, P { a: 30, b: 40 }); \
            let q = match some(p) { Option::Some(v) => v.b, Option::None => 0 }; \
            let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); s.push(500); \
            let six: u64 = 6; let (x, y) = twice(id(six)); \
            wrapping_add(wrapping_add(wrapping_add(p.a, q), first(in s.as_slice(), 9)), wrapping_add(x, y)) }";
    // 30 + 40 + 500 + 12.
    agree(source, "582");
    let (report, program) = inlined(source);
    for callee in ["id", "pick", "some", "first", "twice"] {
        assert_eq!(calls(&program, "main", callee), 0, "{callee}: {report:?}");
    }
}

/// A generic callee that calls another generic function, or applies an
/// operator to a `T` value, keeps its call: the native backend would look
/// their types up by source span.
#[test]
fn generic_callees_that_need_span_types_stay_calls() {
    let source = "fn id[T](sink x: T) -> T { x } \
        fn wrap[T](sink x: T) -> T { id(x) } \
        fn main() -> i64 { wrapping_add(wrap(40), wrap(2)) }";
    agree(source, "42");
    let (report, program) = inlined(source);
    assert_eq!(calls(&program, "main", "wrap"), 2, "{report:?}");
    assert_eq!(
        report.refused["wrap"],
        [(
            Refusal::Callee("generic callee calls a generic function"),
            2
        )]
        .into()
    );
}

/// `in Str` callees (Quarry `util::bound`) inline for every kind of
/// argument: a literal, a `String` local, a field of a borrowed struct, and
/// a sub-view.
#[test]
fn str_callees_inline() {
    let source = "struct Q { err: u64, msg: String, name: String } \
        fn put(inout m: String, in s: Str) { m.push_str(s); } \
        fn bound(inout q: Q, n: u64, max: u64, in what: Str) -> bool { if n > max { if q.err == 0 { q.err = 6; put(inout q.msg, what); } false } else { true } } \
        fn width(in s: Str) -> u64 { wrapping_add(s.len(), match s.try_byte_at(0) { Option::Some(b) => widen_u64(b), Option::None => 0 }) } \
        fn run(inout q: Q) -> u64 { \
            let mut local = String::new(); local.push_str(\"local\"); \
            let a: u64 = if bound(inout q, 1, 2, \"ok\") { 1 } else { 0 }; \
            let b: u64 = if bound(inout q, 9, 2, \"row limit\") { 10 } else { 0 }; \
            wrapping_add(wrapping_add(a, b), wrapping_add(wrapping_add(width(in local), width(in q.name)), wrapping_add(width(\"\"), width(in local[1..3])))) } \
        fn main() -> u64 { \
            let mut name = String::new(); name.push_str(\"Ab\"); let mut q = Q { err: 0, msg: String::new(), name: name }; \
            let r = run(inout q); \
            wrapping_add(wrapping_mul(r, 1000), wrapping_add(wrapping_mul(q.err, 100), q.msg.len())) }";
    // run: a = 1, b = 0; width("local") = 5 + 108, width("Ab") = 2 + 65,
    // width("") = 0, width("oc") = 2 + 111: 1 + 293 = 294. err 6, msg 9.
    agree(source, "294609");
    let (report, program) = inlined(source);
    for callee in ["bound", "width"] {
        assert_eq!(calls(&program, "run", callee), 0, "{callee}: {report:?}");
    }
}

/// Ignis `plan_order.ves`: a derived `Ord::cmp` over five fields, called
/// from a bounded generic sort. Bounded generics are monomorphized
/// (`less$mono$Edge`), so what kept `cmp` a call was its cost once its field
/// compares were inlined into it: the forwarding blocks each inlined
/// `return` leaves, and the `Less`/`Equal`/`Greater` constants.
const EDGE_SORT: &str = "copy struct Edge { before: u32, after: u32, kind: u32, has_resource: u32, resource: u32 } \
    impl Ord for Edge {} \
    fn less[T: mod copy](a: T, b: T) -> bool where T: Ord { match a.cmp(b) { Less => true, _ => false } } \
    fn count_less[T: mod copy](in s: Slice[T], x: T) -> u64 where T: Ord { \
        let mut n: u64 = 0; \
        for i in 0..s.len() { match s.try_get(i) { Option::Some(y) => if less(y, x) { n = wrapping_add(n, 1); }, Option::None => (), } } \
        n } \
    fn main() -> u64 { \
        let mut s: ChunkSeq[Edge] = ChunkSeq::new[Edge](); \
        s.push(Edge { before: 1, after: 2, kind: 0, has_resource: 1, resource: 7 }); \
        s.push(Edge { before: 1, after: 2, kind: 0, has_resource: 1, resource: 9 }); \
        s.push(Edge { before: 0, after: 5, kind: 3, has_resource: 0, resource: 0 }); \
        s.push(Edge { before: 2, after: 0, kind: 0, has_resource: 0, resource: 0 }); \
        count_less(in s.as_slice(), Edge { before: 1, after: 2, kind: 0, has_resource: 1, resource: 8 }) }";

#[test]
fn derived_cmp_inlines_into_a_generic_sort() {
    agree(EDGE_SORT, "2");
    let (report, program) = inlined(EDGE_SORT);
    let caller = "count_less$mono$Edge";
    assert_eq!(calls(&program, caller, "less$mono$Edge"), 0, "{report:?}");
    assert!(
        report.inlined_into[caller].contains("impl$Ord.Edge$cmp"),
        "{report:?}"
    );
    let w = world(EDGE_SORT);
    let cmp = &program.functions["impl$Ord.Edge$cmp"];
    let cost = ir_inline::cost(&w, cmp);
    assert!(cost <= Budget::default().small_callee, "{cost}");
}

/// A function that calls itself from inside a handled body is recursive:
/// the handled body is lowered inside it (Codex review on #1007).
#[test]
fn recursion_through_a_handler_stays_a_call() {
    let source = "fn down(n: u64) -> u64 { if n == 0 { 0 } else { handle Fail[RuntimeError] in { wrapping_add(down(wrapping_sub(n, 1)), 10 / n) } with { return v => v, raise(e, k) => 0, } } } \
        fn main() -> u64 { down(4) }";
    // 10/1 + 10/2 + 10/3 + 10/4 = 10 + 5 + 3 + 2.
    agree(source, "20");
    let (report, program) = inlined(source);
    assert_eq!(calls(&program, "main", "down"), 1, "{report:?}");
    assert_eq!(report.refused["down"], [(Refusal::Recursive, 2)].into());
}

/// A handled callee is charged for its handler graphs, which the native
/// backend emits at every copy (Codex review on #1007).
#[test]
fn handled_bodies_count_against_the_budget() {
    let body = (0..60)
        .map(|i| format!("s = wrapping_add(s, wrapping_add(a / b, {i}));"))
        .collect::<String>();
    let source = format!(
        "fn big(a: u64, b: u64) -> u64 {{ handle Fail[RuntimeError] in {{ let mut s: u64 = 0; {body} s }} with {{ return v => v, raise(e, k) => 0, }} }} \
         fn main() -> u64 {{ wrapping_add(big(10, 2), big(10, 5)) }}"
    );
    let (report, program) = inlined(&source);
    assert_eq!(calls(&program, "main", "big"), 2, "{report:?}");
    assert_eq!(report.refused["big"], [(Refusal::Size, 2)].into());
}

/// `tests/integer_widths.rs` calls a doubly handled helper dozens of times
/// from one function; copying every handle made the JIT 4x slower, so a
/// caller takes at most `Budget::caller_handles` inlined handles.
#[test]
fn handled_copies_per_caller_are_capped() {
    let calls_src = (0..20)
        .map(|i| format!("t({i}, 3)"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "fn op(a: u8, b: u8) -> u8 eff [Fail[ArithOverflow], Fail[RuntimeError]] {{ a / b }} \
         fn t(a: u8, b: u8) -> u8 {{ handle Fail[RuntimeError] in {{ handle Fail[ArithOverflow] in {{ op(a, b) }} with {{ return v => v, raise(e, k) => 1, }} }} with {{ return v => v, raise(e, k) => 2, }} }} \
         fn main() -> ({}) {{ ({calls_src},) }}",
        vec!["u8"; 20].join(", ")
    );
    let (report, program) = inlined(&source);
    let cap = Budget::default().caller_handles;
    assert_eq!(report.callees["t"], cap / 2, "{report:?}");
    assert_eq!(calls(&program, "main", "t"), 20 - cap / 2);
}
