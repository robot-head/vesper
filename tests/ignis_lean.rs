//! `lean/IgnisTriBackendEquiv.lean` (§38.10): verified structurally against
//! D3D12_BARRIER_SYNC constants (`crates/vesper-ignis/src/d3d12.rs`) and
//! kernel-checked when `VESPER_LEAN` is set.
use std::{path::PathBuf, process::Command};

fn model() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/IgnisTriBackendEquiv.lean");
    std::fs::read_to_string(path).expect("read IgnisTriBackendEquiv.lean")
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
        "tri_backend_barriers_preserve_hazards",
        "lowerEdge_preserves_edge",
        "lowerBarriers_sound",
        "tri_backend_barriers_preserve_any_dag",
    ] {
        assert!(
            text.contains(&format!("theorem {theorem}")),
            "missing theorem {theorem}"
        );
    }
}

#[test]
fn model_d3d12_sync_bits_match_spec() {
    let text = model();
    // Matches `vesper_ignis::Sync` bit constants in `crates/vesper-ignis/src/d3d12.rs`.
    let expected: [(&str, u32); 9] = [
        ("all", 0x1),
        ("vertexShading", 0x8),
        ("pixelShading", 0x10),
        ("depthStencil", 0x20),
        ("renderTarget", 0x40),
        ("computeShading", 0x80),
        ("raytracing", 0x100),
        ("copy", 0x200),
        ("allShading", 0x1000),
    ];
    for (variant, bits) in expected {
        let needle = format!("| .{variant} => {bits:#x}");
        assert!(
            text.contains(&needle),
            "IgnisTriBackendEquiv.lean missing `{needle}` for Sync::{variant}"
        );
    }
}

#[test]
fn lean_kernel_checks_the_model() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; IgnisTriBackendEquiv.lean not kernel-checked locally");
        return;
    };
    let dir = std::env::temp_dir().join(format!("vesper-ignis-lean-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("IgnisTriBackendEquiv.lean");
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
