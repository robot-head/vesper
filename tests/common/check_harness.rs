//! The shared differential harness of self-hosting wave SH2 (lane 4, #1176).
//!
//! Every SH2 checker port runs a Vesper module against the Rust code it
//! replaces over the compiler's own inputs. This module does the common
//! part: it loads the corpus (every checked single-file program under
//! `tests/fixtures` and `examples`, and every Loom package under `packages`
//! and `crates`), encodes a world's names, types and tables as one `Str` of
//! little-endian words for `@vesper/check-data`'s decoder (`read_ctx` in
//! `packages/vesper-check-data/src/input.ves`), compiles a package entry,
//! calls it with that `Str`, and compares a digest or a text against Rust.
//! It also has the census's IR mutants, a stride for callgrind runs and ABBA
//! timing. `packages/vesper-check-data/README.md` documents it for lanes
//! 5-14; include it with
//! `#[path = "common/check_harness.rs"] mod check_harness;`.
//!
//! Environment:
//! - `CHECK_HARNESS_STRIDE=n` keeps every n-th file and package (callgrind).
//! - `CHECK_HARNESS_MUTANTS=n` adds n IR mutants of each world ([`ir_cases`]).
//! - `CHECK_HARNESS_SPEED=1` runs the ABBA timings of the ignored tests.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vesper::ast::{Effects, Mode, Pass, Type};
use vesper::check::World;
use vesper::{check, native, packages};

pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

// ------------------------------------------------------------------ corpus

/// One checked corpus world: a single-file program or a Loom package.
pub struct Case {
    /// Repository-relative path; packages end in `/`.
    pub name: String,
    pub module: vesper::ast::Module,
    pub world: World,
}

/// The corpus files and package directories, in path order.
fn corpus_paths() -> (Vec<PathBuf>, Vec<PathBuf>) {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>, pkgs: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.path());
        for e in entries {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if matches!(&*name, "target" | ".git" | "node_modules")
                    || name.starts_with("bazel-")
                {
                    continue;
                }
                if p.join("Loom.toml").exists() {
                    pkgs.push(p.clone());
                    continue;
                }
                walk(&p, files, pkgs);
            } else if p.extension().is_some_and(|e| e == "ves") {
                files.push(p);
            }
        }
    }
    let mut files = vec![];
    let mut pkgs = vec![];
    for d in ["tests/fixtures", "examples", "packages", "crates"] {
        walk(&repo().join(d), &mut files, &mut pkgs);
    }
    let stride = env_usize("CHECK_HARNESS_STRIDE", 1).max(1);
    (
        files.into_iter().step_by(stride).collect(),
        pkgs.into_iter().step_by(stride).collect(),
    )
}

