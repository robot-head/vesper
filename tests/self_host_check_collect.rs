//! The checker's declaration collector in Vesper (#2024 slice 2):
//! `packages/vesper-check-collect` against `World::collect` of src/check.rs.
//!
//! Both sides parse each source (`@vesper/ast` matches
//! `vesper_syntax::parser::parse` node for node, tests/self_host_ast.rs),
//! collect it over the builtin declarations and write one canonical dump:
//! the ADT, effect and function tables in key order (each entry's
//! declaration with its span, `impl` methods under their `<type>::<name>`
//! key with the block's generics and `self` type), then the diagnostics.
//! The two dumps must be equal byte for byte.
//!
//! Part 1 of the port declines a module that could need one of the
//! checker's preludes (`needs_prelude` in the package); the test counts the
//! declined modules and requires every other one to match. The corpus is
//! every `.ves` file of the repository plus hand-written cases for each
//! diagnostic.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use vesper::ast::*;
use vesper::check::World;
use vesper::native;

#[path = "common/check_harness.rs"]
mod check_harness;
use check_harness::*;

const PACKAGE: &str = "packages/vesper-check-collect";

// ---------------------------------------------------------------- the dump

fn effects(o: &mut String, e: &Effects) {
    o.push('{');
    for (i, x) in e.instances.iter().enumerate() {
        if i > 0 {
            o.push_str(", ");
        }
        if let Some(l) = &x.label {
            let _ = write!(o, "{l}:");
        }
        o.push_str(&x.name);
        types(o, &x.args);
    }
    o.push('|');
    o.push_str(e.tail.as_deref().unwrap_or("-"));
    o.push('}');
}

fn types(o: &mut String, ts: &[Type]) {
    if ts.is_empty() {
        return;
    }
    o.push('[');
    for (i, t) in ts.iter().enumerate() {
        if i > 0 {
            o.push_str(", ");
        }
        ty(o, t);
    }
    o.push(']');
}

fn ty(o: &mut String, t: &Type) {
    match t {
        Type::Named(n, args) => {
            o.push_str(n);
            types(o, args);
        }
        Type::Tuple(ts) => {
            o.push('(');
            for (i, t) in ts.iter().enumerate() {
                if i > 0 {
                    o.push_str(", ");
                }
                ty(o, t);
            }
            o.push(')');
        }
        Type::Function {
            params,
            result,
            effects: e,
            mode,
        } => {
            o.push_str("fn(");
            for (i, (p, t)) in params.iter().enumerate() {
                if i > 0 {
                    o.push_str(", ");
                }
                let _ = write!(o, "{} ", *p as u8);
                ty(o, t);
            }
            o.push_str(") -> ");
            ty(o, result);
            o.push(' ');
            effects(o, e);
            let _ = write!(o, " mode={}", *mode as u8);
        }
        Type::Row(e) => {
            o.push_str("row");
            effects(o, e);
        }
        Type::Infer => o.push('_'),
        Type::IntLiteral(_) => o.push_str("lit"),
    }
}

fn generics(o: &mut String, gs: &[Generic], depth: usize) {
    for g in gs {
        let _ = writeln!(
            o,
            "{}generic {} bound={} row={}",
            " ".repeat(depth),
            g.name,
            g.bound as u8,
            g.row
        );
    }
}

fn function(o: &mut String, f: &Function, depth: usize) {
    let _ = writeln!(
        o,
        " fip={} body={} span={}..{}",
        f.fip,
        f.body.is_some(),
        f.span.start,
        f.span.end
    );
    generics(o, &f.generics, depth);
    let pad = " ".repeat(depth);
    for p in &f.params {
        let proj = p
            .projections
            .iter()
            .map(|path| path.join("."))
            .collect::<Vec<_>>()
            .join(",");
        let _ = write!(
            o,
            "{pad}param {} pass={} proj=[{proj}] : ",
            p.name, p.pass as u8
        );
        ty(o, &p.ty);
        o.push('\n');
    }
    let _ = write!(o, "{pad}result ");
    ty(o, &f.result);
    let _ = write!(o, "\n{pad}effects ");
    effects(o, &f.effects);
    o.push('\n');
}

