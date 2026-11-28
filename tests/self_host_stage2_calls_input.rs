//! Bootstrap switch slice 3d (#2659, part of #2349): @vesper/native-lib's
//! calls input made by Stage 2's own front end (@vesper/export-build
//! `stage2_calls_input`: @vesper/stage2's loader, checker and lowering,
//! written with the Vesper writers of #2677) against the one the Rust
//! compiler encodes (tests/common/codegen_calls_words.rs) for the same
//! library.
//!
//! The two inputs spell names and types in different orders, so they are
//! compared through what reads them: @vesper/codegen-calls' instances and
//! declarations (`codegen_calls_text`) and @vesper/place-ir's canonical
//! program dump (`place_ir_dump`).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/codegen_calls_words.rs"]
mod codegen_calls_words;
#[path = "common/ir_words.rs"]
mod ir_words;
#[allow(dead_code)]
#[path = "self_host_codegen_export/ports.rs"]
mod ports;

use check_harness::{Entry, Package, repo};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The driver: `driver <package> <roots file> <optimize: 0 or 1> <out> <os>`
/// writes the Stage 2 calls input and exits 0, or exits with the reason it
/// declined (`CALLS_*` in packages/vesper-export-build/src/calls_input.ves)
/// after writing the front end's error.
const DRIVER: &str = r#"use @vesper/export-build::calls_input::{stage2_calls_input};
use @vesper-std/env::{arg_bytes};
use @vesper-std/fs::{IoError, read_path, write_path};

fn arg(i: u64) -> ChunkSeq[u8] eff [Ffi[StdEnv]] {
    match arg_bytes(i) {
        Option::Some(a) => a,
        Option::None => ChunkSeq::new[u8](),
    }
}

pub fn main() -> u64 eff [Ffi[StdFs], Ffi[StdEnv]] {
    let root = arg(1);
    let roots_path = arg(2);
    let optimize = arg(3);
    let out = arg(4);
    let os = arg(5);
    let mut roots: ChunkSeq[u8] = ChunkSeq::new[u8]();
    handle Fail[IoError] in {
        roots = read_path(in roots_path.as_slice());
    } with {
        return v => v,
        raise(_e, _k) => {},
    }
    let made = stage2_calls_input(
        in root.as_slice(),
        in os.as_slice(),
        in roots.as_slice(),
        optimize.try_get(0).unwrap_or(0) == 49
    );
    let body = if made.declined == 0 { made.bytes } else { made.err };
    handle Fail[IoError] in {
        write_path(in out.as_slice(), in body.as_slice());
    } with {
        return v => v,
        raise(_e, _k) => {},
    }
    widen_u64(made.declined)
}
"#;

/// Why the driver declined, by exit status.
const DECLINES: &[&str] = &[
    "",
    "the Stage 2 front end failed",
    "dyn, fip or an expression the encoder cannot spell",
    "no library root",
];

/// The functions Stage 2 must lower as Rust does today. The rest differ
/// where Stage 2's checker records less than Rust's: overflow and partial
/// operations (`fallible`) and the unit type of statements.
const MIN_SAME_FUNCTIONS: usize = 51;

/// The port functions Stage 2 must lower as Rust does today.
const MIN_SAME_PORT_FUNCTIONS: usize = 18919;

/// Set to print the Rust and Stage 2 bodies of each function that differs.
const SHOW_DIFFS: &str = "VESPER_STAGE2_CALLS_DIFFS";