fn relative(p: &Path) -> String {
    p.strip_prefix(repo())
        .unwrap_or(p)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// `f` of every corpus program that parses and checks with no diagnostics
/// (the programs of [`corpus`]), given the module and its expression types,
/// in corpus order. Each program is parsed and checked once, on a few
/// worker threads, and nothing is kept: for tests that need the checker's
/// types, which [`corpus`] does not keep. A panic in parse, check or `f`
/// skips the program.
#[allow(dead_code)]
pub fn map_checked_corpus<T: Send>(
    f: impl Fn(vesper::ast::Module, &vesper::ast::TypeMap) -> T + Sync,
) -> Vec<T> {
    let (files, pkgs) = corpus_paths();
    let programs: Vec<(PathBuf, bool)> = files
        .into_iter()
        .map(|p| (p, false))
        .chain(pkgs.into_iter().map(|p| (p, true)))
        .collect();
    let one = |path: &Path, package: bool| -> Option<T> {
        let module = if package {
            let a = packages::load(path).ok()?;
            if !a.ok() {
                return None;
            }
            a.module
        } else {
            let src = std::fs::read_to_string(path).ok()?;
            if src.len() > 1 << 20 {
                return None;
            }
            let parsed = vesper::parser::parse(&src);
            if !parsed.diagnostics.is_empty() {
                return None;
            }
            parsed.module
        };
        let checked = check::check(&module);
        if !checked.diagnostics.is_empty() {
            return None;
        }
        Some(f(module, &checked.expression_types))
    };
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut out: Vec<(usize, T)> = std::thread::scope(|scope| {
        let (programs, one) = (&programs, &one);
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                std::thread::Builder::new()
                    .stack_size(256 << 20)
                    .spawn_scoped(scope, move || {
                        (w..programs.len())
                            .step_by(workers)
                            .filter_map(|i| {
                                let (p, package) = &programs[i];
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    one(p, *package)
                                }))
                                .ok()
                                .flatten()
                                .map(|t| (i, t))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap()
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    std::panic::set_hook(quiet);
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, t)| t).collect()
}

/// Every corpus program that parses and checks with no diagnostics, loaded
/// once per test binary. Under Bazel the corpus is what the test's data
/// declares (the root package's files), so its size differs from Cargo's.
pub fn corpus() -> &'static [Case] {
    static CORPUS: OnceLock<Vec<Case>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let (files, pkgs) = corpus_paths();
        let mut out = vec![];
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            if src.len() > 1 << 20 {
                continue;
            }
            let r = std::panic::catch_unwind(|| {
                let parsed = vesper::parser::parse(&src);
                if !parsed.diagnostics.is_empty() {
                    return None;
                }
                let checked = check::check(&parsed.module);
                checked
                    .diagnostics
                    .is_empty()
                    .then_some((parsed.module, checked.world))
            });
            if let Ok(Some((module, world))) = r {
                out.push(Case {
                    name: relative(&f),
                    module,
                    world,
                });
            }
        }
        for p in pkgs {
            let r = std::panic::catch_unwind(|| {
                let a = packages::load(&p).ok()?;
                if !a.ok() {
                    return None;
                }
                let checked = check::check(&a.module);
                checked
                    .diagnostics
                    .is_empty()
                    .then_some((a.module, checked.world))
            });
            if let Ok(Some((module, world))) = r {
                out.push(Case {
                    name: relative(&p) + "/",
                    module,
                    world,
                });
            }
        }
        std::panic::set_hook(quiet);
        out
    })
}

/// Every type a world mentions, each once (by `Debug` text), in a fixed
/// order: ADT self types and field types, function signatures, then the
/// expression types.
pub fn world_types(w: &World) -> Vec<Type> {
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    let mut add = |t: &Type, out: &mut Vec<Type>| {
        if seen.insert(format!("{t:?}")) {
            out.push(t.clone());
        }
    };
    for a in w.adts.values() {
        add(
            &Type::Named(
                a.name.clone(),
                a.generics.iter().map(|g| Type::named(&g.name)).collect(),
            ),
            &mut out,
        );
        for v in &a.variants {
            for f in &v.fields {
                add(&f.ty, &mut out);
            }
        }
    }
    for f in w.functions.values() {
        for p in &f.params {
            add(&p.ty, &mut out);
        }
        add(&f.result, &mut out);
    }
    for t in w.types.values() {
        add(t, &mut out);
    }
    out
}

// ------------------------------------------------------------------ digests

pub const SEED: u64 = 14695981039346656037;

/// FNV-1a over one word: the fold every digest uses, matched bit for bit by
/// `fold` in `packages/vesper-check-data/src/util.ves`.
pub fn fold(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(1099511628211)
}

/// The bytes of `b` then 255 (`fold_bytes` in util.ves).
pub fn fold_bytes(h: u64, b: &[u8]) -> u64 {
    let mut x = h;
    for c in b {
        x = fold(x, *c as u64);
    }
    fold(x, 255)
}

