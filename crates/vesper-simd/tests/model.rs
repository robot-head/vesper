//! Differential tests: the Lean algorithm model, its Rust transliteration and the
//! scalar reference against the REAL Baseline128 `JitScanner`.
//!
//! These compare outputs on concrete inputs. They are not a machine-code
//! certificate; the general claim is the Lean theorem `scan_correct`, which
//! covers the modeled algorithm, not Cranelift/ISA lowering.

#[path = "../model/mod.rs"]
mod model;

use model::{LeanCase, Read, Rng};
use std::path::PathBuf;
use vesper_simd::{ElfTarget, Error, IsaProfileKind, JitScanner, ObjectArtifact, ScanResult};

fn scanner() -> Option<JitScanner> {
    match JitScanner::compile_profile(IsaProfileKind::Baseline128) {
        Ok(scanner) => Some(scanner),
        Err(Error::MissingBaselineFeatures | Error::UnsupportedHost) => {
            eprintln!("Baseline128 unavailable on this host; JIT differential skipped");
            None
        }
        Err(e) => panic!("JIT code generation: {e}"),
    }
}

fn lean_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean/SimdScanner.lean")
}

fn fixture_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean/SimdScanner/Fixture.lean")
}

fn check(jit: Option<&JitScanner>, data: &[u8], delimiter: u8, what: &str) {
    let (count, xor) = model::reference(data, delimiter);
    let run = model::scan(data, delimiter);
    assert_eq!(
        (run.count, run.xor),
        (count, xor),
        "model vs reference: {what}"
    );
    if let Some(jit) = jit {
        assert_eq!(
            jit.scan(data, delimiter),
            ScanResult {
                matches: count,
                xor
            },
            "JIT vs model: {what}"
        );
    }
}

/// Every delimiter, every tail size of the first 80 lengths, every start
/// residue modulo 16 (unaligned slices of one allocation).
#[test]
fn model_reference_and_jit_agree_on_all_tails_offsets_and_delimiters() {
    let jit = scanner();
    let mut data: Vec<u8> = (0..=255u8).collect();
    let mut rng = Rng(0x5eed_0001);
    for i in (1..data.len()).rev() {
        data.swap(i, (rng.next() % (i as u64 + 1)) as usize);
    }
    data.extend(rng.bytes(128));
    let mut residues = [false; 16];
    for offset in 0..16 {
        for len in 0..=80 {
            let slice = &data[offset..offset + len];
            residues[slice.as_ptr() as usize % 16] = true;
            for delimiter in 0..=255u8 {
                check(
                    jit.as_ref(),
                    slice,
                    delimiter,
                    &format!("offset={offset} len={len} delim={delimiter}"),
                );
            }
        }
    }
    assert!(residues.iter().all(|seen| *seen), "all 16 address residues");
}

#[test]
fn model_reference_and_jit_agree_on_seeded_large_inputs() {
    let jit = scanner();
    let mut rng = Rng(0x5eed_0002);
    let data = rng.bytes(300_000);
    let mut lengths = vec![65_535, 65_536, 65_537, 131_089, 262_143];
    lengths.extend((0..24).map(|_| (rng.next() % 290_000) as usize));
    for len in lengths {
        let offset = (rng.next() % 32) as usize;
        // Bias toward delimiters that occur (every byte value occurs here) and
        // include the extremes.
        for delimiter in [0, 255, rng.next() as u8, data[offset]] {
            check(
                jit.as_ref(),
                &data[offset..offset + len],
                delimiter,
                &format!("seeded offset={offset} len={len} delim={delimiter}"),
            );
        }
    }
    // A slice where every byte matches makes the count equal the length.
    let ones = vec![b','; 70_001];
    check(jit.as_ref(), &ones, b',', "all-match");
    assert_eq!(model::scan(&ones, b',').count, 70_001);
}

