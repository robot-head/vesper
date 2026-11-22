//! Self-hosting SH4 lane 6 (#1280), part 1: `@vesper/codegen-scalar`'s
//! decisions against the Rust `Codegen` core of src/native.rs, over every
//! corpus world's program as the native path compiles it (optimize on:
//! after bounds hoisting; optimize off: after drop elaboration).
//!
//! Part 1 ports the parts of `ir_graph_at` and `expr_at` that read the IR
//! and decide, before any Cranelift instruction is emitted (emission waits
//! on lane 1's lent builder handles):
//!
//! - **Block layout**: the order blocks are lowered in (a topological
//!   order of the reached blocks, or a reverse postorder from the entry
//!   when they have a cycle, then the unreached blocks), the blocks on a
//!   cycle, and which blocks the walk seals itself.
//! - **Operators**: for every `Constant`, `Unary` and `Binary`, which arm
//!   of `expr_at` lowers it, the operand's integer kind and lane, and which
//!   run-time checks it needs (overflow, divisor, narrowing bounds, shift
//!   range), with the checked-arithmetic overflow test it uses.
//! - **Exits and terminators**: `Op::Exit`, `Return`, `Jump` (and which
//!   arguments are `Never` defaults), `Branch`, `Match`, `Unreachable`.
//!
//! [`decisions`] is the Rust half: it reads the same `ir::Program` with the
//! guards `expr_at`, `binary`, `checked_arithmetic`, `convert`, `shift` and
//! `bit_builtin` use, calling the compiler's own predicates
//! (`numeric::conversion`, `integer_widths::bit_builtin`, ...) and the
//! same petgraph calls as `ir_graph_at`. The Vesper package must print the
//! same text. Lane 1's CLIF and object differential replaces this with the
//! emitted code itself in part 2.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::OnceLock;
use vesper::ast::{Literal, Type};
use vesper::check::World;
use vesper::ir::{FunctionIr, Op, Program, Terminator};
use vesper::numeric::{Conversion, IntegerKind};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-scalar"))
}

/// Every `n`-th corpus world (`CODEGEN_SCALAR_STEP`, default 4, as for
/// tests/self_host_place_ir.rs); `CODEGEN_SCALAR_STEP=1` runs every world
/// (the report's numbers).
fn step() -> usize {
    std::env::var("CODEGEN_SCALAR_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

/// One program the native path compiles.
pub struct Stage {
    pub name: String,
    pub world: &'static World,
    pub program: Program,
}

/// Every corpus world's program as `native` lowers it: with optimize on,
/// after `ir_opt`, the inliner, drop elaboration and bounds hoisting; with
/// optimize off, after `fip_reuse_pass` and drop elaboration. A pass that
/// refuses a program drops it.
pub fn stages() -> &'static [Stage] {
    static S: OnceLock<Vec<Stage>> = OnceLock::new();
    S.get_or_init(|| {
        let mut out = vec![];
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        for c in corpus().iter().step_by(step()) {
            let w = &c.world;
            let r = std::panic::catch_unwind(|| {
                let mut v = vec![];
                let lowered = vesper::ffi_lowering::drops::lower_world(w);
                let mut p = lowered.clone();
                if vesper::ir_opt::optimize(w, &mut p).is_ok() {
                    vesper::ir_inline::inline(w, &mut p, &vesper::ir_inline::Budget::default());
                    vesper::fip::elaborate_non_fip_drops(w, &mut p);
                    vesper::bounds_hoist::run(&mut p);
                    v.push(("opt", p));
                }
                let mut q = lowered;
                if vesper::ir_opt::fip_reuse_pass(w, &mut q).is_ok() {
                    vesper::fip::elaborate_non_fip_drops(w, &mut q);
                    v.push(("unopt", q));
                }
                v
            });
            if let Ok(v) = r {
                for (stage, program) in v {
                    out.push(Stage {
                        name: format!("{}@{stage}", c.name),
                        world: w,
                        program,
                    });
                }
            }
        }
        std::panic::set_hook(quiet);
        out
    })
}

// ------------------------------------------------------------ Rust half

/// `Codegen::ty` of an expression the IR walk builds at `span` (not a
/// `$ir_value` name): the checker's type there, integer literals defaulted.
fn span_type(w: &World, span: &vesper::ast::Span) -> Type {
    vesper::numeric::default_type(w.types.get(&(span.start, span.end)).unwrap_or(&Type::Infer))
}

/// The Cranelift lane of a value of IR type `t` (`native::scalar` of the
/// defaulted type; aggregates are pointers, one word).
fn lane(t: &Type) -> &'static str {
    match vesper::numeric::default_type(t).name() {
        "f32" => "f32",
        "f64" => "f64",
        n if vesper::numeric::is_wide_name(n) => "i128",
        _ => "i64",
    }
}

