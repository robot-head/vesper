//! Lean model checks for `lean/AegisToctou.lean` and
//! `lean/AegisCollateral.lean`.
//!
//! The static audit always runs. With `VESPER_LEAN` set (as in CI), the
//! model is compiled (which kernel-checks every proof and prints the axiom
//! report) and its printed policy fixture is compared with the Rust
//! `verify_attestation_claims` on the same 128 claim sets. Agreement on
//! inputs is a differential test, not a proof that the Rust code refines
//! the model.

use std::path::PathBuf;
use std::process::Command;
use vesper_aegis::claims::SupplyChainEvidence;
use vesper_aegis::policy::TcbFloors;
use vesper_aegis::*;

const MODEL_ID: &str = "vesper.aegis.toctou-policy-model/1";
const AUDITED_THEOREMS: usize = 7;
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

fn lean_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean/AegisToctou.lean")
}

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
    String::from_utf8(out).expect("UTF-8 model")
}

#[test]
fn lean_model_source_audit() {
    let source = std::fs::read_to_string(lean_file()).expect("read model");
    assert!(source.contains(&format!("def modelId : String := \"{MODEL_ID}\"")));
    assert_eq!(source.matches("\n#print axioms ").count(), AUDITED_THEOREMS);
    for theorem in [
        "theorem snapshot_blocks_hypervisor_toctou",
        "theorem mutation_after_snapshot_is_invisible",
        "theorem double_fetch_interferes",
        "theorem verify_quote_enforces_supply_chain_and_tcb",
        "theorem checkPolicy_complete",
        "theorem debug_reported_first",
    ] {
        assert!(source.contains(theorem), "missing {theorem}");
    }
    let code = strip_lean_comments(&source);
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "axiom ",
        "partial ",
        "unsafe ",
    ] {
        assert!(!code.contains(forbidden), "model contains {forbidden:?}");
    }
}

fn rust_result(mask: u32) -> &'static str {
    let bit = |k: u32| (mask >> k) & 1 == 1;
    let arch = if bit(0) {
        TeeArch::ArmCcaRealm
    } else {
        TeeArch::IntelTdx15
    };
    let binding = if bit(6) { 7u8 } else { 4 };
    let claims = AttestationClaims {
        arch,
        tcb_svn: if bit(2) { 9 } else { 10 },
        enclave_flags: if bit(1) {
            0
        } else {
            ENCLAVE_FLAG_DEBUG_DISABLED
        },
        mr_enclave_sha384: [if bit(3) { 0 } else { 1 }; 48],
        loom_lock_blake3: [if bit(4) { 5 } else { 2 }; 32],
        olean_spec_mif: [if bit(5) { 6 } else { 3 }; 32],
        chan_binding_prefix: [binding; 8],
        channel_binding: [binding; 48],
        supply_chain_evidence: SupplyChainEvidence::RegisterReplay,
        snp_reported_tcb: None,
        snp_vmpl: None,
    };
    let policy = RelyingPartyPolicy {
        allowed_archs: vec![TeeArch::IntelTdx15, TeeArch::AmdSevSnp],
        tcb_floors: TcbFloors {
            tdx: 10,
            snp: 10,
            cca: 10,
        },
        snp_component_floors: None,
        snp_max_vmpl: 0,
        allowed_measurements: vec![
            (TeeArch::IntelTdx15, [1; 48]),
            (TeeArch::AmdSevSnp, [1; 48]),
        ],
        expected_loom_blake3: [2; 32],
        expected_olean_mif: [3; 32],
        require_register_replay: false,
    };
    use AegisVerifyError::*;
    match verify_attestation_claims(&claims, &policy, &[4; 48]) {
        Ok(()) => "ok",
        Err(DisallowedTeeArchitecture(_)) => "arch",
        Err(EnclaveDebugModeEnabled) => "debug",
        Err(StaleTcbSvn { .. }) => "tcb",
        Err(BootMeasurementMismatch) => "measurement",
        Err(SupplyChainLoomLockMismatch) => "loom",
        Err(FormalOleanProofDigestMismatch) => "olean",
        Err(ChannelTranscriptBindingMismatch) => "binding",
        Err(e) => panic!("unmodelled error {e:?}"),
    }
}