/// The digest of a diagnostic: code, message, span, related labels and help
/// (`Diags::digest_row` in diag.ves).
pub fn diag_digest(h0: u64, d: &vesper::ast::Diagnostic) -> u64 {
    let mut h = fold_bytes(h0, d.code.as_bytes());
    h = fold_bytes(h, d.message.as_bytes());
    h = fold(fold(h, d.span.start as u64), d.span.end as u64);
    h = fold(h, d.related.len() as u64);
    for l in &d.related {
        h = fold(fold(h, l.span.start as u64), l.span.end as u64);
        h = fold_bytes(h, l.message.as_bytes());
    }
    fold_bytes(h, d.help.as_deref().unwrap_or("").as_bytes())
}

// ------------------------------------------------------------------ encoder

/// The input encoder. Names and types are interned as they are first
/// written, types in post-order (a type's parts before it), so the decoder
/// rebuilds the arena in one pass. [`Enc::finish`] lays out the sections
/// `read_ctx` reads: names, types, the world, then the task's words.
#[derive(Default)]
pub struct Enc {
    pub names: BTreeMap<String, u64>,
    pub name_list: Vec<String>,
    pub types: BTreeMap<String, u64>,
    pub type_words: Vec<u64>,
    pub type_count: u64,
}

pub fn mode_word(m: Mode) -> u64 {
    match m {
        Mode::Copy => 0,
        Mode::Affine => 1,
        Mode::Linear => 2,
    }
}

pub fn pass_word(p: Pass) -> u64 {
    match p {
        Pass::In => 0,
        Pass::Inout => 1,
        Pass::Sink => 2,
    }
}

impl Enc {
    /// The index of name `s`.
    pub fn name(&mut self, s: &str) -> u64 {
        if let Some(i) = self.names.get(s) {
            return *i;
        }
        let i = self.name_list.len() as u64;
        self.names.insert(s.into(), i);
        self.name_list.push(s.into());
        i
    }

    /// 0 for `None`, else the name's index + 1.
    pub fn opt_name(&mut self, s: &Option<String>) -> u64 {
        s.as_ref().map_or(0, |s| self.name(s) + 1)
    }

    fn row(&mut self, e: &Effects, out: &mut Vec<u64>) {
        let mut words = vec![e.instances.len() as u64];
        for i in &e.instances {
            let args: Vec<u64> = i.args.iter().map(|t| self.ty(t)).collect();
            words.push(self.opt_name(&i.label));
            words.push(self.name(&i.name));
            words.push(args.len() as u64);
            words.extend(args);
        }
        words.push(self.opt_name(&e.tail));
        out.extend(words);
    }

    /// The index of type `t` (its parts are written first).
    pub fn ty(&mut self, t: &Type) -> u64 {
        let key = format!("{t:?}");
        if let Some(i) = self.types.get(&key) {
            return *i;
        }
        let mut words = vec![];
        match t {
            Type::IntLiteral(n) => {
                // Until i128 lands (SH2 lane 2), a sign and a 64-bit magnitude.
                assert!(
                    n.unsigned_abs() <= u64::MAX as u128,
                    "literal {n} exceeds 64 bits"
                );
                words.extend([0, (*n < 0) as u64, n.unsigned_abs() as u64]);
            }
            Type::Named(n, a) => {
                let args: Vec<u64> = a.iter().map(|t| self.ty(t)).collect();
                words.extend([1, self.name(n), args.len() as u64]);
                words.extend(args);
            }
            Type::Tuple(a) => {
                let args: Vec<u64> = a.iter().map(|t| self.ty(t)).collect();
                words.extend([2, args.len() as u64]);
                words.extend(args);
            }
            Type::Function {
                params,
                result,
                effects,
                mode,
            } => {
                let ps: Vec<(u64, u64)> = params
                    .iter()
                    .map(|(p, t)| (pass_word(*p), self.ty(t)))
                    .collect();
                let r = self.ty(result);
                words.extend([3, ps.len() as u64]);
                for (p, t) in ps {
                    words.extend([p, t]);
                }
                words.push(r);
                self.row(effects, &mut words);
                words.push(mode_word(*mode));
            }
            Type::Row(e) => {
                words.push(4);
                self.row(e, &mut words);
            }
            Type::Infer => words.push(5),
        }
        let i = self.type_count;
        self.type_count += 1;
        self.types.insert(key, i);
        self.type_words.extend(words);
        i
    }

