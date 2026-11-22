//! Self-hosting SH4 lane 11 (#1285), part 2: the `numeric` family emitted
//! by `@vesper/codegen-numeric` through the `numeric` hook of
//! `vesper::codegen_swap` (lane 1, #1275):
//!
//! - the vector operations (`Op::Vector`, `Codegen::vector_op` of
//!   src/native_vector.rs), over a generated corpus of every lanes
//!   intrinsic on every element type and shape;
//! - the `TiledSeq` operations (`Codegen::tiled_op` of
//!   src/tiled/native.rs, the `numeric::TILED` request), over the
//!   programs of tests/tiled.rs and a column-type corpus.
//!
//! Each program is compiled by the native backend with no hook and with
//! the Vesper hook, with the optimizer on and off; every function's CLIF
//! text, the object bytes and the JIT result must be equal.
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use vesper::codegen_swap::{self, CodegenHooks, INSTRUCTION, Lent};
use vesper::native::WordFunction;
use vesper::vector::{Elem, Shape};
use vesper_codegen_swap_test::*;

fn repo() -> std::path::PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for base in [manifest.join("../.."), std::path::PathBuf::from(".")] {
        if base
            .join("packages/vesper-codegen-numeric/src/vector.ves")
            .exists()
        {
            return base;
        }
    }
    panic!("cannot find packages/vesper-codegen-numeric");
}

/// The hook, JIT-compiled once with no hook in effect (a hook compiled
/// under `with_hooks` would call itself while its `OnceLock` initializes).
fn hook() -> &'static WordFunction {
    static HOOK: OnceLock<WordFunction> = OnceLock::new();
    HOOK.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), "packages/vesper-codegen-numeric");
            hook_function(&checked, "numeric_hook")
        })
    })
}

// ------------------------------------------------------------- the adapter

/// The `numeric` hook: the family's requests (`codegen_swap::numeric`) go
/// to the Vesper emission; [`INSTRUCTION`] requests are declined, so the
/// Rust code dispatches the operation and sends its typed request.
fn numeric(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    if lent.op == INSTRUCTION {
        return Ok(false);
    }
    let words = lent.args.to_vec();
    call_vesper(lent, lent.op, &words)
}

/// Runs request `request` with `words` through the Vesper hook.
fn call_vesper(lent: &mut Lent<'_, '_>, request: u32, words: &[u64]) -> Result<bool, String> {
    let f = hook();
    let (answer, crossing) = vesper_cranelift_sys::lend(
        lent.builder,
        &mut *lent.module,
        request,
        words,
        // SAFETY: the hook takes one word, the lent call's address.
        |call| unsafe { f.call(&[call]) },
    );
    let answer = answer?;
    if let Some(e) = crossing.error {
        return Err(format!("numeric hook: {e}"));
    }
    if crossing.unreturned {
        return Err("numeric hook kept a lent handle".into());
    }
    if answer == 1 {
        HANDLED.fetch_add(1, Ordering::Relaxed);
        lent.results.extend(crossing.results);
        Ok(true)
    } else {
        DECLINED.fetch_add(1, Ordering::Relaxed);
        Ok(false)
    }
}

fn hooks() -> CodegenHooks {
    CodegenHooks {
        numeric: Some(numeric),
        ..CodegenHooks::default()
    }
}

// --------------------------------------------------------------- the corpus

const ELEMS: [Elem; 6] = [
    Elem::U8,
    Elem::U32,
    Elem::U64,
    Elem::I64,
    Elem::F32,
    Elem::F64,
];

fn literal(e: Elem, i: usize) -> String {
    if e.is_float() {
        format!("{}.5", i + 1)
    } else if e == Elem::U8 {
        format!("{}", (i * 37 + 3) % 251)
    } else {
        format!("{}", i * 1_000_003 + 7)
    }
}

/// `Vec::from[T, N](...)` with distinct lanes.
fn vector(e: Elem, n: u16, salt: usize) -> String {
    let lanes: Vec<String> = (0..n as usize).map(|i| literal(e, i * 3 + salt)).collect();
    format!("Vec::from[{}, {n}]({})", e.name(), lanes.join(", "))
}

