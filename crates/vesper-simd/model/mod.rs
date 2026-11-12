//! Std-only Rust transliteration of the Lean model in `lean/SimdScanner.lean`.
//!
//! This is a *model fixture*, not a machine-code certificate. It mirrors the
//! modeled Cranelift program step by step (block/mask-count/XOR reduction plus
//! checked scalar remainder) and the scalar reference, so differential tests can
//! compare both, and the Lean executable model, with the real `JitScanner`.
//! Agreement on tested inputs is evidence about those inputs only; the general
//! algorithm/bounds claim is the Lean theorem `scan_correct`, and neither covers
//! Cranelift/ISA lowering, object linking or CPU discovery.

#![allow(dead_code, reason = "shared by several differential tests")]

/// Must equal `Vesper.SimdScanner.modelId` in the Lean file.
pub const MODEL_ID: &str = "vesper.simd.baseline128.algorithm-model/1";
/// Must equal `Vesper.SimdScanner.modelClaim` in the Lean file.
pub const MODEL_CLAIM: &str =
    "algorithm/bounds model only; not a Cranelift, ISA-lowering or machine-code certificate";
/// The code shape the model transcribes; checked against `CodeMetadata`.
pub const MODELED_VECTOR_BITS: u16 = 128;
pub const MODELED_TAIL_STRATEGY: &str = "scalar-checked-remainder";
pub const MODELED_ABI: &str = "v1:C:(ptr,u64,u8,ptr-to-u8)->u64";
pub const MODELED_SYMBOL: &str = "vesper_simd_scan_baseline128_v1";

/// Lean's standard axioms; nothing else may appear in the axiom report.
pub const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
/// Number of `#print axioms` lines at the end of the Lean file.
pub const AUDITED_THEOREMS: usize = 13;

/// A logged load, by slice offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    Vector { offset: u64 },
    Scalar { offset: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRun {
    pub count: u64,
    pub xor: u8,
    pub reads: Vec<Read>,
}

/// The scalar reference (`refCount`, `refXor`).
pub fn reference(data: &[u8], delimiter: u8) -> (u64, u8) {
    let count = data.iter().filter(|byte| **byte == delimiter).count() as u64;
    (count, data.iter().fold(0, |acc, byte| acc ^ byte))
}

/// `scan` from the Lean model. Machine words wrap like Cranelift `iadd`; the
/// representability facts the Lean proof establishes are asserted here with
/// checked arithmetic so a violated fixture panics instead of wrapping.
pub fn scan(data: &[u8], delimiter: u8) -> ModelRun {
    let len = u64::try_from(data.len()).expect("slice length fits u64");
    let load = |offset: u64| data[usize::try_from(offset).expect("offset fits usize")];
    // band_imm(len, -16)
    let full = len & (-16i64 as u64);
    let needle = [delimiter; 16];
    let (mut offset, mut count, mut acc) = (0u64, 0u64, [0u8; 16]);
    let mut reads = Vec::new();
    while offset < full {
        reads.push(Read::Vector { offset });
        let chunk: [u8; 16] = std::array::from_fn(|lane| load(offset + lane as u64));
        let eq: [u8; 16] =
            std::array::from_fn(|lane| if chunk[lane] == needle[lane] { 0xff } else { 0 });
        for mask in eq {
            count = count
                .checked_add(u64::from(mask & 1))
                .expect("count fits u64");
        }
        for lane in 0..16 {
            acc[lane] ^= chunk[lane];
        }
        offset = offset.checked_add(16).expect("vector offset fits u64");
    }
    let mut xor = 0u8;
    for byte in acc {
        xor ^= byte;
    }
    while offset < len {
        reads.push(Read::Scalar { offset });
        let byte = load(offset);
        count = count
            .checked_add(u64::from(byte == delimiter))
            .expect("count fits u64");
        xor ^= byte;
        offset = offset.checked_add(1).expect("scalar offset fits u64");
    }
    ModelRun { count, xor, reads }
}

/// The modeled read-legality predicate (`Read.Legal`).
pub fn read_is_legal(read: Read, len: u64) -> bool {
    match read {
        Read::Vector { offset } => offset.checked_add(16).is_some_and(|end| end <= len),
        Read::Scalar { offset } => offset < len,
    }
}

/// Deterministic xorshift64* generator for seeded fixtures.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

/// One case for the Lean executable model.
pub struct LeanCase<'a> {
    pub base: u64,
    pub delimiter: u8,
    pub data: &'a [u8],
}