/// Rust mirror of the Lean read theorems: exactly `len / 16` full-width reads
/// at consecutive block offsets and `len % 16` scalar reads covering the tail,
/// all legal, none past the slice.
#[test]
fn model_read_log_matches_block_tail_decomposition() {
    for len in 0..=200u64 {
        let data = vec![0u8; len as usize];
        let run = model::scan(&data, 0);
        let expected: Vec<Read> = (0..len / 16)
            .map(|block| Read::Vector { offset: block * 16 })
            .chain((len / 16 * 16..len).map(|offset| Read::Scalar { offset }))
            .collect();
        assert_eq!(run.reads, expected, "len={len}");
        assert!(run.reads.iter().all(|r| model::read_is_legal(*r, len)));
    }
    assert!(model::read_is_legal(Read::Vector { offset: 4 }, 20));
    assert!(!model::read_is_legal(Read::Vector { offset: 5 }, 20));
    assert!(!model::read_is_legal(
        Read::Vector {
            offset: u64::MAX - 3
        },
        u64::MAX
    ));
    assert!(!model::read_is_legal(Read::Scalar { offset: 20 }, 20));
    assert!(model::scan(&[], 7).reads.is_empty());
}

/// The model transcribes this exact IR; any codegen change must revisit the
/// Lean model. Value numbers and comments are normalized away.
#[test]
fn jit_clif_matches_modeled_program_and_metadata() {
    let Some(jit) = scanner() else { return };
    let metadata = jit.metadata();
    assert_eq!(metadata.profile, IsaProfileKind::Baseline128);
    assert_eq!(metadata.vector_bits, model::MODELED_VECTOR_BITS);
    assert_eq!(metadata.tail_strategy, model::MODELED_TAIL_STRATEGY);
    assert_eq!(metadata.abi, model::MODELED_ABI);

    let mut expected = vec![
        "iconst.i64 0",
        "iconst.i8 0",
        "splat.i8x16 v",
        "splat.i8x16 v",
        "band_imm v, -16",
        "jump block1(v, v, v)",
        "icmp ult v, v",
        "brif v, block2(v, v, v), block3(v, v, v)",
        "iadd.i64 v, v",
        "load.i8x16 v",
        "icmp eq v, v",
    ];
    let lanes: Vec<String> = (0..16)
        .map(|lane| format!("extractlane v, {lane}"))
        .collect();
    for lane in &lanes {
        expected.extend([lane.as_str(), "band_imm v, 1", "uextend.i64 v", "iadd v, v"]);
    }
    expected.extend(["bxor v, v", "iadd_imm v, 16", "jump block1(v, v, v)"]);
    for (index, lane) in lanes.iter().enumerate() {
        expected.extend([
            lane.as_str(),
            if index == 0 {
                "bxor.i8 v, v"
            } else {
                "bxor v, v"
            },
        ]);
    }
    expected.extend([
        "jump block4(v, v, v)",
        "icmp ult v, v",
        "brif v, block5(v, v, v), block6(v, v, v)",
        "iadd.i64 v, v",
        "load.i8 v",
        "icmp eq v, v",
        "uextend.i64 v",
        "iadd v, v",
        "bxor v, v",
        "iadd_imm v, 1",
        "jump block4(v, v, v)",
        "store v, v",
        "return v",
    ]);
    let normalized: Vec<String> = jit
        .clif()
        .lines()
        .map(|line| line.split(';').next().unwrap().trim())
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("block")
                && !line.starts_with("function")
                && *line != "}"
        })
        .map(|line| {
            let line = line.split_once(" = ").map_or(line, |(_, rhs)| rhs);
            normalize_values(line)
        })
        .collect();
    assert_eq!(normalized, expected, "CLIF:\n{}", jit.clif());
    for target in [ElfTarget::X86_64, ElfTarget::Aarch64] {
        let object = ObjectArtifact::emit(target, IsaProfileKind::Baseline128).unwrap();
        let symbol = model::MODELED_SYMBOL.as_bytes();
        assert!(object.bytes().windows(symbol.len()).any(|w| w == symbol));
        assert_eq!(
            object.metadata().tail_strategy,
            model::MODELED_TAIL_STRATEGY
        );
        assert_eq!(object.metadata().abi, model::MODELED_ABI);
    }
}

/// Removes `/- ... -/` (possibly nested) and `--` comments; string literals in
/// the model contain neither delimiter.
fn strip_lean_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let (mut out, mut depth, mut i) = (Vec::new(), 0usize, 0);
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/-") {
            depth += 1;
            i += 2;
        } else if depth > 0 && bytes[i..].starts_with(b"-/") {
            depth -= 1;
            i += 2;
        } else if depth == 0 && bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            if depth == 0 {
                out.push(bytes[i]);
            }
            i += 1;
        }
    }
    String::from_utf8(out).expect("ASCII-delimited comments")
}