fn adt(o: &mut String, a: &Adt) {
    let tile = a.tile_width.map_or("-".to_owned(), |w| w.to_string());
    let _ = writeln!(
        o,
        "adt {} mode={} enum={} tile={tile} span={}..{}",
        a.name, a.mode as u8, a.is_enum, a.span.start, a.span.end
    );
    generics(o, &a.generics, 1);
    for v in &a.variants {
        let _ = writeln!(o, " variant {} named={}", v.name, v.named);
        for f in &v.fields {
            let modality = f.modality.map_or("-".to_owned(), |m| (m as u8).to_string());
            let temp = match f.temperature {
                None => 0,
                Some(FieldTemperature::Hot) => 1,
                Some(FieldTemperature::Cold) => 2,
            };
            let _ = write!(o, "  field {} mod={modality} temp={temp} : ", f.name);
            ty(o, &f.ty);
            o.push('\n');
        }
    }
}

fn effect(o: &mut String, e: &EffectDecl) {
    let _ = writeln!(
        o,
        "effect {} linear={} span={}..{}",
        e.name, e.linear, e.span.start, e.span.end
    );
    generics(o, &e.generics, 1);
    for op in &e.ops {
        let _ = write!(o, " op {}", op.name);
        function(o, op, 2);
    }
}

/// The Rust side: `World::collect` of `module`, dumped.
fn rust_text(module: &Module) -> String {
    let (w, diagnostics): (World, _) = World::collect(module);
    let mut o = String::new();
    for a in w.adts.values() {
        adt(&mut o, a);
    }
    for e in w.effects.values() {
        effect(&mut o, e);
    }
    for (key, f) in &w.functions {
        let _ = write!(o, "fn {key}");
        function(&mut o, f, 1);
    }
    for d in &diagnostics {
        let _ = writeln!(
            o,
            "diag {} {}..{} {} | {}",
            d.code,
            d.span.start,
            d.span.end,
            d.message,
            d.help.as_deref().unwrap_or("-")
        );
    }
    o
}

// ---------------------------------------------------------------- the Vesper side

/// The package's `collect_text` entry, compiled once (optimized only when
/// timing, as the other check differentials do).
fn entry() -> &'static native::WordFunction {
    static F: OnceLock<native::WordFunction> = OnceLock::new();
    F.get_or_init(|| {
        let t = std::time::Instant::now();
        let f = Package::load(PACKAGE).entry("collect_text", speed()).0;
        eprintln!("compiled collect_text in {:?}", t.elapsed());
        f
    })
}

fn vesper_text(source: &str) -> String {
    let words = [source.len() as u64, 0, source.as_ptr() as u64];
    // SAFETY: the entry takes `in Str` (the address of its three words
    // `{len, 0, data}`, which live across the call, as does `source`).
    let handle = unsafe { entry().call(&[words.as_ptr() as u64]) }.unwrap();
    unpack_text(&take_words(handle))
}

// ---------------------------------------------------------------- the corpus

fn ves_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if p.is_dir() {
            if !matches!(&*name, "target" | ".git" | "node_modules") && !name.starts_with("bazel-")
            {
                ves_files(&p, out);
            }
        } else if p.extension().is_some_and(|e| e == "ves") {
            out.push(p);
        }
    }
}