/// The functions of a `place_ir_dump`, by name: each body's lines with
/// path ids replaced by the paths they name, and the spans (which Stage 2
/// offsets by the modules assembled before the package, and leaves out of
/// parameters) left out.
fn functions(dump: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    let dump = &nested_by_order(dump);
    let mut paths = std::collections::HashMap::new();
    let mut pool = std::collections::HashMap::new();
    let mut out = std::collections::BTreeMap::new();
    let mut current: Option<String> = None;
    for line in dump.lines() {
        if let Some(rest) = line.strip_prefix("path ") {
            let (id, named) = rest.split_once(' ').unwrap_or((rest, ""));
            paths.insert(id.to_string(), named.to_string());
        } else if let Some(rest) = line.strip_prefix("pool ")
            && current.is_none()
        {
            let (id, entry) = rest.split_once(' ').unwrap_or((rest, ""));
            pool.insert(id.to_string(), entry.to_string());
        } else if let Some(name) = line.strip_prefix("fn ") {
            current = Some(name.to_string());
            out.insert(name.to_string(), vec![]);
        } else if let Some(name) = &current {
            let words: Vec<String> = line
                .split_whitespace()
                .map(|w| match (paths.get(w), pool.get(w)) {
                    (Some(p), _) if w.starts_with('p') => format!("[{p}]"),
                    (_, Some(e)) if w.starts_with('#') => format!("{{{e}}}"),
                    _ => w.to_string(),
                })
                .collect();
            if let Some(body) = out.get_mut(name) {
                body.push(words.join(" "));
            }
        }
    }
    out
}

/// The prefixes of the functions the lowering names by their body's span
/// (`nested_name` in packages/vesper-place-ir-lower/src/lower.ves).
const NESTED: &[&str] = &["$closure_", "$handler_body_", "$handler_clause_"];

/// `dump` with each nested function's `<start>_<end>` replaced by its
/// place in span order: Stage 2's spans carry the module's offset, so the
/// two names differ while the order agrees.
fn nested_by_order(dump: &str) -> String {
    let spans = |text: &str| -> Vec<(usize, usize, (u64, u64))> {
        let mut out = vec![];
        for prefix in NESTED {
            let mut from = 0;
            while let Some(i) = text[from..].find(prefix) {
                let start = from + i + prefix.len();
                let digits =
                    |at: usize| at + text[at..].bytes().take_while(u8::is_ascii_digit).count();
                let mid = digits(start);
                let end = if text[mid..].starts_with('_') {
                    digits(mid + 1)
                } else {
                    mid
                };
                if let (Ok(a), Some(Ok(b))) = (
                    text[start..mid].parse(),
                    text.get(mid + 1..end).map(str::parse),
                ) {
                    out.push((start, end, (a, b)));
                }
                from = end.max(start);
            }
        }
        out.sort();
        out
    };
    let found = spans(dump);
    let mut order: Vec<(u64, u64)> = found.iter().map(|f| f.2).collect();
    order.sort();
    order.dedup();
    let mut out = String::with_capacity(dump.len());
    let mut at = 0;
    for (start, end, span) in found {
        out.push_str(&dump[at..start]);
        let place = order.binary_search(&span).unwrap_or(usize::MAX);
        out.push_str(&format!("#{place}"));
        at = end;
    }
    out.push_str(&dump[at..]);
    out
}

/// The first span of a module after the first in Stage 2's lowering: its
/// spans carry the module's index above the low 32 bits.
const MODULE_SPAN: u64 = 1 << 32;

/// Whether two bodies agree once spans are left out: `@a..b` tokens, every
/// number of a header line (parameter spans sit among them), and the
/// numbers Stage 2 offsets by a module (a binding's span).
fn same_body(a: &[String], b: &[String]) -> bool {
    let strip = |l: &String| -> Vec<String> {
        let header = l.starts_with("header");
        l.split(' ')
            .filter(|w| !w.starts_with('@') && !(header && w.parse::<u64>().is_ok()))
            .map(str::to_string)
            .collect()
    };
    let same_word = |x: &String, y: &String| match (x.parse::<u64>(), y.parse::<u64>()) {
        (Ok(m), Ok(n)) => m == n || m.max(n) >= MODULE_SPAN,
        _ => x == y,
    };
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            let (x, y) = (strip(x), strip(y));
            x.len() == y.len() && x.iter().zip(&y).all(|(m, n)| same_word(m, n))
        })
}

/// The calls task flags (packages/vesper-codegen-calls/src/data.ves).
const OPTIMIZE_TASK: u64 = 1;
const LIBRARY_TASK: u64 = 4;
const MORE_ROOTS_TASK: u64 = 8;

