//! Differential test of the Rust WIT table against the executable Lean model
//! in `lean/NexusHandles.lean`.
//!
//! Random operation sequences (inserts, lookups and borrows of issued,
//! stale, forged and cross-tenant handles, drops, transfers, teardown, with a
//! low generation ceiling so slots retire) run on `WitResourceTable` and are
//! replayed through Lean's `step`; every result must match. This is evidence
//! about the replayed runs; the general statements are the Lean theorems,
//! which are about the model, not the Rust code.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use vesper_nexus::{WitHandle, WitResourceTable, WitTableError};

const MODEL_ID: &str = "vesper.nexus.wit-handles-model/1";
const MODEL_CLAIM: &str =
    "model of the generational WIT handle table; not a proof about the Rust implementation";
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const AUDITED_THEOREMS: usize = 15;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean")
}
fn lean_source() -> String {
    std::fs::read_to_string(root().join("NexusHandles.lean")).expect("Lean model")
}

fn err_name(e: &WitTableError) -> &'static str {
    match e {
        WitTableError::InvalidOrStaleHandle(_) => "invalidOrStale",
        WitTableError::UnauthorizedTenant { .. } => "unauthorizedTenant",
        WitTableError::ResourceHasActiveBorrows { .. } => "activeBorrows",
        WitTableError::NoActiveBorrow(_) => "noActiveBorrow",
        WitTableError::BorrowOverflow(_) => "borrowOverflow",
        WitTableError::TableFull { .. } => "tableFull",
        WitTableError::GenerationExhausted(_) => "generationExhausted",
        // Host-owner bookkeeping, not part of the guest-integer model.
        WitTableError::ForeignOwner | WitTableError::StillLive(_) => "unmodelled",
    }
}