/// One program per operation and shape: `k` applies the operation to its
/// parameters (so nothing is folded), `main` calls it.
fn programs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for e in ELEMS {
        let t = e.name();
        for bits in [128u16, 256, 512] {
            let n = bits / e.bits() as u16;
            let Some(shape) = Shape::new(e, n) else {
                continue;
            };
            let profile = shape.required_profile().name();
            let v = format!("Vec[{t}, {n}]");
            let last = n - 1;
            let x = literal(e, 5);
            // (name, result type, body over a, b, c and the scalar x)
            let mut ops: Vec<(String, String, String)> = Vec::new();
            let lane = |expr: &str| format!("Vec::extract({expr}, {last})");
            let bits_of = |expr: &str| format!("Mask::bitmask({expr})");
            ops.push((
                "splat".into(),
                t.into(),
                lane(&format!("Vec::splat[{t}, {n}](x)")),
            ));
            ops.push(("from".into(), t.into(), lane(&vector(e, n, 1))));
            for op in [
                "add",
                "sub",
                "mul",
                "div",
                "min",
                "max",
                "and",
                "or",
                "xor",
                "saturating_add",
                "saturating_sub",
            ] {
                ops.push((op.into(), t.into(), lane(&format!("Vec::{op}(a, b)"))));
            }
            for op in ["not", "sqrt", "abs", "neg"] {
                ops.push((op.into(), t.into(), lane(&format!("Vec::{op}(a)"))));
            }
            ops.push(("mul_add".into(), t.into(), lane("Vec::mul_add(a, b, c)")));
            for op in ["to_bits", "from_bits", "widen_f64", "nearest_f32"] {
                ops.push((
                    op.into(),
                    "u64".into(),
                    format!("Mask::count(Vec::eq(Vec::{op}(a), Vec::{op}(b)))"),
                ));
            }
            for op in ["shl", "shr"] {
                ops.push((op.into(), t.into(), lane(&format!("Vec::{op}(a, 3)"))));
            }
            for op in ["eq", "ne", "lt", "le", "gt", "ge"] {
                ops.push((
                    op.into(),
                    "u64".into(),
                    bits_of(&format!("Vec::{op}(a, b)")),
                ));
            }
            ops.push((
                "select".into(),
                t.into(),
                lane("Vec::select(Vec::lt(a, b), a, c)"),
            ));
            for op in ["and", "or", "xor"] {
                ops.push((
                    format!("mask_{op}"),
                    "u64".into(),
                    bits_of(&format!("Mask::{op}(Vec::lt(a, b), Vec::lt(c, a))")),
                ));
            }
            ops.push((
                "mask_not".into(),
                "u64".into(),
                bits_of("Mask::not(Vec::lt(a, b))"),
            ));
            for op in ["count", "any", "all"] {
                let r = if op == "count" { "u64" } else { "bool" };
                ops.push((
                    format!("mask_{op}"),
                    r.into(),
                    format!("Mask::{op}(Vec::lt(a, b))"),
                ));
            }
            for op in ["add", "xor", "or", "and", "min", "max"] {
                ops.push((
                    format!("reduce_{op}"),
                    t.into(),
                    format!("Vec::reduce_{op}(a)"),
                ));
            }
            ops.push((
                "extract".into(),
                t.into(),
                format!("Vec::extract(a, {})", n / 2),
            ));
            ops.push((
                "replace".into(),
                t.into(),
                lane(&format!("Vec::replace(a, {last}, x)")),
            ));
            let reversed: Vec<String> = (0..n).rev().map(|i| i.to_string()).collect();
            ops.push((
                "shuffle".into(),
                t.into(),
                lane(&format!("Vec::shuffle(a, {})", reversed.join(", "))),
            ));
            ops.push((
                "load_masked".into(),
                t.into(),
                lane(&format!(
                    "Vec::load_masked[{t}, {n}](\"needle in a haystack, twice over and more\", 3)"
                )),
            ));
            ops.push((
                "in_bounds".into(),
                "u64".into(),
                bits_of(&format!(
                    "Mask::in_bounds[{t}, {n}](\"needle in a haystack\", 5)"
                )),
            ));
            ops.push((
                "active".into(),
                "u64".into(),
                bits_of(&format!("Mask::active[{t}, {n}](2, {})", n / 2 + 1)),
            ));
            for (name, result, body) in ops {
                // `main` handles the profile by checked dispatch, so a
                // host without it runs the fallback.
                let fallback = match result.as_str() {
                    "bool" => "false",
                    "f32" | "f64" => "0.0",
                    _ => "0",
                };
                let source = format!(
                    "fn k(a: {v}, b: {v}, c: {v}, x: {t}) -> {result} eff [Isa[{profile}]] {{ {body} }}\n\
                     fn main() -> {result} {{ if Isa::supported[{profile}]() {{ k({}, {}, {}, {x}) }} else {{ {fallback} }} }}\n",
                    vector(e, n, 0),
                    vector(e, n, 2),
                    vector(e, n, 4),
                );
                out.push((format!("{t}x{n} {name}"), source));
            }
        }
    }
    // `widen_f64` doubles the vector's bits, so its programs need the
    // profile of the result, which the template above does not give.
    for (n, profile) in [(4u16, "Avx2"), (8, "Avx512")] {
        let v = format!("Vec[f32, {n}]");
        out.push((
            format!("f32x{n} widen_f64"),
            format!(
                "fn k(a: {v}, b: {v}) -> f64 eff [Isa[{profile}]] {{ let w = Vec::widen_f64(a); Vec::extract(w, {}) + Vec::extract(w, 1) + Vec::reduce_add(Vec::widen_f64(b)) }}\n\
                 fn main() -> f64 {{ if Isa::supported[{profile}]() {{ k({}, {}) }} else {{ 0.0 }} }}\n",
                n - 1,
                vector(Elem::F32, n, 0),
                vector(Elem::F32, n, 2),
            ),
        ));
    }
    // `Isa::supported` of every profile.
    for p in ["Baseline128", "Avx2", "Avx512", "Sve2", "Rvv1"] {
        out.push((
            format!("isa {p}"),
            format!("fn main() -> u64 {{ if Isa::supported[{p}]() {{ 1 }} else {{ 2 }} }}\n"),
        ));
    }
    out
}