    fn spans(out: &mut Vec<u64>, spans: &BTreeSet<(usize, usize)>) {
        out.push(spans.len() as u64);
        for (a, b) in spans {
            out.extend([*a as u64, *b as u64]);
        }
    }

    /// The world section: `fip::active`, the ADT table, the effect
    /// declarations, the expression types and the partial and overflowing
    /// operations (`read_world` in input.ves).
    pub fn world(&mut self, w: &World) -> Vec<u64> {
        let mut out = vec![vesper::fip::active(w) as u64, w.adts.len() as u64];
        for a in w.adts.values() {
            out.push(self.name(&a.name));
            out.push(mode_word(a.mode));
            out.push(a.generics.len() as u64);
            for g in &a.generics {
                out.push(self.name(&g.name));
                out.push(mode_word(g.bound));
                out.push(g.row as u64);
            }
            out.push(a.is_enum as u64);
            out.push(a.tile_width.map_or(0, |x| x as u64 + 1));
            out.push((a.span == (0..0)) as u64);
            out.push(a.variants.len() as u64);
            for v in &a.variants {
                out.push(self.name(&v.name));
                out.push(v.named as u64);
                out.push(v.fields.len() as u64);
                for f in &v.fields {
                    out.push(self.name(&f.name));
                    out.push(self.ty(&f.ty));
                    out.push(f.modality.map_or(0, |m| mode_word(m) + 1));
                }
            }
        }
        out.push(w.effects.len() as u64);
        for (name, d) in &w.effects {
            out.push(self.name(name));
            out.push(d.linear as u64);
        }
        out.push(w.types.len() as u64);
        for ((a, b), t) in &w.types {
            out.extend([*a as u64, *b as u64]);
            out.push(self.ty(t));
        }
        Self::spans(&mut out, &w.partial_ops);
        Self::spans(&mut out, &w.overflow_ops);
        out
    }

    /// The whole input: names, types, the world section, then `tail`.
    pub fn finish(&self, world: &[u64], tail: &[u64]) -> Vec<u8> {
        let mut out: Vec<u8> =
            Vec::with_capacity(8 * (world.len() + tail.len() + self.type_words.len()));
        let put = |out: &mut Vec<u8>, x: u64| out.extend(x.to_le_bytes());
        put(&mut out, self.name_list.len() as u64);
        for n in &self.name_list {
            put(&mut out, n.len() as u64);
            out.extend(n.as_bytes());
            while !out.len().is_multiple_of(8) {
                out.push(0);
            }
        }
        put(&mut out, self.type_count);
        for x in &self.type_words {
            put(&mut out, *x);
        }
        for x in world {
            put(&mut out, *x);
        }
        for x in tail {
            put(&mut out, *x);
        }
        out
    }
}

/// Encodes `world` and a task whose words `task` writes (it may intern more
/// names and types through the encoder).
pub fn encode(world: &World, task: impl FnOnce(&mut Enc, &mut Vec<u64>)) -> Vec<u8> {
    let mut e = Enc::default();
    let w = e.world(world);
    let mut tail = vec![];
    task(&mut e, &mut tail);
    e.finish(&w, &tail)
}

// ------------------------------------------------------------------ packages

/// A checked Loom package whose entries the harness calls.
pub struct Package {
    pub world: World,
}

impl Package {
    /// Loads and checks the package at `dir` (repository-relative).
    pub fn load(dir: &str) -> Self {
        let assembled = packages::load(&repo().join(dir)).unwrap();
        assert!(
            assembled.ok(),
            "{:?} {:?}",
            assembled.diagnostics,
            assembled.manifest_errors
        );
        let checked = check::check(&assembled.module);
        let shown: Vec<_> = checked
            .diagnostics
            .iter()
            .map(|d| assembled.sources.localize(d))
            .collect();
        assert!(checked.diagnostics.is_empty(), "{shown:?}");
        Self {
            world: checked.world,
        }
    }

