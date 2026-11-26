//! `@vesper/verified-codegen` (packages/vesper-verified-codegen) against
//! `src/verified_codegen` (#1886, part 1: the slot IR and instruction
//! selection of `machine.rs`).
//!
//! - The Vesper `table_listing` must equal Rust's byte for byte (and
//!   tests/verified_codegen_lean.rs checks Rust's against the Lean table).
//! - `select` must give the same instructions, in `M`'s display text, on
//!   every table row, on every slot-IR row the Rust backend lowers from the
//!   corpus `main`s, and on random rows over the whole field range.
//!
//! - Part 2: the executable `compile` builds from the slot IR (the
//!   assembler, the x86-64 encoder and the ELF container) must be the same
//!   bytes, for every corpus `main` the Rust backend builds, under both
//!   result protocols.
//!
//! - Part 3: `certificate::check` (widen, generate, validate) must give the
//!   same certificate or rejection on the corpus and on random programs,
//!   and `validate` the same verdict on mutated certificates.
//!
//! - Part 4a: the plan and the lowering must give the same slot IR or
//!   error text on the corpus (programs without handler sites).
//! - Part 4b: monomorphization must give the same program (canonical dump)
//!   or error text on the corpus.
//!
//! - `VERIFIED_CODEGEN_STRIDE=n` keeps every n-th corpus world.
//! - `CHECK_HARNESS_SPEED=1` runs the ABBA timing (release build).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/check_words.rs"]
mod check_words;
#[path = "common/ir_words.rs"]
mod ir_words;

use check_harness::*;
use std::sync::OnceLock;
use vesper::verified_codegen::{self as verified, Lir};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-verified-codegen"))
}

fn entry(name: &str) -> Entry {
    package().entry(name, true)
}

fn arith(op: verified::ArithOp) -> u64 {
    op as u64
}

fn kind(k: verified::Kind) -> u64 {
    k as u64
}

/// One row as the package reads it: `tag | op << 8 | kind << 16`, then the
/// variant's other fields in declaration order.
fn encode_row(row: &Lir, out: &mut Vec<u64>) {
    let head = |tag: u64, op: u64, k: u64| tag | op << 8 | k << 16;
    let w = |x: u32| u64::from(x);
    let words: [u64; 4] = match *row {
        Lir::Const { dst, word } => [head(0, 0, 0), w(dst), word, 0],
        Lir::Move { dst, src } => [head(1, 0, 0), w(dst), w(src), 0],
        Lir::Arith {
            op,
            kind: k,
            dst,
            a,
            b,
        } => [head(2, arith(op), kind(k)), w(dst), w(a), w(b)],
        Lir::Wrapping {
            op,
            kind: k,
            dst,
            a,
            b,
        } => [head(3, op as u64, kind(k)), w(dst), w(a), w(b)],
        Lir::Neg { kind: k, dst, a } => [head(4, 0, kind(k)), w(dst), w(a), 0],
        Lir::Not { dst, a } => [head(5, 0, 0), w(dst), w(a), 0],
        Lir::Compare {
            cond,
            kind: k,
            dst,
            a,
            b,
        } => [head(6, cond as u64, kind(k)), w(dst), w(a), w(b)],
        Lir::FArith { op, dst, a, b } => [head(7, op as u64, 0), w(dst), w(a), w(b)],
        Lir::FNeg { dst, a } => [head(8, 0, 0), w(dst), w(a), 0],
        Lir::FCompare { cond, dst, a, b } => [head(9, cond as u64, 0), w(dst), w(a), w(b)],
        Lir::StoreOut { index, src } => [head(10, 0, 0), w(index), w(src), 0],
        Lir::LoadOut { dst, index } => [head(11, 0, 0), w(dst), w(index), 0],
        Lir::LoadIn { dst, index } => [head(12, 0, 0), w(dst), w(index), 0],
        Lir::StoreIn { index, src } => [head(13, 0, 0), w(index), w(src), 0],
        Lir::LoadPtr { dst, ptr, offset } => [head(14, 0, 0), w(dst), w(ptr), w(offset)],
        Lir::StorePtr { ptr, offset, src } => [head(15, 0, 0), w(ptr), w(offset), w(src)],
        Lir::Call { callee } => [head(16, 0, 0), w(callee), 0, 0],
        Lir::Label(l) => [head(17, 0, 0), w(l), 0, 0],
        Lir::Jump(l) => [head(18, 0, 0), w(l), 0, 0],
        Lir::BranchZero { cond, target } => [head(19, 0, 0), w(cond), w(target), 0],
        Lir::Enter { words } => [head(20, 0, 0), w(words), 0, 0],
        Lir::Return => [head(21, 0, 0), 0, 0, 0],
        Lir::Trap(k) => [head(22, k as u64, 0), 0, 0, 0],
        Lir::Scope { ovf, div0 } => [head(23, 0, 0), w(ovf), w(div0), 0],
    };
    out.extend(words);
}