fn float_lane(l: &str) -> bool {
    matches!(l, "f32" | "f64")
}

fn kind_name(k: Option<IntegerKind>) -> &'static str {
    k.map_or("-", IntegerKind::name)
}

/// `numeric::is_float_intrinsic`, `math_intrinsic` and friends.
fn is_float_intrinsic(op: &str) -> bool {
    vesper::numeric::float::is_float_intrinsic(op)
}

/// The overflow test `checked_arithmetic` emits for `op` on `kind`.
fn checked_test(op: &str, kind: IntegerKind) -> &'static str {
    use IntegerKind as K;
    if kind.wide() {
        return "wide";
    }
    match (op, kind) {
        ("+" | "-" | "*", K::I8 | K::I16 | K::I32) => "narrow-signed",
        ("/", K::I8 | K::I16 | K::I32) => "div-min",
        ("+" | "*", K::U8 | K::U16 | K::U32) => "narrow-unsigned",
        ("-", K::U8 | K::U16 | K::U32 | K::U64 | K::Usize) => "borrow",
        ("+", K::U64 | K::Usize) => "carry",
        ("*", K::U64 | K::Usize) => "umulhi",
        ("+" | "-", K::I64 | K::Isize) => "sign",
        ("*", K::I64 | K::Isize) => "smulhi",
        ("/", K::I64 | K::Isize) => "div-min",
        _ => "none",
    }
}

/// `Codegen::binary`'s instruction for `op` on lanes `a`, `b`.
fn binary(op: &str, a: &str, b: &str, unsigned: bool) -> String {
    if a != b {
        return "error mismatch".into();
    }
    let float = float_lane(a);
    if matches!(op, "/" | "%") && a == "i128" {
        return format!("wide-divide {}", if unsigned { "u" } else { "s" });
    }
    if matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=") {
        return if float {
            format!("fcmp {op}")
        } else if unsigned && matches!(op, "<" | "<=" | ">" | ">=") {
            format!("icmp u{op}")
        } else {
            format!("icmp {op}")
        };
    }
    match (op, float) {
        ("+" | "wrapping_add", false) => "iadd",
        ("-" | "wrapping_sub", false) => "isub",
        ("*" | "wrapping_mul", false) => "imul",
        ("/", false) if unsigned => "udiv",
        ("/", false) => "sdiv-guarded",
        ("%", false) if unsigned => "urem",
        ("%", false) => "srem",
        ("+", true) => "fadd",
        ("-", true) => "fsub",
        ("*", true) => "fmul",
        ("/", true) => "fdiv",
        ("%", true) => "fmod",
        _ => "error operator",
    }
    .into()
}