fn accepted(source: &str) -> Option<vesper::check::World> {
    let parsed = vesper::parser::parse(source);
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let c = vesper::check::check(&parsed.module);
    c.diagnostics.is_empty().then_some(c.world)
}

/// The operations the corpus must compile for at least one shape.
const EXPECTED: [&str; 55] = [
    "Avx2",
    "Avx512",
    "Baseline128",
    "Rvv1",
    "Sve2",
    "abs",
    "active",
    "add",
    "and",
    "div",
    "eq",
    "extract",
    "from",
    "from_bits",
    "ge",
    "gt",
    "in_bounds",
    "le",
    "load_masked",
    "lt",
    "mask_all",
    "mask_and",
    "mask_any",
    "mask_count",
    "mask_not",
    "mask_or",
    "mask_xor",
    "max",
    "min",
    "mul",
    "mul_add",
    "ne",
    "nearest_f32",
    "neg",
    "not",
    "or",
    "reduce_add",
    "reduce_and",
    "reduce_max",
    "reduce_min",
    "reduce_or",
    "reduce_xor",
    "replace",
    "saturating_add",
    "saturating_sub",
    "select",
    "shl",
    "shr",
    "shuffle",
    "splat",
    "sqrt",
    "sub",
    "to_bits",
    "widen_f64",
    "xor",
];

/// The operation part of each program name (`u8x16 add` is `add`).
fn programs_names(names: &[String]) -> std::collections::BTreeSet<&str> {
    names
        .iter()
        .filter_map(|n| n.split_once(' ').map(|(_, op)| op))
        .collect()
}

