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
//! - `VERIFIED_CODEGEN_STRIDE=n` keeps every n-th corpus world.
//! - `CHECK_HARNESS_SPEED=1` runs the ABBA timing (release build).
#[path = "common/check_harness.rs"]
mod check_harness;

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