/// The `expr_at` arm for `Op::Unary(op, v)`.
fn unary(w: &World, f: &FunctionIr, op: &str, v: u32, span: &vesper::ast::Span) -> String {
    let Some(tv) = f.values.get(&v) else {
        return "error untyped".into();
    };
    if op == "~" {
        return "bnot".into();
    }
    if is_float_intrinsic(op)
        || (vesper::numeric::conversion(op).is_some()
            && vesper::numeric::FloatKind::from_type(tv).is_some())
    {
        return "float-unary".into();
    }
    if let Some((bop, kind)) = vesper::integer_widths::bit_builtin(op) {
        return format!("bit {bop:?} {}", kind.name());
    }
    let overflow = w.overflow_ops.contains(&(span.start, span.end));
    if let Some((conversion, to)) = vesper::numeric::conversion(op) {
        let Some(from) = IntegerKind::from_type(tv) else {
            return "error convert".into();
        };
        if from.wide() || to.wide() {
            return format!(
                "convert wide {conversion:?} {} {}{}",
                from.name(),
                to.name(),
                if overflow { " checked" } else { "" }
            );
        }
        return match conversion {
            Conversion::Widen => "convert widen".into(),
            Conversion::Wrapping => format!("convert wrapping {}", to.name()),
            Conversion::Narrow => {
                let mut s = format!("convert narrow {} {}", from.name(), to.name());
                if overflow {
                    let (low, high) = if from.unsigned() {
                        (false, to.max() < from.max())
                    } else {
                        (to.min() > from.min(), to.max() < from.max())
                    };
                    if low {
                        s.push_str(" low");
                    }
                    if high {
                        s.push_str(" high");
                    }
                }
                s
            }
        };
    }
    let kind = IntegerKind::from_type(tv);
    let float = float_lane(lane(tv));
    let mut check = "";
    if op == "-"
        && let Some(kind) = kind
        && overflow
    {
        check = if kind.unsigned() {
            " check=nonzero"
        } else if kind.wide() {
            " check=min128"
        } else {
            " check=min"
        };
    }
    let r = match (op, float) {
        ("-", false) => "ineg",
        ("-", true) => "fneg",
        ("!", false) => "not",
        (vesper::capability_effects::WIDEN_INTRINSIC, false) => "widen",
        _ => return "error unary".into(),
    };
    format!("{r} {}{check}", kind_name(kind))
}

/// The `expr_at` arm for `Op::Binary(op, a, b)`.
fn binary_op(
    w: &World,
    f: &FunctionIr,
    op: &str,
    a: u32,
    b: u32,
    span: &vesper::ast::Span,
) -> String {
    let (Some(ta), Some(tb)) = (f.values.get(&a), f.values.get(&b)) else {
        return "error untyped".into();
    };
    if vesper::numeric::float::math_intrinsic(op).is_some() {
        return "float-math".into();
    }
    if let Some((bop, kind)) = vesper::integer_widths::bit_builtin(op) {
        return format!("bit {bop:?} {}", kind.name());
    }
    if op == "&&" || op == "||" {
        return "logic".into();
    }
    let overflow = w.overflow_ops.contains(&(span.start, span.end));
    if vesper::numeric::is_shift(op) || vesper::numeric::rotate_intrinsic(op).is_some() {
        let Some(kind) = IntegerKind::from_type(ta) else {
            return "error shift".into();
        };
        let range = if vesper::numeric::is_shift(op) && overflow {
            " range"
        } else {
            ""
        };
        let how = match op {
            "<<" => "ishl",
            ">>" if kind.unsigned() => "ushr",
            ">>" => "sshr",
            _ if vesper::numeric::rotate_intrinsic(op) == Some(true) => "rotl",
            _ => "rotr",
        };
        return format!("shift {how} {}{range}", kind.name());
    }
    if matches!(op, "&" | "|" | "^") {
        return format!("bitop {op}");
    }
    if vesper::strings::is_comparison(op)
        && (vesper::strings::is_text(ta) || vesper::strings::is_text(tb))
    {
        return format!("str-compare {op}");
    }
    if matches!(op, "==" | "!=") {
        let aggregate = match vesper::layout::Layout::of(w, ta) {
            Ok(l) => l.aggregate(),
            Err(_) => return "error layout".into(),
        };
        if aggregate || matches!(ta, Type::Function { .. }) || vesper::fip::is_box_type(w, ta) {
            return format!("equal {op}");
        }
    }
    let kind = IntegerKind::from_type(ta);
    let unsigned = kind.is_some_and(|k| k.unsigned());
    let mut s = String::new();
    if matches!(op, "/" | "%") && w.partial_ops.contains(&(span.start, span.end)) {
        s.push_str("divisor ");
    }
    let (la, lb) = (lane(ta), lane(tb));
    if let Some(kind) = kind
        && overflow
    {
        let _ = write!(
            s,
            "checked {} {} {}",
            kind.name(),
            checked_test(op, kind),
            binary(op, la, lb, kind.unsigned())
        );
    } else if let Some(bit) = vesper::capability_effects::bit_intrinsic(op) {
        if kind.is_none() {
            s.push_str("error bitwise-kind");
        } else if la != "i64" || lb != "i64" {
            s.push_str("error bitwise-lane");
        } else {
            let _ = write!(s, "bitwise {bit} {}", kind_name(kind));
        }
    } else {
        s.push_str(&binary(op, la, lb, unsigned));
    }
    s
}