#[test]
fn vector_operations_match_the_rust_lowering() {
    let _ = hook();
    let mut diff = Diff::default();
    let mut compiled = 0;
    let mut skipped = Vec::new();
    let mut compiled_names: Vec<String> = Vec::new();
    let handled_before = HANDLED.load(Ordering::Relaxed);
    for (name, source) in programs() {
        let Some(world) = accepted(&source) else {
            skipped.push(name);
            continue;
        };
        compiled += 1;
        compiled_names.push(name.clone());
        for optimize in [false, true] {
            diff.add(compare(&name, &world, "main", hooks(), optimize, true));
        }
    }
    let handled = HANDLED.load(Ordering::Relaxed) - handled_before;
    eprintln!(
        "vector differential: {compiled} programs ({} rejected by the checker), {}; {handled} requests from Vesper, {} declined",
        skipped.len(),
        diff.summary(),
        DECLINED.load(Ordering::Relaxed)
    );
    // Every operation must be compiled for at least one shape.
    let ops: std::collections::BTreeSet<&str> = programs_names(&compiled_names);
    eprintln!("operations compiled: {ops:?}");
    for op in EXPECTED {
        assert!(ops.contains(op), "no program compiled {op}");
    }
    assert!(diff.is_clean(), "{}", diff.summary());
    assert!(
        compiled > 300,
        "only {compiled} programs checked: {skipped:?}"
    );
    assert!(handled > 0);
}

// ------------------------------------------------------------ TiledSeq

/// The raw-string programs with a `main` in tests/tiled.rs.
fn tiled_rs_programs() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(repo().join("tests/tiled.rs")).unwrap();
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("r#\"") {
        let after = &rest[start + 3..];
        let Some(end) = after.find("\"#") else {
            break;
        };
        let body = &after[..end];
        if body.contains("fn main") {
            out.push((format!("tiled.rs #{}", out.len()), body.to_string()));
        }
        rest = &after[end + 2..];
    }
    out
}

/// Every column type in hot and cold position, read and written.
fn column_programs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (t, one) in [
        ("u8", "3"),
        ("u16", "300"),
        ("u32", "70000"),
        ("u64", "5000000000"),
        ("i32", "7"),
        ("i64", "9"),
        ("f32", "1.5"),
        ("f64", "2.5"),
        ("bool", "true"),
    ] {
        for place in ["hot", "cold"] {
            let source = format!(
                "copy struct Pair {{ a: u64, b: u64 }}\n\
                 tiled(8) copy struct Row {{ {place} x: {t}, hot k: u64, cold p: Pair }}\n\
                 fn main() -> u64 {{\n\
                 \x20   let mut s: TiledSeq[Row] = TiledSeq::new[Row]();\n\
                 \x20   s.push(Row {{ x: {one}, k: 1, p: Pair {{ a: 2, b: 3 }} }});\n\
                 \x20   s.push(Row {{ x: {one}, k: 4, p: Pair {{ a: 5, b: 6 }} }});\n\
                 \x20   s.col_set[x](1, {one});\n\
                 \x20   s.col_set[p](0, Pair {{ a: 7, b: 8 }});\n\
                 \x20   let r = s.get(1);\n\
                 \x20   s.set(0, r);\n\
                 \x20   let x = s.col_get[x](0);\n\
                 \x20   let same: u64 = if x == r.x {{ 1 }} else {{ 0 }};\n\
                 \x20   wrapping_add(wrapping_add(s.col_get[p](0).a, s.col_get[k](1)), wrapping_add(same, wrapping_add(s.len(), s.tile_count())))\n\
                 }}\n"
            );
            out.push((format!("{place} {t}"), source));
        }
    }
    out
}

#[test]
fn tiled_operations_match_the_rust_lowering() {
    let _ = hook();
    let before = HANDLED.load(Ordering::Relaxed);
    let mut diff = Diff::default();
    let mut compiled = Vec::new();
    let mut rejected = Vec::new();
    for (name, source) in tiled_rs_programs().into_iter().chain(column_programs()) {
        let Some(world) = accepted(&source) else {
            rejected.push(name);
            continue;
        };
        for optimize in [false, true] {
            diff.add(compare(&name, &world, "main", hooks(), optimize, true));
        }
        compiled.push(name);
    }
    let handled = HANDLED.load(Ordering::Relaxed) - before;
    eprintln!(
        "tiled differential: {} programs (rejected: {rejected:?}), {}; {handled} requests from Vesper",
        compiled.len(),
        diff.summary(),
    );
    assert!(diff.is_clean(), "{}", diff.summary());
    assert!(compiled.len() >= 20, "{compiled:?} {rejected:?}");
    assert!(diff.worlds >= 40, "{}", diff.summary());
}