fn input(rows: &[Lir]) -> Vec<u8> {
    let mut words = vec![rows.len() as u64];
    for r in rows {
        encode_row(r, &mut words);
    }
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Rust's selection of every row, one `<m>; <m>; ...` line each.
fn rust_text(rows: &[Lir]) -> String {
    let mut out = String::new();
    for r in rows {
        let ms: Vec<String> = verified::select(r)
            .iter()
            .map(ToString::to_string)
            .collect();
        out.push_str(&ms.join("; "));
        out.push('\n');
    }
    out
}

fn vesper_text(select: &Entry, rows: &[Lir]) -> String {
    select.text(&input(rows))
}

#[test]
fn table_listing_matches_rust() {
    let vesper = entry("table_listing_text").text(b"");
    let rust = verified::table_listing();
    assert!(rust.lines().count() > 90, "{rust}");
    if let Some(d) = first_difference(&rust, &vesper) {
        panic!("table listing differs: {d}");
    }
    assert_eq!(rust, vesper);
}

fn stride() -> usize {
    std::env::var("VERIFIED_CODEGEN_STRIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// Every slot-IR row the Rust backend lowers from the corpus `main`s it
/// supports (host target, default options).
fn corpus_rows() -> (usize, Vec<Lir>) {
    let Some(target) = verified::Target::host() else {
        return (0, vec![]);
    };
    let options = verified::Options::new(target);
    let mut programs = 0;
    let mut rows = vec![];
    for case in corpus().iter().step_by(stride()) {
        if let Ok(artifact) = verified::compile(&case.world, "main", &options) {
            programs += 1;
            for (_, f) in artifact.lir {
                rows.extend(f);
            }
        }
    }
    (programs, rows)
}

/// Deterministic random rows over the whole range of every field.
fn random_rows(n: usize) -> Vec<Lir> {
    use verified::{ArithOp, Cond, FloatOp, Kind, TrapKind, WrapOp};
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        let r = next();
        let x = next() as u32;
        let y = (next() >> 7) as u32;
        let z = (next() >> 40) as u32;
        let kinds = Kind::INTEGERS;
        let k = kinds[(r >> 8) as usize % kinds.len()];
        let cmp_kinds = [Kind::U8, Kind::U32, Kind::U64, Kind::I64, Kind::Bool];
        let ck = cmp_kinds[(r >> 8) as usize % cmp_kinds.len()];
        let cond = Cond::ALL[(r >> 16) as usize % Cond::ALL.len()];
        let row = match r % 24 {
            0 => Lir::Const {
                dst: x,
                word: next(),
            },
            1 => Lir::Move { dst: x, src: y },
            2 => Lir::Arith {
                op: ArithOp::ALL[(r >> 16) as usize % ArithOp::ALL.len()],
                kind: k,
                dst: x,
                a: y,
                b: z,
            },
            3 => Lir::Wrapping {
                op: WrapOp::ALL[(r >> 16) as usize % WrapOp::ALL.len()],
                kind: k,
                dst: x,
                a: y,
                b: z,
            },
            4 => Lir::Neg {
                kind: k,
                dst: x,
                a: y,
            },
            5 => Lir::Not { dst: x, a: y },
            6 => Lir::Compare {
                cond,
                kind: ck,
                dst: x,
                a: y,
                b: z,
            },
            7 => Lir::FArith {
                op: FloatOp::ALL[(r >> 16) as usize % FloatOp::ALL.len()],
                dst: x,
                a: y,
                b: z,
            },
            8 => Lir::FNeg { dst: x, a: y },
            9 => Lir::FCompare {
                cond,
                dst: x,
                a: y,
                b: z,
            },
            10 => Lir::StoreOut { index: x, src: y },
            11 => Lir::LoadOut { dst: x, index: y },
            12 => Lir::LoadIn { dst: x, index: y },
            13 => Lir::StoreIn { index: x, src: y },
            14 => Lir::LoadPtr {
                dst: x,
                ptr: y,
                offset: z,
            },
            15 => Lir::StorePtr {
                ptr: x,
                offset: y,
                src: z,
            },
            16 => Lir::Call { callee: x },
            17 => Lir::Label(x),
            18 => Lir::Jump(x),
            19 => Lir::BranchZero { cond: x, target: y },
            20 => Lir::Enter { words: x },
            21 => Lir::Return,
            22 => Lir::Trap(TrapKind::ALL[(r >> 16) as usize % TrapKind::ALL.len()]),
            _ => Lir::Scope { ovf: x, div0: y },
        };
        rows.push(row);
    }
    rows
}

fn compare(what: &str, rows: &[Lir]) -> usize {
    let select = entry("select_text");
    let rust = rust_text(rows);
    let vesper = vesper_text(&select, rows);
    let bad = rust
        .lines()
        .zip(vesper.lines())
        .filter(|(a, b)| a != b)
        .count()
        + rust.lines().count().abs_diff(vesper.lines().count());
    if let Some(d) = first_difference(&rust, &vesper) {
        eprintln!("verified select ({what}): first difference {d}");
    }
    bad
}

#[test]
fn select_matches_rust_on_the_table_and_random_rows() {
    let table: Vec<Lir> = verified::table_rows().into_iter().map(|(_, r)| r).collect();
    assert_eq!(compare("table", &table), 0);
    let random = random_rows(20_000);
    let bad = compare("random", &random);
    eprintln!(
        "verified select: {bad} of {} random rows differ",
        random.len()
    );
    assert_eq!(bad, 0);
}

#[test]
fn select_matches_rust_on_the_corpus() {
    let (programs, rows) = corpus_rows();
    if verified::Target::host().is_none() {
        eprintln!("verified select: no verified target for this host");
        return;
    }
    assert!(programs > 20, "only {programs} corpus programs lower");
    let bad = compare("corpus", &rows);
    eprintln!(
        "verified select corpus: {bad} of {} rows differ over {programs} programs",
        rows.len()
    );
    println!(
        "flip-gate differential differences={bad} cases={}",
        rows.len()
    );
    assert_eq!(bad, 0);
}

/// The entry's label: the `Label` row right before the entry's `Enter`.
fn entry_label(rows: &[Lir]) -> u32 {
    let enter = rows
        .iter()
        .position(|r| matches!(r, Lir::Enter { .. }))
        .expect("the entry has a prologue");
    match rows[..enter].last() {
        Some(Lir::Label(l)) => *l,
        other => panic!("the entry's prologue follows {other:?}"),
    }
}

/// `build_words`'s input for an artifact the Rust backend built.
fn build_input(options: &verified::Options, artifact: &verified::Artifact) -> Vec<u8> {
    let arch = match options.target.arch {
        verified::Arch::X86_64 => 0,
        verified::Arch::Aarch64 => 1,
        verified::Arch::Riscv64 => 2,
    };
    let os = match options.target.os {
        verified::Os::Linux => 0,
        verified::Os::MacOs => 1,
        verified::Os::Windows => 2,
    };
    let protocol = match options.protocol {
        verified::ResultProtocol::ExitStatus => 0,
        verified::ResultProtocol::Words => 1,
    };
    let mut words = vec![
        arch,
        os,
        protocol,
        artifact.result_words.len() as u64,
        u64::from(entry_label(&artifact.lir[0].1)),
        options.max_code_bytes as u64,
        artifact.lir.len() as u64,
    ];
    for (_, rows) in &artifact.lir {
        words.push(rows.len() as u64);
        for r in rows {
            encode_row(r, &mut words);
        }
    }
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The status and bytes `build_words` returns.
fn vesper_build(build: &Entry, input: &[u8]) -> (u64, Vec<u8>) {
    let words = build.words(input);
    let status = words[0];
    let len = words[1] as usize;
    let mut bytes: Vec<u8> = words[2..].iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.truncate(len);
    (status, bytes)
}

/// Every corpus `main` the Rust backend builds for `triple`, under both
/// result protocols.
fn corpus_builds(triple: &str) -> Vec<(String, verified::Options, verified::Artifact)> {
    let target = verified::Target::from_triple(triple).unwrap();
    let mut out = vec![];
    for case in corpus().iter().step_by(stride()) {
        for protocol in [
            verified::ResultProtocol::ExitStatus,
            verified::ResultProtocol::Words,
        ] {
            let options = verified::Options {
                protocol,
                ..verified::Options::new(target)
            };
            if let Ok(artifact) = verified::compile(&case.world, "main", &options) {
                out.push((case.name.clone(), options, artifact));
            }
        }
    }
    out
}

/// Every corpus build for `triple`: the Vesper back half against the Rust
/// backend's bytes. Returns how many differ.
fn executables_match_rust(triple: &str, what: &str) -> usize {
    let build = entry("build_words");
    let builds = corpus_builds(triple);
    assert!(builds.len() > 40, "only {} {what} builds", builds.len());
    let mut bad = 0;
    for (name, options, artifact) in &builds {
        let (status, bytes) = vesper_build(&build, &build_input(options, artifact));
        if status != 0 || bytes != artifact.bytes {
            bad += 1;
            if bad <= 6 {
                let at = bytes
                    .iter()
                    .zip(&artifact.bytes)
                    .position(|(a, b)| a != b)
                    .unwrap_or(bytes.len().min(artifact.bytes.len()));
                eprintln!(
                    "verified {what} {name} ({:?}): status {status}, {} vs {} bytes, first difference at {at}",
                    options.protocol,
                    bytes.len(),
                    artifact.bytes.len()
                );
            }
        }
    }
    eprintln!(
        "verified {what}: {bad} of {} executables differ",
        builds.len()
    );
    println!(
        "flip-gate differential differences={bad} cases={}",
        builds.len()
    );
    bad
}

#[test]
fn x86_64_linux_executables_match_rust() {
    assert_eq!(
        executables_match_rust("x86_64-unknown-linux-gnu", "x86-64 Linux"),
        0
    );
}

#[test]
fn aarch64_linux_executables_match_rust() {
    assert_eq!(
        executables_match_rust("aarch64-unknown-linux-gnu", "AArch64 Linux"),
        0
    );
}

#[test]
fn aarch64_macos_objects_match_rust() {
    assert_eq!(
        executables_match_rust("aarch64-apple-darwin", "AArch64 macOS"),
        0
    );
}

#[test]
fn x86_64_windows_objects_match_rust() {
    assert_eq!(
        executables_match_rust("x86_64-pc-windows-msvc", "x86-64 Windows"),
        0
    );
}

#[test]
fn riscv64_linux_executables_match_rust() {
    assert_eq!(
        executables_match_rust("riscv64gc-unknown-linux-gnu", "RV64 Linux"),
        0
    );
}

/// ABBA: Rust's `select` with `M`'s display against the Vesper entry with
/// its input encoding and text unpacking, over the corpus rows and the
/// table, many rounds.
#[test]
fn flip_gate() {
    if !speed() {
        return;
    }
    let (_, mut rows) = corpus_rows();
    rows.extend(verified::table_rows().into_iter().map(|(_, r)| r));
    let select = entry("select_text");
    let rust = || {
        time(|| {
            for _ in 0..20 {
                std::hint::black_box(rust_text(&rows));
            }
        })
        .as_nanos()
    };
    let vesper = || {
        time(|| {
            for _ in 0..20 {
                std::hint::black_box(vesper_text(&select, &rows));
            }
        })
        .as_nanos()
    };
    for _ in 0..12 {
        let (a, b, c, d) = (rust(), vesper(), vesper(), rust());
        println!("flip-gate pairs rust={} vesper={}", a + d, b + c);
    }
}

// ------------------------------------------------------------ certificate
//
// Part 3: `certificate::check` (widen, generate, validate) must give the
// same certificate word for word, or the same rejection text, on every
// corpus program and on random structured programs; `validate` must give
// the same verdict on certificates mutated from the generated ones.

use verified::certificate::{self, Certificate};

/// `Artifact::lir`: each function's name and slot-IR rows.
type Program = Vec<(String, Vec<Lir>)>;

fn loc_key(l: verified::Loc) -> u64 {
    match l {
        verified::Loc::Slot(i) => u64::from(i),
        verified::Loc::Out(i) => 1 << 32 | u64::from(i),
        verified::Loc::In(i) => 2 << 32 | u64::from(i),
    }
}

fn fact_word(l: verified::Loc, k: verified::Kind) -> u64 {
    loc_key(l) << 3 | k as u64
}

fn put_name(name: &str, words: &mut Vec<u64>) {
    words.push(name.len() as u64);
    for chunk in name.as_bytes().chunks(8) {
        let mut b = [0u8; 8];
        b[..chunk.len()].copy_from_slice(chunk);
        words.push(u64::from_le_bytes(b));
    }
}

fn program_words(program: &[(String, Vec<Lir>)], words: &mut Vec<u64>) {
    words.push(program.len() as u64);
    for (name, rows) in program {
        put_name(name, words);
        words.push(rows.len() as u64);
        for r in rows {
            encode_row(r, words);
        }
    }
}

fn cert_words(cert: &Certificate, words: &mut Vec<u64>) {
    let facts = |a: &[(verified::Loc, verified::Kind)], words: &mut Vec<u64>| {
        words.push(a.len() as u64);
        words.extend(a.iter().map(|(l, k)| fact_word(*l, *k)));
    };
    words.push(cert.functions.len() as u64);
    for f in &cert.functions {
        words.push(f.annotations.len() as u64);
        for a in &f.annotations {
            facts(a, words);
        }
        facts(&f.post, words);
        words.push(f.signed_constants.len() as u64);
        words.extend(f.signed_constants.iter().map(|pc| *pc as u64));
    }
}

fn bytes_of(words: &[u64]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// `put_cert` of the Vesper package.
fn cert_text(cert: &Certificate) -> String {
    use std::fmt::Write as _;
    let facts = |a: &[(verified::Loc, verified::Kind)], out: &mut String| {
        for (l, k) in a {
            let key = loc_key(*l);
            write!(out, " {}.{}.{}", key >> 32, key & 0xffff_ffff, *k as u64).unwrap();
        }
    };
    let mut out = String::from("ok\n");
    for (f, c) in cert.functions.iter().enumerate() {
        writeln!(out, "fn {f}").unwrap();
        for (pc, a) in c.annotations.iter().enumerate() {
            write!(out, "{pc}:").unwrap();
            facts(a, &mut out);
            out.push('\n');
        }
        out.push_str("post:");
        facts(&c.post, &mut out);
        out.push('\n');
        out.push_str("signed:");
        for pc in &c.signed_constants {
            write!(out, " {pc}").unwrap();
        }
        out.push('\n');
    }
    out
}

fn rust_check_text(program: &[(String, Vec<Lir>)]) -> String {
    match certificate::check(program) {
        Ok(cert) => cert_text(&cert),
        Err(r) => format!("{r}\n"),
    }
}

fn check_input(program: &[(String, Vec<Lir>)]) -> Vec<u8> {
    let mut words = vec![];
    program_words(program, &mut words);
    bytes_of(&words)
}

/// Every corpus program the Rust backend lowers (host target), as
/// `Artifact::lir`.
fn corpus_programs() -> Vec<(String, Program)> {
    let Some(target) = verified::Target::host() else {
        return vec![];
    };
    let options = verified::Options::new(target);
    corpus()
        .iter()
        .step_by(stride())
        .filter_map(|case| {
            verified::compile(&case.world, "main", &options)
                .ok()
                .map(|a| (case.name.clone(), a.lir))
        })
        .collect()
}

fn check_differs(check: &Entry, what: &str, program: &[(String, Vec<Lir>)]) -> bool {
    let rust = rust_check_text(program);
    let vesper = check.text(&check_input(program));
    if rust == vesper {
        return false;
    }
    if let Some(d) = first_difference(&rust, &vesper) {
        eprintln!("verified certificate ({what}): first difference {d}");
    }
    true
}

#[test]
fn certificates_match_rust_on_the_corpus() {
    let programs = corpus_programs();
    if verified::Target::host().is_none() {
        eprintln!("verified certificate: no verified target for this host");
        return;
    }
    assert!(
        programs.len() > 20,
        "only {} corpus programs lower",
        programs.len()
    );
    let check = entry("certificate_text");
    let mut bad = 0;
    let mut accepted = 0;
    for (name, program) in &programs {
        if certificate::check(program).is_ok() {
            accepted += 1;
        }
        if check_differs(&check, name, program) {
            bad += 1;
        }
    }
    eprintln!(
        "verified certificate corpus: {bad} of {} programs differ ({accepted} accepted)",
        programs.len()
    );
    println!(
        "flip-gate differential differences={bad} cases={}",
        programs.len()
    );
    assert_eq!(bad, 0);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random program shaped like the lowering's: each function is its entry
/// label, `Enter`, then rows over few slots, `In`/`Out` words and labels
/// (some unknown, some duplicated), and usually a `Return`.
fn random_program(rng: &mut Rng) -> Vec<(String, Vec<Lir>)> {
    use verified::{ArithOp, Cond, FloatOp, Kind, TrapKind, WrapOp};
    let nfun = 1 + rng.below(4) as u32;
    let entry = |f: u32| 4 + f;
    // Each function's own labels; now and then another function's or an
    // unknown one.
    let label = |rng: &mut Rng, f: u32| match rng.below(16) {
        0 => 4 + rng.below(u64::from(nfun) + 40) as u32,
        _ => 16 + 4 * f + rng.below(4) as u32,
    };
    let mut program = vec![];
    for f in 0..nfun {
        let mut rows = vec![Lir::Label(entry(f)), Lir::Enter { words: 4 }];
        // Most bodies start with every slot known, so later rows are
        // usually established and the validator gets past them.
        if rng.below(4) != 0 {
            for dst in 0..6 {
                let word = if rng.below(2) == 0 {
                    rng.below(2)
                } else {
                    rng.below(1000)
                };
                rows.push(Lir::Const { dst, word });
            }
        }
        let n = rng.below(30);
        for _ in 0..n {
            let s = |rng: &mut Rng| rng.below(6) as u32;
            let r = rng.next();
            let kinds = [
                Kind::U8,
                Kind::U32,
                Kind::U64,
                Kind::I64,
                Kind::Bool,
                Kind::F64,
            ];
            let k = kinds[(r >> 8) as usize % if r & 1 == 0 { 4 } else { 6 }];
            let word = match rng.below(6) {
                0 => rng.below(2),
                1 => rng.below(300),
                2 => rng.below(1 << 33),
                3 => u64::MAX - rng.below(5),
                4 => (1 << 63) + rng.below(5),
                _ => rng.next(),
            };
            let row = match rng.below(26) {
                0..=2 => Lir::Const { dst: s(rng), word },
                3 => Lir::Move {
                    dst: s(rng),
                    src: s(rng),
                },
                4 | 5 => Lir::Arith {
                    op: ArithOp::ALL[rng.below(5) as usize],
                    kind: k,
                    dst: s(rng),
                    a: s(rng),
                    b: s(rng),
                },
                6 => Lir::Wrapping {
                    op: WrapOp::ALL[rng.below(3) as usize],
                    kind: k,
                    dst: s(rng),
                    a: s(rng),
                    b: s(rng),
                },
                7 => Lir::Neg {
                    kind: k,
                    dst: s(rng),
                    a: s(rng),
                },
                8 => Lir::Not {
                    dst: s(rng),
                    a: s(rng),
                },
                9 | 10 => Lir::Compare {
                    cond: Cond::ALL[rng.below(6) as usize],
                    kind: k,
                    dst: s(rng),
                    a: s(rng),
                    b: s(rng),
                },
                11 => Lir::FArith {
                    op: FloatOp::ALL[rng.below(4) as usize],
                    dst: s(rng),
                    a: s(rng),
                    b: s(rng),
                },
                12 => Lir::StoreOut {
                    index: rng.below(3) as u32,
                    src: s(rng),
                },
                13 => Lir::LoadOut {
                    dst: s(rng),
                    index: rng.below(3) as u32,
                },
                14 => Lir::LoadIn {
                    dst: s(rng),
                    index: rng.below(3) as u32,
                },
                15 => Lir::StoreIn {
                    index: rng.below(3) as u32,
                    src: s(rng),
                },
                16 => Lir::LoadPtr {
                    dst: s(rng),
                    ptr: s(rng),
                    offset: 8,
                },
                17 => Lir::Call {
                    callee: if rng.below(8) == 0 {
                        label(rng, f)
                    } else {
                        entry(rng.below(u64::from(nfun)) as u32)
                    },
                },
                18 | 19 => {
                    if rng.below(8) == 0 {
                        Lir::Label(label(rng, f))
                    } else {
                        Lir::Move {
                            dst: s(rng),
                            src: s(rng),
                        }
                    }
                }
                20 => Lir::Jump(label(rng, f)),
                21 => Lir::BranchZero {
                    cond: s(rng),
                    target: label(rng, f),
                },
                22 => Lir::Return,
                23 => Lir::Trap(TrapKind::ALL[rng.below(4) as usize]),
                24 => Lir::Scope {
                    ovf: if rng.below(2) == 0 { 0 } else { label(rng, f) },
                    div0: if rng.below(2) == 0 { 1 } else { label(rng, f) },
                },
                _ => Lir::FCompare {
                    cond: Cond::ALL[rng.below(6) as usize],
                    dst: s(rng),
                    a: s(rng),
                    b: s(rng),
                },
            };
            rows.push(row);
        }
        // Each of the function's own labels, once, somewhere in the body.
        for l in 0..4 {
            let at = 2 + rng.below(rows.len() as u64 - 1) as usize;
            rows.insert(at, Lir::Label(16 + 4 * f + l));
        }
        if rng.below(6) != 0 {
            rows.push(Lir::Return);
        }
        // Now and then a malformed shape.
        match rng.below(40) {
            0 => {
                rows.remove(0);
            }
            1 => rows.swap(0, 1),
            2 => rows.push(Lir::Enter { words: 1 }),
            _ => {}
        }
        program.push((format!("f{f}"), rows));
    }
    program
}

#[test]
fn certificates_match_rust_on_random_programs() {
    let check = entry("certificate_text");
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut bad = 0;
    let mut accepted = 0;
    let mut reasons = std::collections::BTreeMap::new();
    let n = 4000;
    for i in 0..n {
        let program = random_program(&mut rng);
        match certificate::check(&program) {
            Ok(_) => accepted += 1,
            Err(r) => {
                let why: String = r.reason.split(['{', '(', '\'']).next().unwrap_or("").into();
                *reasons.entry(why).or_insert(0usize) += 1;
            }
        }
        if check_differs(&check, &format!("random {i}"), &program) {
            bad += 1;
        }
    }
    eprintln!("verified certificate random: {bad} of {n} programs differ ({accepted} accepted)");
    eprintln!("verified certificate random rejections: {reasons:?}");
    // Random rows rarely establish their operands' kinds, so most random
    // programs exercise a rejection; the corpus covers accepted ones.
    assert!(
        accepted > n / 40,
        "only {accepted} random programs accepted"
    );
    assert_eq!(bad, 0);
}

/// One mutation of a generated certificate (or of its program).
fn mutate(rng: &mut Rng, program: &mut [(String, Vec<Lir>)], cert: &mut Certificate) {
    use verified::{Kind, Loc};
    let kinds = [
        Kind::U8,
        Kind::U32,
        Kind::U64,
        Kind::I64,
        Kind::Bool,
        Kind::F64,
    ];
    let f = rng.below(cert.functions.len() as u64) as usize;
    let c = &mut cert.functions[f];
    let rows = c.annotations.len() as u64;
    if rows == 0 {
        return;
    }
    match rng.below(9) {
        0 => {
            // Drop a fact.
            let pc = rng.below(rows) as usize;
            if !c.annotations[pc].is_empty() {
                let i = rng.below(c.annotations[pc].len() as u64) as usize;
                c.annotations[pc].remove(i);
            }
        }
        1 | 2 => {
            // Change a fact's kind, or add a fact (maybe a duplicate).
            let pc = rng.below(rows) as usize;
            let k = kinds[rng.below(6) as usize];
            let a = &mut c.annotations[pc];
            if !a.is_empty() && rng.below(2) == 0 {
                let i = rng.below(a.len() as u64) as usize;
                a[i].1 = k;
            } else {
                let l = match rng.below(3) {
                    0 => Loc::Slot(rng.below(6) as u32),
                    1 => Loc::Out(rng.below(3) as u32),
                    _ => Loc::In(rng.below(3) as u32),
                };
                let at = rng.below(a.len() as u64 + 1) as usize;
                a.insert(at, (l, k));
            }
        }
        3 => {
            // A signed constant on any row.
            c.signed_constants.insert(rng.below(rows + 1) as usize);
        }
        4 => {
            // A post fact.
            let l = if rng.below(4) == 0 {
                Loc::Slot(rng.below(6) as u32)
            } else {
                Loc::In(rng.below(3) as u32)
            };
            c.post.push((l, kinds[rng.below(6) as usize]));
        }
        5 => {
            // An annotation too few or too many.
            if rng.below(2) == 0 {
                c.annotations.pop();
            } else {
                c.annotations.push(vec![]);
            }
        }
        6 => {
            // A function certificate too few.
            cert.functions.pop();
        }
        7 => {
            // An entry precondition.
            c.annotations[0].push((Loc::In(rng.below(3) as u32), kinds[rng.below(5) as usize]));
        }
        _ => {
            // Retarget a jump or call in the program.
            let rows = &mut program[f].1;
            let pc = rng.below(rows.len() as u64) as usize;
            let l = rng.below(12) as u32;
            match &mut rows[pc] {
                Lir::Jump(t) | Lir::BranchZero { target: t, .. } | Lir::Call { callee: t } => {
                    *t = l
                }
                Lir::Scope { ovf, .. } => *ovf = l,
                _ => {}
            }
        }
    }
}

#[test]
fn validate_matches_rust_on_mutated_certificates() {
    let validate = entry("validate_text");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut cases: Vec<Vec<(String, Vec<Lir>)>> =
        corpus_programs().into_iter().map(|(_, p)| p).collect();
    for _ in 0..1500 {
        cases.push(random_program(&mut rng));
    }
    let mut bad = 0;
    let mut total = 0;
    let mut rejected = 0;
    for program in &cases {
        let widened: Vec<(String, Vec<Lir>)> = program
            .iter()
            .map(|(n, rows)| (n.clone(), rows.iter().map(certificate::widen).collect()))
            .collect();
        let generated = certificate::generate(&widened);
        for round in 0..6 {
            let mut p = widened.clone();
            let mut cert = generated.clone();
            if !cert.functions.is_empty() {
                for _ in 0..round.min(2) + usize::from(round > 0) {
                    if cert.functions.is_empty() {
                        break;
                    }
                    mutate(&mut rng, &mut p, &mut cert);
                }
            }
            let rust = match certificate::validate(&p, &cert) {
                Ok(()) => "ok\n".to_string(),
                Err(r) => {
                    rejected += 1;
                    format!("{r}\n")
                }
            };
            let mut words = vec![];
            program_words(&p, &mut words);
            cert_words(&cert, &mut words);
            let vesper = validate.text(&bytes_of(&words));
            total += 1;
            if rust != vesper {
                bad += 1;
                if bad <= 6 {
                    eprintln!("verified validate: rust {rust:?} vesper {vesper:?}");
                }
            }
        }
    }
    eprintln!("verified validate: {bad} of {total} certificates differ ({rejected} rejected)");
    println!("flip-gate differential differences={bad} cases={total}");
    assert!(rejected > total / 10, "only {rejected} rejections");
    assert_eq!(bad, 0);
}

/// ABBA: Rust's `certificate::check` against the Vesper entry with its
/// input encoding and text unpacking, over the corpus programs.
#[test]
fn certificate_flip_gate() {
    if !speed() {
        return;
    }
    let programs: Vec<_> = corpus_programs().into_iter().map(|(_, p)| p).collect();
    let inputs: Vec<Vec<u8>> = programs.iter().map(|p| check_input(p)).collect();
    let check = entry("certificate_text");
    let (r, v) = abba(
        "verified certificate check",
        &mut || {
            time(|| {
                for _ in 0..5 {
                    for p in &programs {
                        std::hint::black_box(rust_check_text(p));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..5 {
                    for i in &inputs {
                        std::hint::black_box(check.text(i));
                    }
                }
            })
        },
    );
    println!(
        "flip-gate pairs rust={} vesper={}",
        r.as_nanos(),
        v.as_nanos()
    );
}

// ----- part 4a: the lowering ---------------------------------------------------
//
// `lower.ves` against `lower::plan` and `lower::function`, through
// `verified_codegen::lowering_for_test`: the program `compile` lowers (after
// ir_opt, monomorphization and evidence passing), as ir_words encodes it,
// then the entry, the limits, whether the region prelude is active and the
// names of the world's functions. The package prints every planned
// function's rows in the Rust `Debug` form, or the error's text. Programs
// with `handle` expressions or region operations are part 4c's: the
// package answers `not ported` for them.

fn put_string(s: &str, words: &mut Vec<u64>) {
    words.push(s.len() as u64);
    for chunk in s.as_bytes().chunks(8) {
        let mut b = [0u8; 8];
        b[..chunk.len()].copy_from_slice(chunk);
        words.push(u64::from_le_bytes(b));
    }
}

fn lowering_input(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    entry: &str,
    options: &verified::Options,
) -> Option<Vec<u8>> {
    if !ir_words::encodable(world, program) {
        return None;
    }
    let mut t = ir_words::Tables::of(program);
    t.label_names();
    let section = t.section();
    let encoded = t.enc.world(world);
    let mut words = expr_rows(&mut t)?;
    lowering_extras(world, entry, options, &mut words);
    let mut bytes = t.enc.finish(&encoded, &section);
    bytes.extend(bytes_of(&words));
    Some(bytes)
}

/// The lowering's extra section: the entry, the limits, whether the region
/// prelude is active and the names of the world's functions.
fn lowering_extras(
    world: &vesper::check::World,
    entry: &str,
    options: &verified::Options,
    words: &mut Vec<u64>,
) {
    put_string(entry, words);
    words.push(options.max_functions as u64);
    words.push(options.max_rows as u64);
    words.push(u64::from(vesper::regions::active(world)));
    words.push(world.functions.len() as u64);
    for name in world.functions.keys() {
        put_string(name, words);
    }
}

/// One clause of a `handle` expression: its operation name, patterns
/// (0 `_`, 1 a name and the name, 2 any other) and body span.
fn clause_row(t: &mut ir_words::Tables, c: &vesper::ast::Clause, out: &mut Vec<u64>) {
    use vesper::ast::PatternKind;
    let n = t.enc.name(&c.name);
    out.push(n);
    out.push(c.pats.len() as u64);
    for p in &c.pats {
        match &p.kind {
            PatternKind::Wild => out.push(0),
            PatternKind::Bind(b) => {
                out.push(1);
                let n = t.enc.name(b);
                out.push(n);
            }
            _ => out.push(2),
        }
    }
    out.extend([c.body.span.start as u64, c.body.span.end as u64]);
}

/// The expressions of the payload pool, by pool index: 0 for an operation;
/// else 1 (a closure) or 2 (a `handle`), the span and the free names
/// (`check::free_names`). A `handle` adds its effect (name, label + 1 or
/// 0, argument types), whether it is stateful, the body span, the `return`
/// clause and the operation clauses. `None` when a type cannot be encoded.
fn expr_rows(t: &mut ir_words::Tables) -> Option<Vec<u64>> {
    use vesper::ast::ExprKind;
    let items = t.pool.items.clone();
    let mut out = vec![items.len() as u64];
    for item in &items {
        let ir_words::Payload::Expr(e) = item else {
            out.push(0);
            continue;
        };
        let handle = matches!(e.kind, ExprKind::Handle { .. });
        out.push(if handle { 2 } else { 1 });
        out.extend([e.span.start as u64, e.span.end as u64]);
        let free = vesper::check::free_names(e);
        out.push(free.len() as u64);
        for n in &free {
            let i = t.enc.name(n);
            out.push(i);
        }
        let ExprKind::Handle {
            effect,
            state,
            body,
            ret,
            ops,
        } = &e.kind
        else {
            continue;
        };
        if !effect.args.iter().all(ir_words::literal_fits) {
            return None;
        }
        let n = t.enc.name(&effect.name);
        out.push(n);
        let label = effect.label.as_ref().map_or(0, |l| t.enc.name(l) + 1);
        out.push(label);
        out.push(effect.args.len() as u64);
        for a in &effect.args {
            let i = t.enc.ty(a);
            out.push(i);
        }
        out.push(u64::from(state.is_some()));
        out.extend([body.span.start as u64, body.span.end as u64]);
        clause_row(t, ret, &mut out);
        out.push(ops.len() as u64);
        for c in ops {
            clause_row(t, c, &mut out);
        }
    }
    Some(out)
}

/// `lower_input`'s text for the Rust lowering's result.
fn rust_lowering_text(lowered: &Result<Program, verified::VerifiedError>) -> String {
    use std::fmt::Write as _;
    match lowered {
        Ok(functions) => {
            let mut out = String::new();
            for (name, rows) in functions {
                writeln!(out, "fn {name}").unwrap();
                for row in rows {
                    writeln!(out, "{row:?}").unwrap();
                }
            }
            out
        }
        Err(e) => format!("error: {e}\n"),
    }
}

/// Every corpus world's lowering input and the Rust result: (name, input,
/// Rust text), for the programs without handler sites.
fn lowering_cases(options: &verified::Options) -> Vec<(String, Vec<u8>, String)> {
    lowering_programs(options)
        .into_iter()
        .map(|(name, _, input, rust)| (name, input, rust))
        .collect()
}

/// [`lowering_cases`] with each case's world index and program.
#[allow(clippy::type_complexity)]
fn lowering_programs(
    options: &verified::Options,
) -> Vec<(String, (usize, vesper::ir::Program), Vec<u8>, String)> {
    let mut out = vec![];
    for (k, case) in corpus().iter().enumerate().step_by(stride()) {
        let Ok((program, sites, lowered)) =
            verified::lowering_for_test(&case.world, "main", options)
        else {
            continue;
        };
        if sites {
            continue;
        }
        let Some(input) = lowering_input(&case.world, &program, "main", options) else {
            continue;
        };
        let rust = rust_lowering_text(&lowered);
        out.push((case.name.clone(), (k, program), input, rust));
    }
    out
}

#[test]
fn lowering_matches_rust_on_the_corpus() {
    let Some(target) = verified::Target::host() else {
        eprintln!("verified lowering: no verified target for this host");
        return;
    };
    let lower = entry("lowering_text");
    let mut bad = 0;
    let mut compared = 0;
    let mut lowered = 0;
    let mut not_ported = 0;
    for optimize in [true, false] {
        let mut options = verified::Options::new(target);
        options.optimize = optimize;
        for (name, input, rust) in lowering_cases(&options) {
            let vesper = lower.text(&input);
            if vesper.starts_with("error: not ported") {
                not_ported += 1;
                continue;
            }
            compared += 1;
            if !rust.starts_with("error: ") {
                lowered += 1;
            }
            if vesper != rust {
                bad += 1;
                if bad <= 5 {
                    let d = first_difference(&rust, &vesper);
                    eprintln!(
                        "verified lowering ({name}, optimize {optimize}): first difference {d:?}"
                    );
                }
            }
        }
    }
    eprintln!(
        "verified lowering corpus: {bad} of {compared} programs differ ({lowered} lowered, {not_ported} not ported)"
    );
    println!("flip-gate differential differences={bad} cases={compared}");
    assert!(lowered > 20, "only {lowered} corpus programs lower");
    assert_eq!(bad, 0);
}

/// ABBA timing of the lowering (`CHECK_HARNESS_SPEED=1`): the Rust plan,
/// lowering and text against the package's decode, plan, lowering and
/// text, over the corpus programs the package lowers.
#[test]
fn lowering_flip_gate() {
    if !speed() {
        return;
    }
    let Some(target) = verified::Target::host() else {
        return;
    };
    let options = verified::Options::new(target);
    let lower = entry("lowering_text");
    let cases: Vec<_> = lowering_programs(&options)
        .into_iter()
        .filter(|(_, _, input, _)| !lower.text(input).starts_with("error: not ported"))
        .collect();
    let (r, v) = abba(
        "verified lowering",
        &mut || {
            time(|| {
                for _ in 0..5 {
                    for (_, (k, program), _, _) in &cases {
                        let world = &corpus()[*k].world;
                        let lowered = verified::lower_for_test(world, program, "main", &options);
                        std::hint::black_box(rust_lowering_text(&lowered));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..5 {
                    for (_, _, input, _) in &cases {
                        std::hint::black_box(lower.text(input));
                    }
                }
            })
        },
    );
    println!(
        "flip-gate pairs rust={} vesper={}",
        r.as_nanos(),
        v.as_nanos()
    );
}

// ----- part 4b: monomorphization (mono.ves) ---------------------------------
//
// `mono_words` reads the world and the Place IR after ir_opt, then an extra
// section: the entry, the instance limit, every world function's generics
// (name, then (name, row) per generic) and `World::calls` ((start, end), then
// (name, type) per argument). Names and types are indices into the input's
// tables. It answers 0 and the monomorphized program's
// words, or 1 and the error's packed text. Programs with `handle`
// expressions are part 4c's (`not ported`).

/// The input of `mono_words`, and the payload pool its program refers to.
fn mono_input(
    world: &vesper::check::World,
    program: &vesper::ir::Program,
    entry: &str,
    max_instances: usize,
    lowering: Option<&verified::Options>,
) -> Option<(Vec<u8>, ir_words::Pool)> {
    if !ir_words::encodable(world, program)
        || !world
            .calls
            .values()
            .all(|m| m.values().all(ir_words::literal_fits))
    {
        return None;
    }
    let mut t = ir_words::Tables::of(program);
    t.label_names();
    let section = t.section();
    // Monomorphization reads only the ADTs of the world section (which
    // names are types), so without the lowering the expression types and
    // spans stay out.
    let encoded = if lowering.is_some() {
        t.enc.world(world)
    } else {
        t.enc.world(&vesper::check::World {
            adts: world.adts.clone(),
            effects: world.effects.clone(),
            ..Default::default()
        })
    };
    let mut words = expr_rows(&mut t)?;
    let e = t.enc.name(entry);
    words.push(e);
    words.push(max_instances as u64);
    // Only functions with a body are instantiated or called by instance,
    // and only the type arguments of the program's calls are looked up.
    let functions: Vec<_> = world
        .functions
        .iter()
        .filter(|(name, _)| program.functions.contains_key(*name))
        .collect();
    let spans: std::collections::BTreeSet<(usize, usize)> = program
        .functions
        .values()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(i.op, vesper::ir::Op::Call { .. }))
        .map(|i| (i.span.start, i.span.end))
        .collect();
    let calls: Vec<_> = world
        .calls
        .iter()
        .filter(|(k, _)| spans.contains(k))
        .collect();
    words.push(functions.len() as u64);
    for (name, f) in functions {
        let n = t.enc.name(name);
        words.push(n);
        words.push(f.generics.len() as u64);
        for g in &f.generics {
            let n = t.enc.name(&g.name);
            words.push(n);
            words.push(u64::from(g.row));
        }
    }
    words.push(calls.len() as u64);
    for ((start, end), args) in calls {
        words.push(*start as u64);
        words.push(*end as u64);
        words.push(args.len() as u64);
        for (k, ty) in args {
            let n = t.enc.name(k);
            words.push(n);
            let i = t.enc.ty(ty);
            words.push(i);
        }
    }
    if let Some(options) = lowering {
        lowering_extras(world, entry, options, &mut words);
    }
    let mut bytes = t.enc.finish(&encoded, &section);
    bytes.extend(bytes_of(&words));
    Some((bytes, t.pool))
}

/// The text both sides compare: the program's canonical dump, or the
/// error's text.
fn rust_mono_text(mono: &Result<vesper::ir::Program, verified::VerifiedError>) -> String {
    match mono {
        Ok(p) => ir_words::dump(p),
        Err(e) => format!("error: {e}\n"),
    }
}

fn vesper_mono_text(words: &[u64], pool: &ir_words::Pool) -> String {
    match words.first() {
        Some(0) => ir_words::dump(&ir_words::decode_program(&words[1..], pool)),
        Some(_) => unpack_text(&words[1..]),
        None => "error: empty output\n".to_owned(),
    }
}

/// One corpus program's monomorphization case.
struct MonoCase {
    name: String,
    input: Vec<u8>,
    pool: ir_words::Pool,
    rust: String,
}

fn mono_cases(options: &verified::Options) -> Vec<MonoCase> {
    let mut out = vec![];
    for case in corpus().iter().step_by(stride()) {
        let Ok((program, max, mono)) = verified::mono_for_test(&case.world, "main", options) else {
            continue;
        };
        let Some((input, pool)) = mono_input(&case.world, &program, "main", max, None) else {
            continue;
        };
        out.push(MonoCase {
            name: case.name.clone(),
            input,
            pool,
            rust: rust_mono_text(&mono),
        });
    }
    out
}

#[test]
fn mono_matches_rust_on_the_corpus() {
    let Some(target) = verified::Target::host() else {
        eprintln!("verified mono: no verified target for this host");
        return;
    };
    let mono = entry("mono_words");
    let mut bad = 0;
    let mut compared = 0;
    let mut generic = 0;
    let mut not_ported = 0;
    for optimize in [true, false] {
        let mut options = verified::Options::new(target);
        options.optimize = optimize;
        for case in mono_cases(&options) {
            let vesper = vesper_mono_text(&mono.words(&case.input), &case.pool);
            if vesper.starts_with("error: not ported") {
                not_ported += 1;
                continue;
            }
            compared += 1;
            if case.rust.contains('<') || case.rust.contains("unsupported Generic") {
                generic += 1;
            }
            if vesper != case.rust {
                bad += 1;
                if bad <= 5 {
                    let d = first_difference(&case.rust, &vesper);
                    eprintln!(
                        "verified mono ({}, optimize {optimize}): first difference {d:?}",
                        case.name
                    );
                }
            }
        }
    }
    eprintln!(
        "verified mono corpus: {bad} of {compared} programs differ ({generic} with instances or generic errors, {not_ported} not ported)"
    );
    println!("flip-gate differential differences={bad} cases={compared}");
    assert!(compared > 100, "only {compared} corpus programs compared");
    assert_eq!(bad, 0);
}

// The front half: monomorphization and the lowering in one Vesper pass
// (`front_text`), against Rust's monomorphization, evidence passing and
// lowering, on the programs evidence passing leaves unchanged.

/// One corpus program's front-half case.
struct FrontCase {
    name: String,
    world: usize,
    program: vesper::ir::Program,
    input: Vec<u8>,
    rust: String,
}

fn front_cases(options: &verified::Options) -> Vec<FrontCase> {
    let mut out = vec![];
    for (k, case) in corpus().iter().enumerate().step_by(stride()) {
        let Ok((program, max, outcome)) = verified::front_for_test(&case.world, "main", options)
        else {
            continue;
        };
        let rust = match outcome {
            Ok(o) if o.evidence_unchanged && !o.has_sites => rust_lowering_text(&o.lowered),
            Ok(_) => continue,
            Err(e) => format!("error: {e}\n"),
        };
        let Some((input, _)) = mono_input(&case.world, &program, "main", max, Some(options)) else {
            continue;
        };
        out.push(FrontCase {
            name: case.name.clone(),
            world: k,
            program,
            input,
            rust,
        });
    }
    out
}

#[test]
fn front_matches_rust_on_the_corpus() {
    let Some(target) = verified::Target::host() else {
        eprintln!("verified front: no verified target for this host");
        return;
    };
    let front = entry("front_text");
    let mut bad = 0;
    let mut compared = 0;
    let mut lowered = 0;
    let mut not_ported = 0;
    for optimize in [true, false] {
        let mut options = verified::Options::new(target);
        options.optimize = optimize;
        for case in front_cases(&options) {
            let vesper = front.text(&case.input);
            if vesper.starts_with("error: not ported") {
                not_ported += 1;
                continue;
            }
            compared += 1;
            if !case.rust.starts_with("error: ") {
                lowered += 1;
            }
            if vesper != case.rust {
                bad += 1;
                if bad <= 5 {
                    let d = first_difference(&case.rust, &vesper);
                    eprintln!(
                        "verified front ({}, optimize {optimize}): first difference {d:?}",
                        case.name
                    );
                }
            }
        }
    }
    eprintln!(
        "verified front corpus: {bad} of {compared} programs differ ({lowered} lowered, {not_ported} not ported)"
    );
    println!("flip-gate differential differences={bad} cases={compared}");
    assert!(lowered > 20, "only {lowered} corpus programs lower");
    assert_eq!(bad, 0);
}

/// ABBA timing of the front half (`CHECK_HARNESS_SPEED=1`): Rust's
/// monomorphization, evidence passing, plan, lowering and text against the
/// package's decode, monomorphization, plan, lowering and text, over the
/// corpus programs the package lowers.
#[test]
fn front_flip_gate() {
    if !speed() {
        return;
    }
    let Some(target) = verified::Target::host() else {
        return;
    };
    let options = verified::Options::new(target);
    let front = entry("front_text");
    let cases: Vec<_> = front_cases(&options)
        .into_iter()
        .filter(|c| !front.text(&c.input).starts_with("error: not ported"))
        .collect();
    let (r, v) = abba(
        "verified front",
        &mut || {
            time(|| {
                for _ in 0..5 {
                    for c in &cases {
                        let world = &corpus()[c.world].world;
                        let lowered =
                            verified::front_timing_for_test(world, &c.program, "main", &options);
                        std::hint::black_box(rust_lowering_text(&lowered));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for _ in 0..5 {
                    for c in &cases {
                        std::hint::black_box(front.text(&c.input));
                    }
                }
            })
        },
    );
    println!(
        "flip-gate pairs rust={} vesper={}",
        r.as_nanos(),
        v.as_nanos()
    );
}