fn normalize_values(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        let starts_value = c == 'v'
            && chars.peek().is_some_and(char::is_ascii_digit)
            && !out[..out.len() - 1]
                .chars()
                .last()
                .is_some_and(|p| p.is_ascii_alphanumeric() || p == '_' || p == '.');
        if starts_value {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        }
    }
    out
}

#[test]
fn lean_and_rust_model_identifiers_agree() {
    let source = std::fs::read_to_string(lean_file()).expect("read Lean model");
    assert!(source.contains(&format!("def modelId : String := \"{}\"", model::MODEL_ID)));
    assert!(source.contains(&format!("\"{}\"", model::MODEL_CLAIM)));
    assert_eq!(
        source.matches("\n#print axioms ").count(),
        model::AUDITED_THEOREMS
    );
    let code = strip_lean_comments(&source);
    assert!(!code.contains("partial ") && !code.contains("unsafe "));
    for forbidden in [
        "sorry",
        "native_decide",
        "bv_decide",
        "admit",
        "ofReduceBool",
    ] {
        assert!(!code.contains(forbidden), "Lean model uses {forbidden}");
    }
    assert!(
        !code
            .lines()
            .any(|line| line.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
}

/// Runs the executable Lean model (which re-elaborates and kernel-checks every
/// proof first) and compares its machine-program outputs with the JIT and the
/// Rust model. Requires `VESPER_LEAN` (as in CI); otherwise reports a skip.
#[test]
fn lean_model_matches_real_jit() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean executable-model differential skipped");
        return;
    };
    let jit = scanner();
    let mut rng = Rng(0x5eed_0003);
    let pool = rng.bytes(4096);
    let permutation: Vec<u8> = (0..=255u8)
        .map(|b| b.wrapping_mul(167).wrapping_add(3))
        .collect();
    let mut owned: Vec<(u64, u8, Vec<u8>)> = Vec::new();
    // Every remainder size and three full-block counts, several delimiters,
    // unaligned base addresses including the top of the address space.
    for len in 0..=48usize {
        let start = (len * 7) % 64;
        let slice = pool[start..start + len].to_vec();
        let top = (u64::MAX - len as u64).wrapping_add(1);
        for (base, delimiter) in [
            (0x1000 + start as u64, 0u8),
            (0x2003, 255),
            (top, pool[start]),
            (0x7fff_ffff_fff1, b','),
        ] {
            owned.push((base, delimiter, slice.clone()));
        }
    }
    // Every delimiter against a slice that contains every byte value.
    let mut all = permutation.clone();
    all.extend_from_slice(&pool[..9]);
    for delimiter in 0..=255u8 {
        owned.push((0x10_0007, delimiter, all.clone()));
    }
    // Seeded larger slices.
    for _ in 0..6 {
        let len = (rng.next() % 2000) as usize + 100;
        let start = (rng.next() % 1000) as usize;
        owned.push((
            0x4000_0000 + start as u64,
            pool[start + 3],
            pool[start..start + len].to_vec(),
        ));
    }
    let cases: Vec<LeanCase<'_>> = owned
        .iter()
        .map(|(base, delimiter, data)| LeanCase {
            base: *base,
            delimiter: *delimiter,
            data,
        })
        .collect();
    let (header, results) = model::run_lean(&lean, &lean_file(), &fixture_file(), &cases);
    assert_eq!(
        header,
        format!("{} | {}", model::MODEL_ID, model::MODEL_CLAIM)
    );
    for (case, result) in cases.iter().zip(&results) {
        let what = format!(
            "Lean base={:#x} len={} delim={}",
            case.base,
            case.data.len(),
            case.delimiter
        );
        let run = model::scan(case.data, case.delimiter);
        assert_eq!((result.count, result.xor), (run.count, run.xor), "{what}");
        assert_eq!(result.reads, run.reads.len(), "{what}");
        assert!(result.in_bounds, "{what}");
        check(jit.as_ref(), case.data, case.delimiter, &what);
    }
}