    /// The JIT entry `name`, which takes one `in Str`.
    pub fn entry(&self, name: &str, optimize: bool) -> Entry {
        Entry(
            native::WordFunction::compile(&self.world, name, optimize)
                .unwrap_or_else(|e| panic!("{name}: {e}")),
        )
    }
}

/// `@vesper/check-data`, checked once per test binary.
pub fn check_data() -> &'static Package {
    static PACKAGE: OnceLock<Package> = OnceLock::new();
    PACKAGE.get_or_init(|| Package::load("packages/vesper-check-data"))
}

/// A compiled entry that takes one `in Str`.
pub struct Entry(pub native::WordFunction);

impl Entry {
    /// Calls the entry on `input` and returns its word (a digest, a count).
    pub fn word(&self, input: &[u8]) -> u64 {
        let words = [input.len() as u64, 0, input.as_ptr() as u64];
        // SAFETY: the entry takes one `in Str`, passed as the address of its
        // three words `{len, 0, data}`, which live across the call, as does
        // `input`.
        unsafe { self.0.call(&[words.as_ptr() as u64]) }.unwrap()
    }

    /// Calls an entry returning `ChunkSeq[u64]`, copies and frees it.
    pub fn words(&self, input: &[u8]) -> Vec<u64> {
        take_words(self.word(input))
    }

    /// Calls an entry returning `pack_text`'s packing (lib.ves).
    pub fn text(&self, input: &[u8]) -> String {
        unpack_text(&self.words(input))
    }
}

/// Reads and frees a returned `ChunkSeq[u64]` (header `{len, cap, data}`).
pub fn take_words(handle: u64) -> Vec<u64> {
    // SAFETY: `handle` is a live sequence header the entry returned, whose
    // `data` holds `len` packed u64 elements.
    let words = unsafe {
        let header = handle as *const u64;
        let len = *header as usize;
        let data = *header.add(2) as *const u64;
        if len == 0 {
            vec![]
        } else {
            std::slice::from_raw_parts(data, len).to_vec()
        }
    };
    vesper::seq::native_runtime::vesper_seq_free(handle);
    words
}