/// The block order, cycles and seals of `ir_graph_at`, with its petgraph
/// calls.
fn layout(f: &FunctionIr, out: &mut String) {
    let mut graph = petgraph::graphmap::DiGraphMap::<usize, ()>::new();
    for id in 0..f.blocks.len() {
        graph.add_node(id);
    }
    for (id, block) in f.blocks.iter().enumerate() {
        let targets = match &block.terminator {
            Terminator::Jump { block, .. } => vec![*block],
            Terminator::Branch { yes, no, .. } => vec![*yes, *no],
            Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
            _ => vec![],
        };
        for target in targets {
            graph.add_edge(id, target, ());
        }
    }
    let mut postorder = vec![];
    let mut dfs = petgraph::visit::DfsPostOrder::new(&graph, 0);
    while let Some(n) = dfs.next(&graph) {
        postorder.push(n);
    }
    let reached = postorder.iter().copied().collect::<BTreeSet<_>>();
    let unreached = (0..f.blocks.len()).filter(|n| !reached.contains(n));
    let mut live = graph.clone();
    for n in unreached.clone() {
        live.remove_node(n);
    }
    let mut order = petgraph::algo::toposort(&live, None).unwrap_or_else(|_| {
        postorder.reverse();
        postorder
    });
    order.extend(unreached);
    let cyclic = petgraph::algo::tarjan_scc(&live)
        .into_iter()
        .filter(|c| c.len() > 1 || live.contains_edge(c[0], c[0]))
        .flatten()
        .collect::<BTreeSet<_>>();
    let mut lowered = vec![false; f.blocks.len()];
    let mut sealed = vec![];
    for &i in &order {
        if i != 0
            && !cyclic.contains(&i)
            && graph
                .neighbors_directed(i, petgraph::Direction::Incoming)
                .all(|p| lowered[p])
        {
            sealed.push(i);
        }
        lowered[i] = true;
    }
    let list = |v: &mut dyn Iterator<Item = usize>| v.map(|n| format!(" {n}")).collect::<String>();
    let _ = writeln!(out, "order{}", list(&mut order.iter().copied()));
    let _ = writeln!(out, "cyclic{}", list(&mut cyclic.iter().copied()));
    let _ = writeln!(out, "sealed{}", list(&mut sealed.iter().copied()));
}

fn never(f: &FunctionIr, v: u32) -> bool {
    f.values.get(&v).is_some_and(|t| t.name() == "Never")
}