// ----------------------------------------------------------------- regions

/// The region fixtures of tests/regions_backends.rs and the verified
/// codegen corpus, and one program per scalar payload type.
fn region_programs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for dir in ["tests/fixtures/regions", "tests/fixtures/verified_codegen"] {
        let mut files: Vec<_> = std::fs::read_dir(repo().join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "ves")
                    && std::fs::read_to_string(p).is_ok_and(|t| t.contains("region "))
            })
            .collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            out.push((
                format!("{dir}/{name}"),
                std::fs::read_to_string(&f).unwrap(),
            ));
        }
    }
    for (t, one, two, bits) in [
        ("u8", "3", "4", "widen_u64(x)"),
        ("u16", "300", "301", "widen_u64(x)"),
        ("u32", "70000", "70001", "widen_u64(x)"),
        ("u64", "5000000000", "5000000001", "x"),
        ("i32", "7", "8", "wrapping_u64(x)"),
        ("i64", "9", "10", "wrapping_u64(x)"),
        ("f32", "1.5", "2.5", "widen_u64(to_bits_f32(x))"),
        ("f64", "2.5", "3.5", "to_bits_f64(x)"),
        ("bool", "true", "false", "if x { 1 } else { 2 }"),
        ("u128", "7", "8", "wrapping_u64(x)"),
    ] {
        out.push((
            format!("region payload {t}"),
            format!(
                "fn main() -> u64 {{\n\
                 \x20   region r(256) {{\n\
                 \x20       let mark = r.checkpoint();\n\
                 \x20       let h: RegionBox[r, {t}] = r.box({one});\n\
                 \x20       r.set(h, {two});\n\
                 \x20       let x: {t} = r.get(h);\n\
                 \x20       let y = {bits};\n\
                 \x20       let used = r.used();\n\
                 \x20       r.rewind(mark);\n\
                 \x20       wrapping_add(y, used)\n\
                 \x20   }}\n\
                 }}\n"
            ),
        ));
    }
    out
}

#[test]
fn region_blocks_match_the_rust_lowering() {
    let _ = hook();
    let before = HANDLED.load(Ordering::Relaxed);
    let mut diff = Diff::default();
    let mut compiled = Vec::new();
    let mut rejected = Vec::new();
    for (name, source) in region_programs() {
        let Some(world) = accepted(&source) else {
            rejected.push(name);
            continue;
        };
        for optimize in [false, true] {
            diff.add(compare(&name, &world, "main", hooks(), optimize, true));
        }
        compiled.push(name);
    }
    let handled = HANDLED.load(Ordering::Relaxed) - before;
    eprintln!(
        "region differential: {} programs (rejected: {rejected:?}), {}; {handled} requests from Vesper",
        compiled.len(),
        diff.summary(),
    );
    assert!(diff.is_clean(), "{}", diff.summary());
    for name in [
        "region payload f32",
        "region payload bool",
        "tests/fixtures/regions/aggregates.ves",
    ] {
        assert!(
            compiled.iter().any(|c| c == name),
            "{name} did not compile: {rejected:?}"
        );
    }
    assert!(handled > 0);
}

// ----------------------------------------------------------------- atomics