/// Libraries and their export roots.
const LIBRARIES: &[(&str, &str, &[&str])] = &[
    (
        "arith",
        "pub fn add3(a: i64, b: i64, c: i64) -> i64 { wrapping_add(wrapping_add(a, b), c) }
pub fn mix(a: u32, b: u32) -> u32 { (a ^ b) | (a & 7) }",
        &["add3", "mix"],
    ),
    (
        "branches",
        "pub fn sign(x: i16) -> i16 { if x < 0 { -1 } else if x == 0 { 0 } else { 1 } }
pub fn pick(c: bool, x: u8) -> u8 { let mut y: u8 = x; if c { y = x & 15; } y }",
        &["pick", "sign"],
    ),
    (
        "calls",
        "fn square(x: i64) -> i64 { wrapping_mul(x, x) }
pub fn sum_squares(a: i64, b: i64) -> i64 { wrapping_add(square(a), square(b)) }
pub fn quad(x: i64) -> i64 { square(square(x)) }",
        &["quad", "sum_squares"],
    ),
    (
        "fails",
        "fn step(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow]] { a * b + 1 }
pub fn total(a: u64, b: u64) -> u64 eff [Fail[ArithOverflow], Fail[RuntimeError]] { step(a, b) / b }",
        &["total"],
    ),
    (
        "guards",
        "pub fn safe_div(a: u64, b: u64) -> u64 eff [Fail[RuntimeError]] { if b == 0 { return 0; } a / b }
pub fn bounded(x: u8) -> u8 eff [Fail[ArithOverflow]] { if x < 200 { x + 50 } else { x } }
pub fn negate(x: i32) -> i32 eff [Fail[ArithOverflow]] { -x }
pub fn consts() -> i64 eff [Fail[ArithOverflow]] { let a = 5; let b = a * 3; b - 1 }
pub fn shifts(x: u64, n: u32) -> u64 eff [Fail[ArithOverflow]] { if n < 64 { x << n } else { x >> (n & 7) } }
pub fn low_byte(x: u64) -> u8 eff [Fail[ArithOverflow]] { narrow_u8(x & 255) }
pub fn any_byte(x: u64) -> u8 eff [Fail[ArithOverflow]] { narrow_u8(x) }
pub fn modulo(x: u64, m: u64) -> u64 eff [Fail[RuntimeError]] { x % (m | 1) }
pub fn halve(x: i64) -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] { x / 2 }
pub fn signed_div(x: i64, y: i64) -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] { if y > 0 { x / y } else { 0 } }",
        &["any_byte", "bounded", "consts", "halve", "low_byte", "modulo", "negate", "safe_div", "shifts", "signed_div"],
    ),
    (
        "loops",
        "pub fn sum_to(n: u64) -> u64 eff [Fail[ArithOverflow]] { let mut s: u64 = 0; for i in 0..n { s = s + i; } s }
pub fn count_down(n: i64) -> i64 eff [Fail[ArithOverflow]] { let mut k = n; let mut c: i64 = 0; loop { if k <= 0 { break; } k = k - 1; c += 1; } c }
pub fn fixed(lo: u32, hi: u32) -> u32 eff [Fail[ArithOverflow]] { let mut t: u32 = 0; for i in lo..hi { t = wrapping_add(t, i * 2); } t }
pub fn product() -> u64 eff [Fail[ArithOverflow]] { let mut acc: u64 = 1; for i in 1..=16 { acc = wrapping_mul(acc, i + 1); } acc }",
        &["count_down", "fixed", "product", "sum_to"],
    ),
    (
        "arrays",
        "pub fn pick(i: u64, a: u32) -> u32 eff [Fail[RuntimeError]] { let t: [u32; 4] = [a, a, a, a]; t[i] }
pub fn pick_masked(i: u64, a: u32) -> u32 eff [Fail[RuntimeError]] { let t: [u32; 4] = [a, a, a, a]; t[i & 3] }
pub fn guarded(i: u64, a: u32) -> u32 eff [Fail[RuntimeError]] { let t: [u32; 4] = [a, a, a, a]; if i < 4 { t[i] } else { a } }",
        &["guarded", "pick", "pick_masked"],
    ),
    (
        "seqs",
        "fn first(in s: Slice[u64], j: u64) -> u64 eff [Fail[RuntimeError]] { s[j] }
pub fn seqs(n: u64, j: u64) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] { let mut v = ChunkSeq::new[u64](); v.push(n); v.push(j); let mut t: u64 = 0; for i in 0..v.len() { t = t + v.get(i); } t + v[j] + first(in v.as_slice(), j) }",
        &["seqs"],
    ),
    // Literal typing the Stage 2 checker does not match yet (#2691): a
    // shift amount, a range typed by its index's uses, array literal
    // elements and sequence indices.
    (
        "literals",
        "pub fn shift3(x: u64) -> u64 { x >> 3 }
pub fn steps() -> u32 eff [Fail[ArithOverflow]] { let mut t: u32 = 0; for i in 0..10 { t = wrapping_add(t, i * 2); } t }
pub fn sum_arr() -> u32 eff [Fail[RuntimeError], Fail[ArithOverflow]] { let t: [u32; 4] = [1, 2, 3, 4]; let mut s: u32 = 0; for i in 0..4 { s = s + t[i]; } s }
pub fn second(n: u64) -> u64 eff [Fail[RuntimeError]] { let mut v = ChunkSeq::new[u64](); v.push(n); v[0] }",
        &["second", "shift3", "steps", "sum_arr"],
    ),
    // Generic arguments inferred from the expected result (`apply`).
    (
        "results",
        "fn fail_with[T](code: u64) -> Result[T, u64] { Result::Err(code) }
pub fn checked(x: u64) -> Result[bool, u64] { if x == 0 { return fail_with(x); } Result::Ok(true) }
pub fn tagged(x: u64) -> Result[u8, u64] { fail_with(x) }",
        &["checked", "tagged"],
    ),
    // A `break` statement ends its block, not the `if` around it.
    (
        "breaks",
        "pub fn digits(n: u64) -> u64 { let mut d: u64 = 1; let mut x = n; loop { if x < 10 { break; } x = x / 10; d = wrapping_add(d, 1); } d }",
        &["digits"],
    ),
    // A sub-slice view passed as an argument needs no element check.
    (
        "views",
        "fn same(in a: Slice[u8], in b: Slice[u8]) -> bool { a.len() == b.len() }
pub fn prefix_same(in x: ChunkSeq[u8], in y: ChunkSeq[u8], n: u64) -> bool { if n <= x.len() && n <= y.len() { same(in x[0..n], in y[0..n]) } else { false } }",
        &["prefix_same"],
    ),
    // A tuple branch's literals take the other branch's integer kinds.
    (
        "joins",
        "pub fn bounds(n: u64, role: u64, next: u64) -> bool { let (lo, hi) = if role == 0 { (0, n) } else { (role, next) }; lo <= hi }
fn stop() -> Never { loop {} }
fn take(x: u64) -> u64 { x }
pub fn pick(t: u64) -> u64 { let c = if t == 1 { 0 } else if t == 2 { 1 } else { stop() }; take(c) }
pub fn pick2(t: u64) -> u64 { let c = if t == 1 { 0 } else { stop() }; take(c) }
pub fn pick3(t: u64) -> u64 { let c = if t == 1 { 0 } else { 1 }; take(c) }
pub fn done(t: u64) -> Result[Unit, u64] { if t == 0 { return Result::Ok(()); } Result::Err(t) }
const REGS: u64 = 4;
pub fn count(k: u64) -> u64 { let mut r = 0; loop { if r == REGS { break; } r = wrapping_add(r, 1); } k }
pub fn count2(k: u64) -> u64 { let mut r = 0; loop { if r == k { break; } r = wrapping_add(r, 1); } k }
pub fn first(k: u64) -> u64 { let mut j: u64 = 0; loop { if j >= k { break 0; } j = wrapping_add(j, 1); } }",
        &["bounds", "count", "count2", "done", "first", "pick", "pick2", "pick3"],
    ),
    (
        "floats",
        "pub fn lerp(a: f64, b: f64, t: f64) -> f64 { a + (b - a) * t }
pub fn half(x: f32) -> f32 { x * 0.5 }",
        &["half", "lerp"],
    ),
];