/// The text of `pack_text`: a byte length, then the bytes eight per word.
pub fn unpack_text(words: &[u64]) -> String {
    let len = words[0] as usize;
    let mut bytes = Vec::with_capacity(len + 8);
    for w in &words[1..] {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes.truncate(len);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The first differing line of two texts, with its number.
pub fn first_difference(rust: &str, vesper: &str) -> Option<String> {
    let r: Vec<&str> = rust.lines().collect();
    let v: Vec<&str> = vesper.lines().collect();
    for i in 0..r.len().max(v.len()) {
        let (a, b) = (r.get(i).copied(), v.get(i).copied());
        if a != b {
            return Some(format!(
                "line {}:\n  rust:   {}\n  vesper: {}",
                i + 1,
                a.unwrap_or("<end>"),
                b.unwrap_or("<end>")
            ));
        }
    }
    None
}

/// Runs `vesper` and `rust` (each producing a text) on every corpus case
/// and returns how many differ, printing the first few differences.
pub fn differential(
    label: &str,
    mut rust: impl FnMut(&Case) -> String,
    mut vesper: impl FnMut(&Case) -> String,
) -> usize {
    let corpus = corpus();
    let mut bad = 0;
    let mut lines = 0;
    for c in corpus {
        let r = rust(c);
        let v = vesper(c);
        lines += r.lines().count();
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 5 {
                eprintln!("{label}: {} differs at {d}", c.name);
            }
        }
    }
    eprintln!(
        "{label}: {bad} of {} worlds differ ({lines} lines)",
        corpus.len()
    );
    bad
}

// ------------------------------------------------------------------ mutants

/// The census's IR mutants: copies of a lowered program that make the IR
/// checker's diagnostics fire (the clean corpus produces none). `m` is 1 to
/// 5: an `In` read made `Sink`, `Sink` made `Inout`, binds made wildcards,
/// tokens dropped, call arguments sunk with an early return.
pub fn ir_mutant(p: &vesper::ir::Program, m: usize) -> vesper::ir::Program {
    use vesper::ast::PatternKind;
    use vesper::ir::{Op, Terminator};
    let mut p = p.clone();
    for f in p.functions.values_mut() {
        let first = f
            .blocks
            .first()
            .and_then(|b| b.instructions.first())
            .map(|i| i.dest);
        for (bi, b) in f.blocks.iter_mut().enumerate() {
            for (k, i) in b.instructions.iter_mut().enumerate() {
                match (m, &mut i.op) {
                    (1, Op::Read { pass, .. }) if *pass == Pass::In => *pass = Pass::Sink,
                    (2, Op::Read { pass, .. }) if *pass == Pass::Sink => *pass = Pass::Inout,
                    (3, Op::Bind { pattern, .. }) => pattern.kind = PatternKind::Wild,
                    (4, _) if (k + bi) % 7 == 3 => i.token = None,
                    (5, Op::Call { args, .. }) => {
                        for a in args.iter_mut() {
                            a.pass = Pass::Sink;
                        }
                    }
                    _ => {}
                }
            }
            if m == 5
                && let (Terminator::Return(v), Some(x)) = (&mut b.terminator, first)
            {
                *v = x;
            }
            if m == 4 && bi % 3 == 1 {
                b.incoming_tokens.reverse();
                b.outgoing_token = b.outgoing_token.wrapping_add(1);
            }
        }
    }
    p
}

pub const IR_MUTANTS: usize = 5;

/// A corpus world with its lowered (possibly mutated) IR program.
pub struct IrCase<'a> {
    pub name: String,
    pub world: &'a World,
    pub program: vesper::ir::Program,
}

/// Each corpus world's IR, followed by `CHECK_HARNESS_MUTANTS` (0 to 5,
/// default `mutants`) of its mutants.
pub fn ir_cases(mutants: usize) -> Vec<IrCase<'static>> {
    let mutants = env_usize("CHECK_HARNESS_MUTANTS", mutants).min(IR_MUTANTS);
    let mut out = vec![];
    for c in corpus() {
        let program = vesper::ir::lower_world(&c.world);
        for m in 1..=mutants {
            out.push(IrCase {
                name: format!("{}#m{m}", c.name),
                world: &c.world,
                program: ir_mutant(&program, m),
            });
        }
        out.push(IrCase {
            name: c.name.clone(),
            world: &c.world,
            program,
        });
    }
    out
}

// ------------------------------------------------------------------ timing

/// ABBA timing, best of six runs per side; returns (rust, vesper). Wall
/// clock on the cloud hosts moves by up to 30% between runs.
pub fn abba(
    label: &str,
    rust: &mut dyn FnMut() -> Duration,
    vesper: &mut dyn FnMut() -> Duration,
) -> (Duration, Duration) {
    let mut rs = vec![];
    let mut vs = vec![];
    for _ in 0..3 {
        rs.push(rust());
        vs.push(vesper());
        vs.push(vesper());
        rs.push(rust());
    }
    let r = *rs.iter().min().unwrap();
    let v = *vs.iter().min().unwrap();
    eprintln!(
        "ABBA {label}: rust {r:?} vesper {v:?} ratio {:.2}",
        v.as_secs_f64() / r.as_secs_f64()
    );
    (r, v)
}

/// The wall time of `f`.
pub fn time(f: impl FnOnce()) -> Duration {
    let t = Instant::now();
    f();
    t.elapsed()
}

/// Whether the ignored tests should time (`CHECK_HARNESS_SPEED` set).
pub fn speed() -> bool {
    std::env::var_os("CHECK_HARNESS_SPEED").is_some()
}