/// Every operation of `Atomic[k]` for each cell kind, and a scoped-worker
/// loop (which object output lowers to the prelude's loop).
fn atomic_programs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for k in ["u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64"] {
        let v = |x: u64| format!("wrapping_{k}({x})");
        let mut body = String::new();
        for (i, op) in [
            "swap",
            "fetch_add",
            "fetch_sub",
            "fetch_and",
            "fetch_or",
            "fetch_xor",
        ]
        .iter()
        .enumerate()
        {
            body += &format!(
                "    acc = mix(acc, wrapping_u64(c.{op}({}, MemoryOrder::SeqCst)));\n",
                v(0x9b + 77 * i as u64)
            );
        }
        body += &format!("    c.store({}, MemoryOrder::Release);\n", v(250));
        body += "    acc = mix(acc, wrapping_u64(c.load(MemoryOrder::Acquire)));\n";
        body += &format!(
            "    match c.compare_exchange({}, {}, MemoryOrder::AcqRel, MemoryOrder::Acquire) {{\n\
             \x20       Result::Ok(p) => {{ acc = mix(acc, wrapping_u64(p)); }}\n\
             \x20       Result::Err(p) => {{ acc = mix(acc, wrapping_add(wrapping_u64(p), 1)); }}\n\
             \x20   }}\n",
            v(250),
            v(3)
        );
        out.push((
            format!("atomic {k}"),
            format!(
                "fn mix(acc: u64, v: u64) -> u64 {{ wrapping_add(wrapping_mul(acc, 1099511628211), v) }}\n\
                 fn main() -> u64 {{\n\
                 \x20   let c: Atomic[{k}] = Atomic::new({});\n\
                 \x20   let mut acc: u64 = 7;\n\
                 {body}\x20   mix(acc, wrapping_u64(c.into_inner()))\n\
                 }}\n",
                v(0xf1)
            ),
        ));
    }
    out.push((
        "atomic bool".into(),
        "fn bit(b: bool) -> u64 { if b { 1 } else { 0 } }\n\
         fn main() -> u64 {\n\
         \x20   let f: Atomic[bool] = Atomic::new(false);\n\
         \x20   let a = bit(f.fetch_or(true, MemoryOrder::Relaxed));\n\
         \x20   let b = bit(f.fetch_xor(true, MemoryOrder::AcqRel));\n\
         \x20   let c = bit(f.swap(true, MemoryOrder::SeqCst));\n\
         \x20   let d = bit(f.fetch_and(false, MemoryOrder::Release));\n\
         \x20   f.store(true, MemoryOrder::Relaxed);\n\
         \x20   let h = bit(f.load(MemoryOrder::Acquire));\n\
         \x20   wrapping_add(wrapping_add(a, wrapping_mul(b, 2)), wrapping_add(wrapping_mul(c, 4), wrapping_add(wrapping_mul(d, 8), wrapping_mul(h, 16))))\n\
         }\n"
            .into(),
    ));
    out
}

#[test]
fn atomic_operations_match_the_rust_lowering() {
    let _ = hook();
    let before = HANDLED.load(Ordering::Relaxed);
    let mut diff = Diff::default();
    let mut compiled = Vec::new();
    let mut rejected = Vec::new();
    for (name, source) in atomic_programs() {
        let Some(world) = accepted(&source) else {
            rejected.push(name);
            continue;
        };
        for optimize in [false, true] {
            diff.add(compare(&name, &world, "main", hooks(), optimize, true));
        }
        compiled.push(name);
    }
    let handled = HANDLED.load(Ordering::Relaxed) - before;
    eprintln!(
        "atomic differential: {} programs (rejected: {rejected:?}), {}; {handled} requests from Vesper",
        compiled.len(),
        diff.summary(),
    );
    assert!(diff.is_clean(), "{}", diff.summary());
    assert!(rejected.is_empty(), "{rejected:?}");
    assert!(diff.worlds >= 18, "{}", diff.summary());
    assert!(handled > 0);
}

// ------------------------------------------------------------------ floats