fn build_driver(dir: &Path) -> PathBuf {
    let package = dir.join("driver");
    std::fs::create_dir_all(package.join("src")).unwrap();
    let path = |p: &str| serde_json::to_string(repo().join(p).to_str().unwrap()).unwrap();
    std::fs::write(
        package.join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/stage2-calls-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
             [dependencies]\n\
             \"@vesper/export-build\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/env\" = {{ version = \"^0.1\", path = {} }}\n\
             \"@vesper-std/fs\" = {{ version = \"^0.1\", path = {} }}\n",
            path("packages/vesper-export-build"),
            path("packages/vesper-std/env"),
            path("packages/vesper-std/fs"),
        ),
    )
    .unwrap();
    std::fs::write(package.join("src/main.ves"), DRIVER).unwrap();
    let binary = dir.join("stage2-calls-driver");
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&package)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    binary
}

/// The calls input the Rust compiler encodes for the package at `dir`'s
/// library task over `roots`, optimized, from its first lowering.
fn rust_input(dir: &Path, roots: &[&str]) -> Result<Vec<u8>, String> {
    let p = vesper::packages::load(dir).map_err(|e| format!("{e:?}"))?;
    if !p.ok() {
        return Err(format!("{:?} {:?}", p.diagnostics, p.manifest_errors));
    }
    let tasks: Vec<(String, u64)> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let place = if i == 0 {
                LIBRARY_TASK
            } else {
                MORE_ROOTS_TASK
            };
            (r.to_string(), OPTIMIZE_TASK | place)
        })
        .collect();
    let encode = |world: &vesper::check::World| {
        let program = vesper::ir_swap::lower(world);
        codegen_calls_words::calls_input_flags_wide(world, &program, &tasks)
            .ok_or_else(|| "Rust does not encode the library".to_string())
    };
    // The `.vsys` imports `vesper build --lib` reads: the root source's
    // `@export vsys` directives and each package's `[package] vsys`
    // (native_export.rs `compile_package_module`).
    let root_source = std::fs::read_to_string(dir.join("src/lib.ves")).unwrap_or_default();
    let mut directives = vesper::native::export::Directives::parse(&root_source, None)
        .map_err(|e| format!("directives: {e}"))?;
    directives
        .add_packages(&p.vsys, None)
        .map_err(|e| format!("directives: {e}"))?;
    if directives.vsys.is_empty() {
        let checked = vesper::check::check(&p.module);
        if let Some(d) = checked.diagnostics.first() {
            return Err(format!(
                "Rust's checker: {} diagnostics, first {d:?}",
                checked.diagnostics.len()
            ));
        }
        return encode(&checked.world);
    }
    // A library with `.vsys` imports checks its `Ffi` calls against them
    // (native_export.rs `compile_foreign`).
    let mut imports = vesper::ffi_lowering::Imports::new();
    for v in &directives.vsys {
        imports
            .add(&v.source, v.handler)
            .map_err(|e| format!("{}: {e}", v.library))?;
    }
    let checked = vesper::ffi_lowering::check_module(&p.module, &imports)
        .map_err(|e| format!("Rust's foreign checker: {e}"))?;
    checked
        .with_lowering(None, encode)
        .map_err(|e| e.to_string())
}