#[test]
fn lean_model_matches_rust_policy() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean model compile and differential skipped");
        return;
    };
    let out = Command::new(lean)
        .arg(lean_file())
        .output()
        .expect("run lean");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "lean failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("AEGIS-POLICY "))
        .expect("fixture line");
    let lean_results: Vec<&str> = line.split(',').collect();
    assert_eq!(lean_results.len(), 128);
    for (mask, lean_r) in lean_results.iter().enumerate() {
        assert_eq!(*lean_r, rust_result(mask as u32), "mask {mask:#09b}");
    }
    let mut audited = 0;
    for l in stdout.lines().filter(|l| l.contains("depends on axioms")) {
        audited += 1;
        let axioms = l.split('[').nth(1).unwrap_or("").trim_end_matches(']');
        for ax in axioms.split(',').map(str::trim).filter(|a| !a.is_empty()) {
            assert!(
                STANDARD_AXIOMS.contains(&ax),
                "non-standard axiom {ax} in {l}"
            );
        }
    }
    assert_eq!(audited, AUDITED_THEOREMS);
    assert!(!stdout.contains("sorryAx"));
}

// ------------------------------------------- lean/AegisCollateral.lean --

const COLLATERAL_MODEL_ID: &str = "vesper.aegis.tcb-evaluation-model/1";
const COLLATERAL_AUDITED_THEOREMS: usize = 15;

fn collateral_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lean/AegisCollateral.lean")
}

#[test]
fn collateral_model_source_audit() {
    let source = std::fs::read_to_string(collateral_file()).expect("read model");
    assert!(source.contains(&format!(
        "def modelId : String := \"{COLLATERAL_MODEL_ID}\""
    )));
    assert_eq!(
        source.matches("\n#print axioms ").count(),
        COLLATERAL_AUDITED_THEOREMS
    );
    for theorem in [
        "theorem converge_monotone",
        "theorem find_monotone",
        "theorem evaluate_monotone",
        "theorem evaluate_revoked_qe",
        "theorem evaluate_revoked_module",
        "theorem evaluate_module_first",
        "theorem evaluate_platform_before_qe",
        "theorem rollback_rejected",
        "theorem appraise_sound",
        "theorem appraise_refs_mono",
        "theorem appraise_rejects_unknown",
    ] {
        assert!(source.contains(theorem), "missing {theorem}");
    }
    let code = strip_lean_comments(&source);
    for forbidden in [
        "sorry",
        "admit",
        "native_decide",
        "axiom ",
        "partial ",
        "unsafe ",
    ] {
        assert!(!code.contains(forbidden), "model contains {forbidden:?}");
    }
}

/// `TcbStatus::converge` on every (platform, QE/module) pair, platform
/// first, in the model's order.
fn rust_converge_table() -> Vec<&'static str> {
    use vesper_aegis::intel::TcbStatus::*;
    let platform = [
        UpToDate,
        SWHardeningNeeded,
        ConfigurationNeeded,
        ConfigurationAndSWHardeningNeeded,
        OutOfDate,
        OutOfDateConfigurationNeeded,
        Revoked,
    ];
    let isv = [UpToDate, OutOfDate, Revoked];
    platform
        .iter()
        .flat_map(|p| isv.iter().map(move |o| p.converge(*o).as_str()))
        .collect()
}

#[test]
fn collateral_model_matches_rust_converge() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; collateral model compile and differential skipped");
        return;
    };
    let out = Command::new(lean)
        .arg(collateral_file())
        .output()
        .expect("run lean");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "lean failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains(&format!("AEGIS-COLLATERAL-MODEL {COLLATERAL_MODEL_ID}")));
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("AEGIS-CONVERGE "))
        .expect("converge fixture line");
    let lean_table: Vec<&str> = line.split(',').collect();
    assert_eq!(lean_table, rust_converge_table());
    let mut audited = 0;
    for l in stdout.lines() {
        if l.contains("does not depend on any axioms") {
            audited += 1;
        } else if l.contains("depends on axioms") {
            audited += 1;
            let axioms = l.split('[').nth(1).unwrap_or("").trim_end_matches(']');
            for ax in axioms.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                assert!(
                    STANDARD_AXIOMS.contains(&ax),
                    "non-standard axiom {ax} in {l}"
                );
            }
        }
    }
    assert_eq!(audited, COLLATERAL_AUDITED_THEOREMS);
    assert!(!stdout.contains("sorryAx"));
}