/// Sources written for the diagnostics and the `impl` rules.
const CASES: &[&str] = &[
    "fn main() -> i64 { 0 }",
    "struct A { x: i64 } struct A { y: bool } fn f() -> A { A { y: true } }",
    "enum Option[T] { None, Some(T) } fn f() -> i64 { 1 }",
    "struct Unit {} struct Never {} struct String {}",
    "effect Ask { fn ask() -> i64; } effect Ask { fn ask() -> bool; }",
    "effect ConstTime { fn tick() -> Unit; } effect Isa[P] { }",
    "effect DetFloat { } effect Realtime { } effect ConstDenorm { }",
    "fn f() -> i64 { 1 } fn f() -> bool { true }",
    "fn wrapping_add(a: i64, b: i64) -> i64 { a } fn wrapping_mul(a: i64) -> i64 { a }",
    "struct P { x: i64 } impl P { fn get(self) -> i64 { self.x } fn put(inout self, v: i64) { self.x = v; } }",
    "struct P[T] { x: T } impl[T] P[T] { fn get[U](self, u: U) -> T { self.x } }",
    "struct P { x: i64 } impl P { fn get(self) -> i64 { 1 } } impl P { fn get(self) -> i64 { 2 } }",
    "struct P { x: i64 } fn P::get(p: P) -> i64 { 1 } impl P { fn get(self) -> i64 { 2 } }",
    "effect Ask[T] { fn ask(x: T) -> T; } fn f() -> i64 eff [a: Ask[i64] | r] { 1 }",
    "fn apply(f: fn(in i64) -> i64 eff [Fail[i64]], x: (i64, bool)) -> i64 { 1 }",
    "linear effect Once { fn once() -> Unit; } affine struct H { v: i64 }",
    "struct S { hot x: i64, cold y: i64, linear z: i64 }",
    "enum E { A { x: i64 }, B(i64, bool), C }",
    // The string and option preludes, and methods a module defines itself.
    "fn f(s: Str) -> u64 { s.len() }",
    "fn f() -> String { String::new() }",
    "impl Str { fn concat(in self, in o: Str) -> String { String::new() } } fn f(s: Str) -> u64 { 1 }",
    "fn f(o: Option[u64]) -> u64 { o.unwrap_or(0) }",
    "impl[T: mod copy] Option[T] { fn unwrap_or(self, f: T) -> T { f } } fn f(o: Option[u64]) -> u64 { o.or(Option::None).unwrap_or(0) }",
    "impl[T: mod copy] Option[T] { fn is_some(self) -> bool { true } } fn f(o: Option[u64]) -> bool { o.is_some() }",
    "fn f(s: Str, o: Option[u64]) -> u64 { s.char_count() + o.unwrap_or(1) }",
    // The width prelude: one name of every family (`WIDTH_NAMES`), a
    // module that defines one itself, and the reserved names.
    "fn f(x: u64) -> u64 { let (lo, hi) = mul_wide(x, x); lo }",
    "fn checked_mul_u64(a: u64, b: u64) -> Option[u64] { Option::None } fn f() -> u64 { checked_mul_u64(1, 2); 0 }",
    "fn leading_zeros_u32(x: u32) -> u32 { x } fn f() -> u32 { 0 }",
    "fn to_le_bytes_u8(x: u8) -> u8 { x } fn f() -> u8 { to_le_bytes_u8(1) }",
    // The `Box` prelude.
    "fn f(b: Box[i64]) -> i64 { 1 }",
    "fn f() -> i64 { let b = Box::new(1); 1 }",
    "fn f(o: Option[i64]) -> i64 { match o { Option::Some(Box::Box(x)) => x, _ => 0 } }",
    "struct Box { x: i64 } fn f(b: Box) -> i64 { 1 }",
    "struct S { b: Box[u8] }",
    // The atomic and worker preludes.
    "fn f(a: Atomic[u64]) -> u64 { 1 }",
    "fn f() -> u64 { let o = MemoryOrder::SeqCst; 1 }",
    "struct Atomic { x: u64 } fn f(a: Atomic) -> u64 { 1 }",
    "fn body(in d: u64, i: u64) { } fn f() { parallel_for(in 1, 2, body); }",
    "fn f(o: Option[u64]) -> u64 { let c = Atomic::new(1); parallel_for(in c, 1, g); o.unwrap_or(0) }",
    // The float prelude.
    "fn f(x: f64) -> f64 { exp_f64(x) }",
    "fn f(x: f32) -> f32 { cos_f32(sin_f32(x)) + ln_f32(x) }",
    "fn float_kernel_sin_f64(x: f64, y: f64, i: u64) -> f64 { x } fn f(x: f64) -> f64 { sin_f64(x) }",
    "fn exp_f64(x: f64) -> f64 { x } fn f(x: f32) -> f32 { exp_f32(x) }",
    "fn f(x: f64, s: Str) -> f64 { let n = checked_add_u64(1, 2); exp_f64(x) }",
];