/// Output of the Lean executable model for one case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeanResult {
    pub count: u64,
    pub xor: u8,
    pub reads: usize,
    pub in_bounds: bool,
}

/// Compiles `lean/SimdScanner.lean` with the given Lean (kernel-checking every
/// proof and checking its `#print axioms` report), then runs
/// `lean/SimdScanner/Fixture.lean` against that artifact. Returns the model
/// header plus one result per case.
pub fn run_lean(
    lean: &str,
    model_file: &std::path::Path,
    fixture_file: &std::path::Path,
    cases: &[LeanCase<'_>],
) -> (String, Vec<LeanResult>) {
    use std::fmt::Write as _;
    use std::io::Write as _;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("vesper-simd-model-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("private Lean output directory");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    // Lean derives the module name from the path relative to its working
    // directory, so compile from the model's own directory.
    let model_dir = model_file.parent().expect("model directory");
    let model_name = model_file.file_name().expect("model file name");
    let compiled = std::process::Command::new(lean)
        .current_dir(model_dir)
        .arg("-o")
        .arg(dir.join("SimdScanner.olean"))
        .arg(model_name)
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8 Lean output");
    let stderr = String::from_utf8_lossy(&compiled.stderr);
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{stderr}",
        compiled.status
    );
    assert!(!report.contains("PANIC") && !stderr.contains("PANIC"));
    // Each `#print axioms` line must list only Lean's standard axioms (no
    // sorryAx, Lean.ofReduceBool or project axiom), and nothing else is printed.
    let mut audited = 0;
    for line in report.lines() {
        let axioms = line
            .split_once(" depends on axioms: [")
            .and_then(|(_, rest)| rest.strip_suffix(']'))
            .unwrap_or_else(|| panic!("unexpected Lean model output: {line}"));
        for axiom in axioms.split(", ") {
            assert!(
                STANDARD_AXIOMS.contains(&axiom),
                "unapproved axiom {axiom} in {line}"
            );
        }
        audited += 1;
    }
    assert_eq!(audited, AUDITED_THEOREMS, "axiom report lines");

    let mut input = String::new();
    for case in cases {
        write!(input, "{} {} ", case.base, case.delimiter).unwrap();
        for byte in case.data {
            write!(input, "{byte:02x}").unwrap();
        }
        input.push('\n');
    }
    let mut child = std::process::Command::new(lean)
        .env("LEAN_PATH", &dir)
        .arg("--run")
        .arg(fixture_file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Lean");
    let mut stdin = child.stdin.take().expect("Lean stdin");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().expect("wait for Lean");
    writer
        .join()
        .expect("writer thread")
        .expect("write Lean stdin");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 Lean output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Lean failed: {}\n{stdout}\n{stderr}",
        output.status
    );
    assert!(!stdout.contains("PANIC") && !stderr.contains("PANIC"));
    let mut lines = stdout.lines();
    let header = lines.next().expect("Lean model header").to_string();
    let results = lines
        .map(|line| {
            let fields: Vec<_> = line.split(' ').collect();
            assert_eq!(fields.len(), 4, "Lean result line: {line}");
            LeanResult {
                count: fields[0].parse().expect("count"),
                xor: fields[1].parse().expect("xor"),
                reads: fields[2].parse().expect("reads"),
                in_bounds: match fields[3] {
                    "true" => true,
                    "false" => false,
                    other => panic!("in_bounds field {other}"),
                },
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), cases.len(), "one Lean result per case");
    (header, results)
}