/// One function's decisions.
fn function(w: &World, f: &FunctionIr, out: &mut String) {
    let _ = writeln!(out, "fn {}", f.name);
    layout(f, out);
    for (bi, block) in f.blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            let d = match &inst.op {
                Op::Constant(l) => match l {
                    Literal::Int(_) => {
                        let t = span_type(w, &inst.span);
                        if !matches!(t, Type::IntLiteral(_))
                            && IntegerKind::from_type(&t).is_some_and(IntegerKind::wide)
                        {
                            "const wide".to_string()
                        } else {
                            "const int".to_string()
                        }
                    }
                    Literal::Float(_) if span_type(w, &inst.span).name() == "f32" => {
                        "const f32".into()
                    }
                    Literal::Float(_) => "const f64".into(),
                    Literal::Bool(_) => "const bool".into(),
                    Literal::String(_) => "const string".into(),
                    // A literal above i128::MAX (#1244) is always u128.
                    Literal::WideInt(_) => "const wide".into(),
                },
                Op::Unary(op, v) => format!("unary {op} {}", unary(w, f, op, *v, &inst.span)),
                Op::Binary(op, a, b) => {
                    format!("binary {op} {}", binary_op(w, f, op, *a, *b, &inst.span))
                }
                Op::Loop => "loop".into(),
                Op::Exit { value, owner } => {
                    format!(
                        "exit {owner}{}",
                        if never(f, *value) { " never" } else { "" }
                    )
                }
                _ => continue,
            };
            let _ = writeln!(out, "{bi}.{ii} %{} {d}", inst.dest);
        }
        let t = match &block.terminator {
            Terminator::Return(v) => format!("return{}", if never(f, *v) { " never" } else { "" }),
            Terminator::Jump { block, args } => {
                let mut s = format!("jump {block}");
                for (k, a) in args.iter().enumerate() {
                    if never(f, *a) {
                        let _ = write!(s, " never{k}");
                    }
                }
                s
            }
            Terminator::Branch { yes, no, .. } => format!("branch {yes} {no}"),
            Terminator::Match { arms, .. } => {
                let mut s = format!("match {}", arms.len());
                for (_, b) in arms {
                    let _ = write!(s, " {b}");
                }
                s
            }
            Terminator::Unreachable => "trap".into(),
        };
        let _ = writeln!(out, "{bi} {t}");
    }
}

/// The decisions of every function of `p`, in name order.
pub fn decisions(w: &World, p: &Program) -> String {
    let mut out = String::new();
    for f in p.functions.values() {
        function(w, f, &mut out);
    }
    out
}

// ---------------------------------------------------------------- tests

/// Counts each decision of a dump by its first two words, so the report
/// can say what a run exercises.
fn count(dump: &str, seen: &mut std::collections::BTreeMap<String, usize>) {
    for l in dump.lines() {
        let mut words = l.split(' ');
        let first = words.next().unwrap_or("");
        let key = if first.contains('.') {
            // `b.i %dest kind op decision...`
            let rest: Vec<&str> = words.skip(1).collect();
            match rest.as_slice() {
                [k @ ("unary" | "binary"), _, d, ..] => format!("{k} {d}"),
                ["exit", ..] => "exit".to_string(),
                [k, d, ..] => format!("{k} {d}"),
                [k] => k.to_string(),
                [] => String::new(),
            }
        } else if first.chars().all(|c| c.is_ascii_digit()) {
            format!("term {}", words.next().unwrap_or(""))
        } else {
            continue;
        };
        *seen.entry(key).or_default() += 1;
    }
}

#[test]
fn vesper_decisions_match_rust() {
    let entry = package().entry("codegen_scalar_decisions", true);
    let (mut bad, mut skipped, mut fns, mut lines) = (0, 0, 0, 0);
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for s in stages() {
        let Some((input, _)) = ir_words::program_input(s.world, &s.program) else {
            skipped += 1;
            continue;
        };
        let r = decisions(s.world, &s.program);
        fns += s.program.functions.len();
        lines += r.lines().count();
        count(&r, &mut seen);
        if let Some(d) = first_difference(&r, &entry.text(&input)) {
            bad += 1;
            if bad <= 5 {
                eprintln!("decisions: {} differs at {d}", s.name);
            }
        }
    }
    eprintln!(
        "decisions: {bad} of {} programs differ ({fns} functions, {lines} lines), \
         {skipped} not encodable",
        stages().len()
    );
    for (k, n) in &seen {
        eprintln!("  {n:>8}  {k}");
    }
    assert!(stages().len() > 10, "only {} programs", stages().len());
    assert_eq!(bad, 0);
}

