//! Checks `lean/SiliconFrames.lean` and `lean/SiliconAia.lean`: always scans
//! them for `sorry`, `admit` and new axioms, and checks that the QEMU output
//! `SiliconAia.lean` proves its model reproduces is the one
//! `tests/aia_qemu.rs` compares against; elaborates and kernel-checks both
//! when `VESPER_LEAN` names a Lean 4 binary (as in CI). The theorems are
//! about the Lean models, not about this crate's Rust code.

#[path = "common/aia_hyp_expected.rs"]
mod aia_hyp_expected;

use std::path::PathBuf;
use std::process::Command;

fn lean_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../lean")
        .join(name)
}

fn lean_file() -> PathBuf {
    lean_path("SiliconFrames.lean")
}

const ESCAPES: [&str; 6] = [
    "sorry",
    "admit",
    "axiom ",
    "unsafe ",
    "implemented_by",
    "native_decide",
];

fn strip_comments(src: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"/-") {
            depth += 1;
            i += 2;
        } else if depth > 0 && b[i..].starts_with(b"-/") {
            depth -= 1;
            i += 2;
        } else if depth == 0 && b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else {
            if depth == 0 {
                out.push(b[i] as char);
            }
            i += 1;
        }
    }
    out
}

#[test]
fn lean_model_has_no_escape_hatches() {
    let src = std::fs::read_to_string(lean_file()).expect("read Lean file");
    let code = strip_comments(&src);
    for word in ESCAPES {
        assert!(!code.contains(word), "SiliconFrames.lean uses `{word}`");
    }
    for theorem in [
        "w1c_preserves_unwritten",
        "w1c_single_ack",
        "rmw_clears_all_pending",
        "step_preserves_pfns",
        "step_preserves_nodup",
        "count_partition",
        "step_conserves_total",
        "no_reuse_without_shootdown",
        "pending_exit_is_shootdown",
    ] {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "missing {theorem}"
        );
    }
}

#[test]
fn aia_model_has_no_escape_hatches() {
    let src = std::fs::read_to_string(lean_path("SiliconAia.lean")).expect("read Lean file");
    let code = strip_comments(&src);
    for word in ESCAPES {
        assert!(!code.contains(word), "SiliconAia.lean uses `{word}`");
    }
    for theorem in [
        "top_spec",
        "top_none",
        "claim_clears_top",
        "claim_keeps_others",
        "msi_asserts",
        "external_file_is_silent",
        "topei_ignores_delivery",
        "warl_guest_never_external",
        "level_file_on",
        "level_external",
        "level_off",
        "msiPage_injective",
        "qemu_s_level_drops_hart",
        "forward_pending",
        "forward_count",
        "forward_targets",
        "hgeip_iff",
        "vseip_iff",
        "hgeie_bit0",
        "sgeip_needs_hgeie",
        "vs_access_iff",
        "vsclaim_isolated",
        "vsclaim_own_file",
        "toCpu_sound",
        "toCpu_none",
        "roundtrip",
        "compose_sound",
        "mapId_sound",
        "mapId_none",
        "mapId_mask_idem",
        "hypTrace_matches_qemu",
    ] {
        assert!(
            code.contains(&format!("theorem {theorem}")),
            "missing {theorem}"
        );
    }
}

/// The list `hypTrace_matches_qemu` proves the Lean model produces is the
/// QEMU 8.2.2 output `tests/aia_qemu.rs` checks the Rust model and live
/// QEMU against.
#[test]
fn aia_model_trace_is_the_recorded_qemu_output() {
    let src = std::fs::read_to_string(lean_path("SiliconAia.lean")).expect("read Lean file");
    let code = strip_comments(&src);
    let start = code.find("def qemuHyp : List Nat :=").expect("qemuHyp");
    let open = start + code[start..].find('[').unwrap();
    let close = open + code[open..].find(']').unwrap();
    let values: Vec<u64> = code[open + 1..close]
        .split(',')
        .map(str::trim)
        .map(|v| match v {
            "FAULT" => aia_hyp_expected::FAULT,
            v => match v.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16).unwrap(),
                None => v.parse().unwrap(),
            },
        })
        .collect();
    assert_eq!(values, aia_hyp_expected::HYPERVISOR_EXPECTED);
    assert!(code.contains("def FAULT : Nat := 0xfa17000000000002"));
}

#[test]
fn lean_model_checks() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; SiliconFrames.lean and SiliconAia.lean not elaborated");
        return;
    };
    for file in [lean_file(), lean_path("SiliconAia.lean")] {
        elaborate(&lean, &file);
    }
}

fn elaborate(lean: &str, file: &PathBuf) {
    let out = Command::new(lean)
        .arg(file)
        .output()
        .unwrap_or_else(|e| panic!("run {lean}: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "lean failed:\n{text}");
    assert!(!text.contains("error"), "lean reported errors:\n{text}");
    assert!(!text.contains("declaration uses 'sorry'"), "{text}");
}
