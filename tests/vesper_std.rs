//! Port wave P0 lane 8 (#897): the `@vesper-std` packages under
//! packages/vesper-std/, written in Vesper.
//!
//! - Every package checks on its own, within a capability ceiling of at most
//!   `["Fail", "Alloc"]`.
//! - The unit tests in packages/vesper-std/tests (`run_all`, one check per
//!   public function) pass in the evaluator and natively.
//! - The Strata conformance traces of #847
//!   (crates/vesper-alloc/tests/fixtures/strata/) regenerate byte for byte
//!   from the Vesper `CacheBTree` and `RadixSeq`: the Vesper ports of the
//!   trace generators return the FNV-1a 64 hash of the text they produce,
//!   which must equal the hash of the checked-in trace without its `#` lines.
use std::path::Path;
use vesper::{check, native, packages, runtime::Runtime};

const STD: &str = "packages/vesper-std";
const TESTS: &str = "packages/vesper-std/tests";
const TRACES: &str = "crates/vesper-alloc/tests/fixtures/strata";
const PACKAGES: [&str; 4] = ["core", "collections", "bytes", "fmt"];

/// Entry point, trace file stem, in the order `trace_digest` folds them.
const TRACE_ENTRIES: [(&str, &str); 7] = [
    ("trace_small_churn", "cache_btree_small_churn"),
    ("trace_dense_churn", "cache_btree_dense_churn"),
    ("trace_wide", "cache_btree_wide"),
    ("trace_ascending", "cache_btree_ascending"),
    ("trace_descending", "cache_btree_descending"),
    ("trace_seq_small", "chunk_seq_small"),
    ("trace_seq_deep", "chunk_seq_deep"),
];

fn world(dir: &str) -> check::Checked {
    let assembled = packages::load(Path::new(dir)).unwrap();
    assert!(
        assembled.ok(),
        "{dir}: {:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let checked = check::check(&assembled.module);
    let localized: Vec<_> = checked
        .diagnostics
        .iter()
        .map(|d| assembled.sources.localize(d))
        .collect();
    assert!(checked.diagnostics.is_empty(), "{dir}: {localized:?}");
    checked
}

/// FNV-1a 64 of a trace file's non-comment lines, newlines included.
fn trace_hash(stem: &str) -> u64 {
    let text = std::fs::read_to_string(Path::new(TRACES).join(format!("{stem}.trace")))
        .unwrap()
        .replace("\r\n", "\n");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        for b in line.bytes().chain(*b"\n") {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

fn evaluate(checked: &check::Checked, entry: &str) -> String {
    let mut runtime = Runtime::new(&checked.world);
    runtime.optimize = false;
    runtime.fuel = 4_000_000_000;
    runtime.run(entry, vec![]).unwrap().to_string()
}

fn jit(checked: &check::Checked, entry: &str) -> String {
    native::jit_with_options(&checked.world, entry, false)
        .unwrap()
        .to_string()
}

#[test]
fn every_package_checks_on_its_own_within_the_ceiling() {
    for name in PACKAGES {
        let dir = format!("{STD}/{name}");
        world(&dir);
        let manifest = std::fs::read_to_string(format!("{dir}/Loom.toml")).unwrap();
        assert!(
            manifest.contains(&format!("name = \"@vesper-std/{name}\"")),
            "{dir}"
        );
        let ceiling = manifest
            .lines()
            .find_map(|l| l.strip_prefix("capability_ceiling = "))
            .unwrap_or_else(|| panic!("{dir}: no capability_ceiling"));
        for cap in ceiling.trim_matches(['[', ']']).split(',') {
            let cap = cap.trim().trim_matches('"');
            assert!(
                cap.is_empty() || cap == "Fail" || cap == "Alloc",
                "{dir}: capability {cap} is above the std ceiling"
            );
        }
    }
}

// `1000 * t + c` names check `c` of test `t` (see
// packages/vesper-std/tests/src/main.ves).
#[test]
fn unit_tests_pass_in_the_evaluator() {
    let checked = world(TESTS);
    assert_eq!(evaluate(&checked, "run_small"), "0", "small sizes");
    assert_eq!(evaluate(&checked, "run_all"), "0", "full sizes");
}

#[test]
fn unit_tests_pass_natively() {
    let checked = world(TESTS);
    assert_eq!(jit(&checked, "run_small"), "0", "small sizes");
    assert_eq!(jit(&checked, "run_all"), "0", "full sizes");
    // The other native architecture the repository builds for compiles the
    // suite too (CI runs the JIT on x86-64 Linux and AArch64 macOS).
    let other = if cfg!(target_arch = "aarch64") {
        "x86_64-unknown-linux-gnu"
    } else {
        "aarch64-unknown-linux-gnu"
    };
    let (object, _) = native::object_with_options(&checked.world, "run_all", Some(other), false)
        .unwrap_or_else(|e| panic!("{other}: {e}"));
    assert_eq!(&object[..4], b"\x7fELF", "{other}");
}

#[test]
fn conformance_traces_replay_identically_natively() {
    let checked = world(TESTS);
    let expected = TRACE_ENTRIES.iter().fold(0u64, |h, (_, stem)| {
        h.wrapping_mul(31).wrapping_add(trace_hash(stem))
    });
    let digest = jit(&checked, "trace_digest");
    if digest != expected.to_string() {
        // Name the trace that differs.
        for (entry, stem) in TRACE_ENTRIES {
            assert_eq!(
                jit(&checked, entry),
                trace_hash(stem).to_string(),
                "{stem}: the Vesper collections diverge from the checked-in trace"
            );
        }
        panic!("trace digest {digest} != {expected}, but every trace matches");
    }
}

#[test]
fn a_conformance_trace_replays_identically_in_the_evaluator() {
    let checked = world(TESTS);
    assert_eq!(
        evaluate(&checked, "trace_small_churn"),
        trace_hash("cache_btree_small_churn").to_string()
    );
}

#[test]
fn the_trace_hash_sees_every_result() {
    // A changed result anywhere in a trace changes its hash.
    let stem = "cache_btree_small_churn";
    let text = std::fs::read_to_string(Path::new(TRACES).join(format!("{stem}.trace"))).unwrap();
    let edited = text.replacen("=> none", "=> 0", 1);
    assert_ne!(text, edited);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in edited.lines().filter(|l| !l.starts_with('#')) {
        for b in line.bytes().chain(*b"\n") {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    assert_ne!(h, trace_hash(stem));
}