/// What comparing one library found.
#[derive(Default)]
struct Tally {
    same_calls: usize,
    same_fns: usize,
    failures: Vec<String>,
    differing: Vec<String>,
}

/// The two readers of a calls input.
struct Readers {
    calls: Entry,
    dump: Entry,
}

impl Readers {
    fn load() -> Readers {
        Readers {
            calls: Package::load("packages/vesper-codegen-calls").entry("codegen_calls_text", true),
            dump: Package::load("packages/vesper-place-ir").entry("place_ir_dump", true),
        }
    }
}

/// Runs the Stage 2 driver on the package at `package` over `roots` and
/// compares its calls input with Rust's.
fn compare(
    t: &mut Tally,
    driver: &Path,
    readers: &Readers,
    scratch: &Path,
    library: &str,
    package: &Path,
    roots: &[&str],
) {
    let roots_file = scratch.join(format!("{library}.roots"));
    std::fs::write(&roots_file, roots.join("\n")).unwrap();
    let out = scratch.join(format!("{library}.calls"));
    let status = Command::new(driver)
        .arg(package)
        .arg(&roots_file)
        .arg("1")
        .arg(&out)
        .arg(std::env::consts::OS)
        .status()
        .unwrap();
    let written = std::fs::read(&out).unwrap_or_default();
    match status.code() {
        Some(0) => {}
        Some(c) => {
            let why = DECLINES.get(c as usize).copied().unwrap_or("?");
            t.failures.push(format!(
                "{library}: declined: {why}: {}",
                String::from_utf8_lossy(&written)
            ));
            return;
        }
        None => panic!("{library}: the driver was killed"),
    }
    let rust = match rust_input(package, roots) {
        Ok(r) => r,
        Err(e) => {
            t.failures.push(format!("{library}: {e}"));
            return;
        }
    };
    let (rust_calls, stage2_calls) = (readers.calls.text(&rust), readers.calls.text(&written));
    if rust_calls != stage2_calls {
        t.failures.push(format!(
            "{library}: instances or declarations differ\nRust:\n{rust_calls}\nStage 2:\n{stage2_calls}"
        ));
        return;
    }
    t.same_calls += 1;
    let rust_fns = functions(&readers.dump.text(&rust));
    let stage2_fns = functions(&readers.dump.text(&written));
    let mut missing = vec![];
    for (name, body) in &rust_fns {
        match stage2_fns.get(name) {
            Some(b) if same_body(body, b) => t.same_fns += 1,
            Some(b) => {
                if std::env::var_os(SHOW_DIFFS).is_some() {
                    eprintln!(
                        "{library}::{name}\nRust:\n{}\nStage 2:\n{}",
                        body.join("\n"),
                        b.join("\n")
                    );
                }
                t.differing.push(format!("{library}::{name}"))
            }
            None => missing.push(name.as_str()),
        }
    }
    if !missing.is_empty() {
        let extra: Vec<&str> = stage2_fns
            .keys()
            .filter(|n| !rust_fns.contains_key(*n))
            .map(String::as_str)
            .collect();
        t.failures.push(format!(
            "{library}: Stage 2 did not lower {} functions: {}; it lowered {} others: {}",
            missing.len(),
            missing.join(" "),
            extra.len(),
            extra.join(" ")
        ));
    }
}