#[test]
fn lean_and_rust_vocabularies_agree() {
    let src = lean_source();
    assert!(src.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert!(src.contains(MODEL_CLAIM));
    let h = WitHandle {
        idx: 0,
        generation: 0,
    };
    for e in [
        WitTableError::InvalidOrStaleHandle(h),
        WitTableError::UnauthorizedTenant { actual: 0 },
        WitTableError::ResourceHasActiveBorrows {
            idx: 0,
            active_borrows: 1,
        },
        WitTableError::NoActiveBorrow(h),
        WitTableError::BorrowOverflow(h),
        WitTableError::TableFull { capacity: 0 },
        WitTableError::GenerationExhausted(h),
    ] {
        let name = err_name(&e);
        assert!(
            src.contains(&format!("| .{name} => \"{name}\"")),
            "Lean Err lacks {name}"
        );
    }
    assert!(src.contains("def borrowMax : Nat := 4294967295"));
    for theorem in [
        "wit_handle_tenant_isolated",
        "wit_drop_own_rejects_active_borrows",
    ] {
        assert!(src.contains(&format!("theorem {theorem}")), "{theorem}");
    }
    assert_eq!(src.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    for banned in ["sorry", "admit"] {
        assert!(!src.contains(banned), "Lean model contains {banned}");
    }
    assert!(
        !src.lines().any(|l| l.trim_start().starts_with("axiom ")),
        "Lean model declares an axiom"
    );
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

/// Run one random sequence on the Rust table, returning the Lean input
/// lines and the expected (Rust) output lines.
fn record(seed: u64, steps: usize, input: &mut String, expected: &mut Vec<String>) {
    const CAP: u32 = 6;
    const MAX_GEN: u32 = 4;
    let mut rng = Rng(seed);
    let mut t = WitResourceTable::<u64>::with_generation_limit(CAP, MAX_GEN);
    let mut issued: Vec<WitHandle> = Vec::new();
    input.push_str(&format!("init {CAP} {MAX_GEN}\n"));
    expected.push("ok".into());
    let render_unit = |r: Result<(), WitTableError>| match r {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("err {}", err_name(&e)),
    };
    for step in 0..steps {
        let tenant = rng.below(3);
        let h = if !issued.is_empty() && rng.below(4) != 0 {
            let h = issued[rng.below(issued.len() as u64) as usize];
            match rng.below(5) {
                0 => WitHandle {
                    generation: h.generation + 1,
                    ..h
                },
                1 => WitHandle {
                    idx: h.idx + 1,
                    ..h
                },
                _ => h,
            }
        } else {
            WitHandle {
                idx: rng.below(8) as u32,
                generation: rng.below(6) as u32,
            }
        };
        let hs = format!("{} {}", h.idx, h.generation);
        let (line, out) = match rng.below(10) {
            0..=2 => {
                let v = step as u64;
                let out = match t.insert_own(tenant, v) {
                    Ok(own) => {
                        let h = t.lower_to_guest(own);
                        issued.push(h);
                        format!("ok {} {}", h.idx, h.generation)
                    }
                    Err((_, e)) => format!("err {}", err_name(&e)),
                };
                (format!("insert {tenant} {v}"), out)
            }
            3 => (
                format!("lookup {tenant} {hs}"),
                match t.lookup(tenant, h) {
                    Ok(v) => format!("ok {v}"),
                    Err(e) => format!("err {}", err_name(&e)),
                },
            ),
            4 | 5 => (
                format!("borrow {tenant} {hs}"),
                render_unit(t.begin_borrow_raw(tenant, h)),
            ),
            6 => (
                format!("unborrow {tenant} {hs}"),
                render_unit(t.end_borrow_raw(tenant, h)),
            ),
            7 => (
                format!("drop {tenant} {hs}"),
                match t.drop_own_raw(tenant, h) {
                    Ok(v) => format!("ok {v}"),
                    Err(e) => format!("err {}", err_name(&e)),
                },
            ),
            8 => {
                let to = rng.below(3);
                (
                    format!("transfer {tenant} {hs} {to}"),
                    match t.transfer_raw(tenant, h, to) {
                        Ok(h) => {
                            issued.push(h);
                            format!("ok {} {}", h.idx, h.generation)
                        }
                        Err(e) => format!("err {}", err_name(&e)),
                    },
                )
            }
            _ if rng.below(4) == 0 => (
                format!("reclaim {tenant}"),
                format!("ok {}", t.reclaim_tenant(tenant, |_| {})),
            ),
            _ => (
                format!("lookup {tenant} {hs}"),
                match t.lookup(tenant, h) {
                    Ok(v) => format!("ok {v}"),
                    Err(e) => format!("err {}", err_name(&e)),
                },
            ),
        };
        input.push_str(&line);
        input.push('\n');
        expected.push(out);
    }
}

/// `dir` plus the toolchain's own library directory.
fn lean_path(lean: &str, dir: &Path) -> std::ffi::OsString {
    let mut paths = vec![dir.to_path_buf()];
    if let Ok(out) = std::process::Command::new(lean)
        .arg("--print-libdir")
        .output()
        && out.status.success()
    {
        let lib = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !lib.is_empty() {
            paths.push(PathBuf::from(lib));
        }
    }
    std::env::join_paths(paths).expect("LEAN_PATH")
}

fn run_lean(lean: &str, input: &str) -> (String, Vec<String>) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("vesper-nexus-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("private Lean output directory");
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let compiled = std::process::Command::new(lean)
        .current_dir(root())
        .arg("-o")
        .arg(dir.join("NexusHandles.olean"))
        .arg("NexusHandles.lean")
        .output()
        .expect("spawn Lean");
    let report = String::from_utf8(compiled.stdout).expect("UTF-8");
    let stderr = String::from_utf8_lossy(&compiled.stderr);
    assert!(
        compiled.status.success(),
        "Lean model failed: {}\n{report}\n{stderr}",
        compiled.status
    );
    let mut audited = 0;
    for line in report.lines() {
        if line.ends_with("does not depend on any axioms") {
            audited += 1;
            continue;
        }
        let axioms = line
            .split_once(" depends on axioms: [")
            .and_then(|(_, rest)| rest.strip_suffix(']'))
            .unwrap_or_else(|| panic!("unexpected Lean output: {line}"));
        for axiom in axioms.split(", ") {
            assert!(
                STANDARD_AXIOMS.contains(&axiom),
                "unapproved axiom {axiom} in {line}"
            );
        }
        audited += 1;
    }
    assert_eq!(audited, AUDITED_THEOREMS, "axiom report lines");

    let mut child = std::process::Command::new(lean)
        .env("LEAN_PATH", lean_path(lean, &dir))
        .arg("--run")
        .arg(root().join("NexusHandles/Fixture.lean"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Lean");
    let mut stdin = child.stdin.take().expect("stdin");
    let owned = input.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(owned.as_bytes()));
    let output = child.wait_with_output().expect("wait for Lean");
    writer.join().expect("writer").expect("write stdin");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        output.status.success(),
        "Lean fixture failed: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = stdout.lines().map(str::to_string);
    let header = lines.next().expect("header");
    (header, lines.collect())
}

#[test]
fn rust_sequences_cover_every_outcome() {
    let (mut input, mut expected) = (String::new(), Vec::new());
    for seed in 1..=8 {
        record(0x5eed_0000 + seed, 600, &mut input, &mut expected);
    }
    for name in [
        "invalidOrStale",
        "unauthorizedTenant",
        "activeBorrows",
        "noActiveBorrow",
        "tableFull",
        "generationExhausted",
    ] {
        assert!(
            expected.iter().any(|l| l == &format!("err {name}")),
            "no {name} outcome"
        );
    }
    assert!(
        expected
            .iter()
            .any(|l| l.starts_with("ok ") && l.split(' ').count() == 3)
    );
}

/// The same sequences through the executable Lean model. Requires `VESPER_LEAN`.
#[test]
fn lean_model_matches_rust_table() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean executable-model differential skipped");
        return;
    };
    let (mut input, mut expected) = (String::new(), Vec::new());
    for seed in 1..=8 {
        record(0x5eed_0000 + seed, 600, &mut input, &mut expected);
    }
    input.push_str("bogus line\n");
    expected.push("unknown".into());
    let (header, lines) = run_lean(&lean, &input);
    assert_eq!(header, format!("{MODEL_ID} | {MODEL_CLAIM}"));
    assert_eq!(lines.len(), expected.len(), "one Lean line per input line");
    let inputs: Vec<&str> = input.lines().collect();
    for (i, (got, want)) in lines.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "line {i}: {}", inputs[i]);
    }
    eprintln!("Lean table model agreed on {} operations", lines.len());
}
