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
        "stage_only_compute_waw_rejected",
        "read_only_access_compute_waw_rejected",
        "vk_shading_masks_cover_vertex_attribute_input",
        "vertex_shader_only_missing_attribute_input_rejected",
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
fn model_d3d12_access_bits_match_spec() {
    let text = model();
    // Matches `vesper_ignis::Access` bit constants in `crates/vesper-ignis/src/d3d12.rs`.
    let expected: [(&str, u32); 12] = [
        ("noAccess", 0x8000_0000),
        ("vertexBuffer", 0x1),
        ("constantBuffer", 0x2),
        ("indexBuffer", 0x4),
        ("renderTarget", 0x8),
        ("unorderedAccess", 0x10),
        ("depthStencilWrite", 0x20),
        ("depthStencilRead", 0x40),
        ("shaderResource", 0x80),
        ("indirectArgument", 0x200),
        ("copyDest", 0x400),
        ("copySource", 0x800),
    ];
    for (variant, bits) in expected {
        let needle = format!("| .{variant} => {bits:#x}");
        assert!(
            text.contains(&needle),
            "IgnisTriBackendEquiv.lean missing `{needle}` for Access::{variant}"
        );
    }
}

#[test]
fn model_vk_stage_and_access_masks_match_translate() {
    let text = model();
    // Matches `SYNC_TABLE` in `crates/vesper-ignis-vulkan/src/translate.rs`
    // (including `VERTEX_ATTRIBUTE_INPUT = 0x20_0000_0000` in VERTEX_SHADING and ALL_SHADING).
    let expected_stages: [(&str, u64); 9] = [
        ("all", 0x10000),
        ("vertexShading", 0x20_0000_0008),
        ("pixelShading", 0x80),
        ("depthStencil", 0x300),
        ("renderTarget", 0x400),
        ("computeShading", 0x800),
        ("raytracing", 0x200000),
        ("copy", 0x9_0000_0000),
        ("allShading", 0x60_0000_0880),
    ];
    for (variant, bits) in expected_stages {
        let needle = format!("| .{variant} => {bits:#x}");
        assert!(
            text.contains(&needle),
            "IgnisTriBackendEquiv.lean missing `{needle}` for vkStageMask .{variant}"
        );
    }

    // Matches `ACCESS_TABLE` in `crates/vesper-ignis-vulkan/src/translate.rs`.
    let expected_accesses: [(&str, u64); 12] = [
        ("noAccess", 0x0),
        ("vertexBuffer", 0x4),
        ("constantBuffer", 0x8),
        ("indexBuffer", 0x2),
        ("renderTarget", 0x180),
        ("unorderedAccess", 0x6_0000_0000),
        ("depthStencilWrite", 0x600),
        ("depthStencilRead", 0x200),
        ("shaderResource", 0x3_0000_0000),
        ("indirectArgument", 0x1),
        ("copyDest", 0x1000),
        ("copySource", 0x800),
    ];
    for (variant, bits) in expected_accesses {
        let needle = format!("| .{variant} => {bits:#x}");
        assert!(
            text.contains(&needle),
            "IgnisTriBackendEquiv.lean missing `{needle}` for vkAccessMask .{variant}"
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