/// A one-file library as a package directory.
fn one_file_package(scratch: &Path, library: &str, source: &str) -> PathBuf {
    let package = scratch.join(format!("pkg-{library}"));
    std::fs::create_dir_all(package.join("src")).unwrap();
    std::fs::write(
        package.join("Loom.toml"),
        format!(
            "[package]\nname = \"@port/{}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
            library.replace('_', "-")
        ),
    )
    .unwrap();
    std::fs::write(package.join("src/lib.ves"), source).unwrap();
    package
}

#[cfg(unix)]
#[test]
fn stage2_calls_inputs_read_as_rust_ones() {
    let scratch = std::env::temp_dir().join(format!("vesper-stage2-calls-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let driver = build_driver(&scratch);
    let readers = Readers::load();
    let mut t = Tally::default();
    for (library, source, roots) in LIBRARIES {
        let package = one_file_package(&scratch, library, source);
        compare(
            &mut t, &driver, &readers, &scratch, library, &package, roots,
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "stage2 calls inputs: {} of {} libraries with Rust's instances and \
         declarations; {} functions lowered as Rust lowers them, {} differ ({})",
        t.same_calls,
        LIBRARIES.len(),
        t.same_fns,
        t.differing.len(),
        t.differing.join(", ")
    );
    assert!(t.failures.is_empty(), "{}", t.failures.join("\n\n"));
    assert!(
        t.same_fns >= MIN_SAME_FUNCTIONS,
        "only {} functions lowered as Rust lowers them",
        t.same_fns
    );
}

/// `STAGE2_CALLS_PORTS` limits the port corpus to a comma-separated list
/// of libraries (the default is all of them).
const PORTS_ENV: &str = "STAGE2_CALLS_PORTS";

/// `STAGE2_CALLS_FAILURES=<file>` writes every port failure there.
const FAILURES_ENV: &str = "STAGE2_CALLS_FAILURES";

/// The export roots of a port: its interface's functions, in name order,
/// as `vesper build --lib` writes them (`native_export::object`).
fn port_roots(port: &ports::Port, input: &Path, scratch: &Path) -> Result<Vec<String>, String> {
    let library = port.1;
    let json = scratch.join(format!("{library}.json"));
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["build", "--no-opt", "--lib", "--lib-name", library])
        .arg(input)
        .arg("-o")
        .arg(scratch.join(format!("{library}.o")))
        .arg("--interface")
        .arg(&json)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let text = std::fs::read_to_string(&json).map_err(|e| e.to_string())?;
    let interface: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut names: Vec<String> = interface["functions"]
        .as_array()
        .map(|fs| {
            fs.iter()
                .filter_map(|f| f["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

/// The port libraries (tests/self_host_codegen_export/ports.rs), each over
/// its export roots. Reports how far Stage 2's calls inputs are from
/// Rust's; the floor is [`MIN_SAME_PORT_FUNCTIONS`].
#[cfg(unix)]
#[test]
fn stage2_port_calls_inputs_read_as_rust_ones() {
    let scratch =
        std::env::temp_dir().join(format!("vesper-stage2-port-calls-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let driver = build_driver(&scratch);
    let readers = Readers::load();
    let mut t = Tally::default();
    let selected = |library: &str| match std::env::var(PORTS_ENV) {
        Ok(list) if !list.is_empty() => list.split(',').any(|l| l.trim() == library),
        _ => true,
    };
    let mut libraries = 0;
    for port in ports::PORTS.iter().filter(|p| selected(p.1)) {
        let library = port.1;
        libraries += 1;
        let input = ports::port_input(port, &repo(), &scratch).unwrap();
        let roots = match port_roots(port, &input, &scratch) {
            Ok(r) => r,
            Err(e) => {
                t.failures
                    .push(format!("{library}: Rust build failed: {e}"));
                continue;
            }
        };
        let package = if input.is_dir() {
            input
        } else {
            one_file_package(&scratch, library, &std::fs::read_to_string(&input).unwrap())
        };
        let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
        let before = (t.same_fns, t.differing.len(), t.failures.len());
        compare(
            &mut t, &driver, &readers, &scratch, library, &package, &roots,
        );
        eprintln!(
            "{library}: {} same, {} differ{}",
            t.same_fns - before.0,
            t.differing.len() - before.1,
            if t.failures.len() > before.2 {
                format!(", {}", t.failures[before.2].lines().next().unwrap_or(""))
            } else {
                String::new()
            }
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
    if let Some(path) = std::env::var_os(FAILURES_ENV) {
        std::fs::write(path, t.failures.join("\n\n")).unwrap();
    }
    eprintln!(
        "stage2 port calls inputs: {} of {libraries} libraries with Rust's instances and \
         declarations; {} functions lowered as Rust lowers them, {} differ; {} failures",
        t.same_calls,
        t.same_fns,
        t.differing.len(),
        t.failures.len()
    );
    assert!(
        t.same_fns >= MIN_SAME_PORT_FUNCTIONS,
        "only {} port functions lowered as Rust lowers them",
        t.same_fns
    );
}
