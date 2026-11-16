//! `lean/VesperUnifiedTypeTheory.lean` (Appendix A.3): verified structurally
//! and kernel-checked when `VESPER_LEAN` is set.
use std::{path::PathBuf, process::Command};

fn model() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/VesperUnifiedTypeTheory.lean");
    std::fs::read_to_string(path).expect("read VesperUnifiedTypeTheory.lean")
}

#[test]
fn model_has_no_escape_hatches() {
    let text = model();
    for line in text.lines() {
        let code = line.split("--").next().unwrap_or("");
        for word in ["sorry", "admit", "axiom", "native_decide", "unsafe"] {
            assert!(
                !code
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|w| w == word),
                "{word} in: {line}"
            );
        }
    }
    for theorem in [
        "modevec_le_trans",
        "exclusive_place_never_duplicated",
        "ordered_owner_never_duplicated",
        "shared_linear_view_can_split",
        "capsule_rejects_thread_affinity",
        "capsule_rejects_outer_capability",
        "capsule_rejects_external_alias",
        "capsule_rejects_shared_owner",
        "capsule_rejects_ordered_owner",
        "capsule_seal_produces_portable_uncontended",
        "lifo_rejects_out_of_order_irq_restore",
        "ghost_linear_token_zero_runtime_footprint",
        "out_split_slice_disjoint_and_exhaustive",
        "semver_refinement_variance_sound",
        "graded_region_loop_elides_limit_checks",
        "uncompacted_zset_eq_compacted",
    ] {
        assert!(
            text.contains(&format!("theorem {theorem}")),
            "missing theorem {theorem}"
        );
    }
}

#[test]
fn lean_kernel_checks_the_model() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; VesperUnifiedTypeTheory.lean not kernel-checked locally");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "vesper-unified-type-theory-lean-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("VesperUnifiedTypeTheory.lean");
    std::fs::write(&copy, model()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let cwd = if root.join("lean-toolchain").is_file() {
        root
    } else {
        dir.as_path()
    };
    let out = Command::new(lean)
        .arg(&copy)
        .current_dir(cwd)
        .output()
        .expect("run lean");
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        out.status.success(),
        "lean failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