/// Every byte-order, bit and checked builtin, named once.
const WIDTH_NAMES: &str = "to_le_bytes_u8; to_le_bytes_u16; to_le_bytes_u32; to_le_bytes_u64; to_le_bytes_usize; to_le_bytes_i8; to_le_bytes_i16; to_le_bytes_i32; to_le_bytes_i64; to_le_bytes_isize; to_le_bytes_i128; to_le_bytes_u128; to_be_bytes_u8; to_be_bytes_u16; to_be_bytes_u32; to_be_bytes_u64; to_be_bytes_usize; to_be_bytes_i8; to_be_bytes_i16; to_be_bytes_i32; to_be_bytes_i64; to_be_bytes_isize; to_be_bytes_i128; to_be_bytes_u128; from_le_bytes_u8; from_le_bytes_u16; from_le_bytes_u32; from_le_bytes_u64; from_le_bytes_usize; from_le_bytes_i8; from_le_bytes_i16; from_le_bytes_i32; from_le_bytes_i64; from_le_bytes_isize; from_le_bytes_i128; from_le_bytes_u128; from_be_bytes_u8; from_be_bytes_u16; from_be_bytes_u32; from_be_bytes_u64; from_be_bytes_usize; from_be_bytes_i8; from_be_bytes_i16; from_be_bytes_i32; from_be_bytes_i64; from_be_bytes_isize; from_be_bytes_i128; from_be_bytes_u128; leading_zeros_u8; leading_zeros_u16; leading_zeros_u32; leading_zeros_u64; leading_zeros_usize; leading_zeros_i8; leading_zeros_i16; leading_zeros_i32; leading_zeros_i64; leading_zeros_isize; leading_zeros_i128; leading_zeros_u128; trailing_zeros_u8; trailing_zeros_u16; trailing_zeros_u32; trailing_zeros_u64; trailing_zeros_usize; trailing_zeros_i8; trailing_zeros_i16; trailing_zeros_i32; trailing_zeros_i64; trailing_zeros_isize; trailing_zeros_i128; trailing_zeros_u128; count_ones_u8; count_ones_u16; count_ones_u32; count_ones_u64; count_ones_usize; count_ones_i8; count_ones_i16; count_ones_i32; count_ones_i64; count_ones_isize; count_ones_i128; count_ones_u128; wrapping_shl_u8; wrapping_shl_u16; wrapping_shl_u32; wrapping_shl_u64; wrapping_shl_usize; wrapping_shl_i8; wrapping_shl_i16; wrapping_shl_i32; wrapping_shl_i64; wrapping_shl_isize; wrapping_shl_i128; wrapping_shl_u128; wrapping_shr_u8; wrapping_shr_u16; wrapping_shr_u32; wrapping_shr_u64; wrapping_shr_usize; wrapping_shr_i8; wrapping_shr_i16; wrapping_shr_i32; wrapping_shr_i64; wrapping_shr_isize; wrapping_shr_i128; wrapping_shr_u128; wrapping_neg_u8; wrapping_neg_u16; wrapping_neg_u32; wrapping_neg_u64; wrapping_neg_usize; wrapping_neg_i8; wrapping_neg_i16; wrapping_neg_i32; wrapping_neg_i64; wrapping_neg_isize; wrapping_neg_i128; wrapping_neg_u128; checked_add_u8; checked_add_u16; checked_add_u32; checked_add_u64; checked_add_usize; checked_add_i8; checked_add_i16; checked_add_i32; checked_add_i64; checked_add_isize; checked_add_i128; checked_add_u128; checked_sub_u8; checked_sub_u16; checked_sub_u32; checked_sub_u64; checked_sub_usize; checked_sub_i8; checked_sub_i16; checked_sub_i32; checked_sub_i64; checked_sub_isize; checked_sub_i128; checked_sub_u128; checked_mul_u8; checked_mul_u16; checked_mul_u32; checked_mul_u64; checked_mul_usize; checked_mul_i8; checked_mul_i16; checked_mul_i32; checked_mul_i64; checked_mul_isize; checked_mul_i128; checked_mul_u128";

#[test]
fn collect_matches_rust() {
    let _quiet = quiet_panics();
    let mut sources: Vec<(String, String)> = CASES
        .iter()
        .enumerate()
        .map(|(i, s)| (format!("case {i}"), (*s).to_owned()))
        .collect();
    sources.push((
        "width names".into(),
        format!("fn f() -> u64 {{ {WIDTH_NAMES}; 0 }}"),
    ));
    let mut files = vec![];
    for d in ["examples", "tests", "packages", "crates", "src"] {
        ves_files(&repo().join(d), &mut files);
    }
    for p in files {
        if let Ok(s) = std::fs::read_to_string(&p)
            && s.len() <= 1 << 20
        {
            sources.push((
                p.strip_prefix(repo()).unwrap_or(&p).display().to_string(),
                s,
            ));
        }
    }
    let (mut matched, mut declined, mut skipped) = (0usize, 0usize, 0usize);
    let mut differ = vec![];
    for (name, source) in &sources {
        let parsed = vesper::parser::parse(source);
        if !parsed.diagnostics.is_empty() {
            skipped += 1;
            continue;
        }
        let v = vesper_text(source);
        if v == "declined\n" {
            declined += 1;
            continue;
        }
        let r = rust_text(&parsed.module);
        if r == v {
            matched += 1;
        } else {
            let first = first_difference(&r, &v).unwrap_or_else(|| "trailing bytes".into());
            differ.push(format!("{name}: {first}"));
        }
    }
    for d in differ.iter().take(20) {
        eprintln!("  {d}");
    }
    eprintln!(
        "collect: {} sources, {matched} match, {declined} declined (prelude), {skipped} skipped (parse errors), {} differ",
        sources.len(),
        differ.len()
    );
    assert!(matched > CASES.len(), "too few compared");
    assert_eq!(differ.len(), 0);
}