/// Operator names an operator mutant cycles through: every family of
/// `expr_at`'s arms, with names that are none of them.
const UNARY_NAMES: [&str; 18] = [
    "~",
    "!",
    "-",
    "widen_u64",
    "narrow_u8",
    "narrow_i32",
    "wrapping_u16",
    "widen_i128",
    "narrow_u128",
    "narrow_usize",
    "leading_zeros_u32",
    "count_ones_i128",
    "wrapping_neg_i8",
    "sqrt",
    "nearest_f32",
    "saturating_u8",
    "to_bits_f64",
    "unknown_op",
];
const BINARY_NAMES: [&str; 30] = [
    "+",
    "-",
    "*",
    "/",
    "%",
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "&&",
    "||",
    "<<",
    ">>",
    "&",
    "|",
    "^",
    "rotate_left",
    "rotate_right",
    "bit_and",
    "bit_shl",
    "bit_shr",
    "wrapping_add",
    "wrapping_mul",
    "min",
    "fma",
    "wrapping_shl_u16",
    "wrapping_shr_i64",
    "unknown_op",
];
const SCALAR_NAMES: [&str; 16] = [
    "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize", "i128", "u128", "f32",
    "f64", "String", "bool",
];

/// Mutant `m` of a program: every operator renamed and every scalar value
/// retyped, round robin, so the decisions meet every arm, kind and lane
/// (the corpus has no `i128`, bit builtins or rotations). The mutants
/// need not type check: the decisions read only names and types.
fn operator_mutant(p: &Program, m: usize) -> Program {
    let mut p = p.clone();
    for f in p.functions.values_mut() {
        for (v, t) in f.values.iter_mut() {
            if let Type::Named(n, args) = t
                && args.is_empty()
                && (vesper::numeric::is_scalar_name(n) || n == "String")
                && n != "Unit"
                && n != "Never"
            {
                *n = SCALAR_NAMES[(*v as usize + m) % SCALAR_NAMES.len()].into();
            }
        }
        let mut j = m;
        for b in &mut f.blocks {
            for i in &mut b.instructions {
                match &mut i.op {
                    Op::Unary(op, _) => *op = UNARY_NAMES[j % UNARY_NAMES.len()].into(),
                    Op::Binary(op, _, _) => *op = BINARY_NAMES[j % BINARY_NAMES.len()].into(),
                    _ => continue,
                }
                j += 1;
            }
        }
    }
    p
}

#[test]
fn vesper_decisions_match_rust_on_operator_mutants() {
    let entry = package().entry("codegen_scalar_decisions", true);
    let (mut bad, mut programs, mut lines) = (0, 0, 0);
    let mut errors = 0;
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for s in stages().iter().step_by(4) {
        for m in 1..=3 {
            let p = operator_mutant(&s.program, m);
            let Some((input, _)) = ir_words::program_input(s.world, &p) else {
                continue;
            };
            programs += 1;
            let r = decisions(s.world, &p);
            lines += r.lines().count();
            errors += r.matches(" error ").count();
            count(&r, &mut seen);
            if let Some(d) = first_difference(&r, &entry.text(&input)) {
                bad += 1;
                if bad <= 5 {
                    eprintln!("mutant decisions: {}#m{m} differs at {d}", s.name);
                }
            }
        }
    }
    eprintln!(
        "mutant decisions: {bad} of {programs} programs differ ({lines} lines, {errors} errors)"
    );
    for (k, n) in &seen {
        eprintln!("  {n:>8}  {k}");
    }
    assert!(programs > 3, "only {programs} programs");
    assert_eq!(bad, 0);
}

/// The decisions of every corpus program, Rust and Vesper, best of five
/// (the encoding is timed separately and excluded, as SH3 measures passes).
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn codegen_scalar_decision_cost() {
    if !speed() {
        return;
    }
    let entry = package().entry("codegen_scalar_decisions", true);
    let inputs: Vec<Vec<u8>> = stages()
        .iter()
        .filter_map(|s| ir_words::program_input(s.world, &s.program).map(|(i, _)| i))
        .collect();
    let best = |f: &mut dyn FnMut()| (0..5).map(|_| time(&mut *f)).min().unwrap();
    let rust = best(&mut || {
        for s in stages() {
            std::hint::black_box(decisions(s.world, &s.program));
        }
    });
    let vesper = best(&mut || {
        for i in &inputs {
            std::hint::black_box(entry.words(i));
        }
    });
    eprintln!(
        "codegen-scalar decisions over {} programs: Rust {rust:?}, Vesper {vesper:?} ({:.2}x)",
        inputs.len(),
        vesper.as_secs_f64() / rust.as_secs_f64()
    );
}