/// Float programs: `(name, source, run)`. Each operation sits in a function
/// `k` of parameters, so neither optimizer folds it away; `main` returns its
/// bit pattern. Checked narrowings raise `Fail[ArithOverflow]` under a
/// handler in `main`.
fn float_programs() -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let mut push =
        |name: String, params: &str, result: &str, body: &str, args: &str, bits: &str| {
            for (effect, tag) in [("", ""), (" eff [ConstDenorm]", " flushed")] {
                let source = format!(
                    "fn k({params}) -> {result}{effect} {{ {body} }}\n\
                 fn main() -> u64 {{ {bits} }}\n",
                    bits = bits.replace("K", &format!("k({args})")),
                );
                out.push((format!("{name}{tag}"), source, true));
            }
        };
    for (t, a, b, c) in [
        ("f32", "2.25", "-7.5", "0.375"),
        ("f64", "2.75", "-3.5", "1.0e-310"),
    ] {
        let (bits, word, one) = if t == "f32" {
            ("widen_u64(to_bits_f32(K))".to_string(), "u32", "1073741824")
        } else {
            ("to_bits_f64(K)".to_string(), "u64", "4611686018427387904")
        };
        for op in ["sqrt", "abs", "floor", "ceil", "round", "trunc"] {
            for x in [a, b, c] {
                push(
                    format!("{op} {t} {x}"),
                    &format!("x: {t}"),
                    t,
                    &format!("{op}(x)"),
                    x,
                    &bits,
                );
            }
        }
        for op in ["min", "max"] {
            push(
                format!("{op} {t}"),
                &format!("x: {t}, y: {t}"),
                t,
                &format!("{op}(x, y)"),
                &format!("{a}, {b}"),
                &bits,
            );
        }
        push(
            format!("fma {t}"),
            &format!("x: {t}, y: {t}, z: {t}"),
            t,
            "fma(x, y, z)",
            &format!("{a}, {b}, {c}"),
            &bits,
        );
        push(
            format!("to_bits {t}"),
            &format!("x: {t}"),
            word,
            &format!("to_bits_{t}(x)"),
            b,
            "widen_u64(K)",
        );
        push(
            format!("from_bits {t}"),
            &format!("x: {word}"),
            t,
            &format!("from_bits_{t}(x)"),
            one,
            &bits,
        );
        for k in ["u8", "i8", "u32", "i32", "u64", "i64"] {
            push(
                format!("saturating_{k} {t}"),
                &format!("x: {t}"),
                k,
                &format!("saturating_{k}(x)"),
                b,
                "wrapping_u64(K)",
            );
        }
    }
    push(
        "widen_f64 f32".into(),
        "x: f32",
        "f64",
        "widen_f64(x)",
        "2.25",
        "to_bits_f64(K)",
    );
    push(
        "nearest_f32 f64".into(),
        "x: f64",
        "f32",
        "nearest_f32(x)",
        "2.75",
        "widen_u64(to_bits_f32(K))",
    );
    for (k, x) in [
        ("u64", "18446744073709551615"),
        ("i64", "9"),
        ("u32", "70000"),
        ("i32", "7"),
        ("u8", "200"),
    ] {
        for t in ["f32", "f64"] {
            let bits = if t == "f32" {
                "widen_u64(to_bits_f32(K))"
            } else {
                "to_bits_f64(K)"
            };
            push(
                format!("nearest_{t} {k}"),
                &format!("x: {k}"),
                t,
                &format!("nearest_{t}(x)"),
                x,
                bits,
            );
        }
    }
    for t in ["f32", "f64"] {
        for k in ["i32", "u8", "i64"] {
            out.push((
                format!("narrow_{k} {t}"),
                format!(
                    "fn k(x: {t}) -> {k} eff [Fail[ArithOverflow]] {{ narrow_{k}(x) }}\n\
                     fn main() -> u64 {{ handle Fail[ArithOverflow] in {{ wrapping_u64(k(3.75)) }} with {{ return v => v, raise(e, k) => 4242, }} }}\n"
                ),
                true,
            ));
        }
    }
    out
}

#[test]
fn float_operations_match_the_rust_lowering() {
    let _ = hook();
    let before = HANDLED.load(Ordering::Relaxed);
    let mut diff = Diff::default();
    let mut compiled = Vec::new();
    let mut rejected = Vec::new();
    for (name, source, run) in float_programs() {
        let Some(world) = accepted(&source) else {
            rejected.push(name);
            continue;
        };
        for optimize in [false, true] {
            diff.add(compare(&name, &world, "main", hooks(), optimize, run));
        }
        compiled.push(name);
    }
    let handled = HANDLED.load(Ordering::Relaxed) - before;
    eprintln!(
        "float differential: {} programs (rejected: {rejected:?}), {}; {handled} requests from Vesper",
        compiled.len(),
        diff.summary(),
    );
    assert!(diff.is_clean(), "{}", diff.summary());
    for op in [
        "sqrt",
        "round",
        "fma",
        "min",
        "saturating_u8",
        "from_bits",
        "nearest_f32",
        "narrow_i32",
        "flushed",
    ] {
        assert!(
            compiled.iter().any(|n| n.contains(op)),
            "no {op} program compiled: {rejected:?}"
        );
    }
}
