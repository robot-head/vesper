//! `lean/ChromaHighlightSafety.lean` (§43.4): kernel-checked when
//! `VESPER_LEAN` is set, and tied to the Rust palettes by its literals.
use std::{path::PathBuf, process::Command};
use vesper_highlight::ChromaPalette;

fn model() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/ChromaHighlightSafety.lean");
    std::fs::read_to_string(path).expect("read the Lean model")
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
        "chroma_contiguous_spans_lossless",
        "chroma_wcag_aaa_gate_sound",
        "chroma_spans_lossless",
        "chroma_verify_reconstructs",
        "verifyGates_sound",
        "dusk_gates",
        "dawn_gates",
    ] {
        assert!(text.contains(&format!("theorem {theorem}")), "{theorem}");
    }
}

#[test]
fn model_literals_match_the_rust_palettes() {
    let text = model();
    for p in [ChromaPalette::vesper_dusk(), ChromaPalette::vesper_dawn()] {
        let primary = [
            p.fg_default,
            p.linear_amber,
            p.inout_cyan,
            p.effect_iris,
            p.wire_emerald,
            p.keyword_rose,
        ]
        .map(|s| s.rel_lum_x1000.to_string())
        .join(", ");
        let call = format!(
            "verifyGates {} [{primary}] {} = true",
            p.bg_card.rel_lum_x1000, p.comment_slate.rel_lum_x1000
        );
        assert!(text.contains(&call), "{} needs `{call}`", p.name);
        p.verify_contrast_gates().unwrap();
    }
}

#[test]
fn lean_kernel_checks_the_model() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; ChromaHighlightSafety.lean not kernel-checked");
        return;
    };
    // Lean rejects symlinked inputs outside its root (Bazel runfiles), so
    // check a regular copy.
    let dir = std::env::temp_dir().join(format!("vesper-chroma-lean-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("ChromaHighlightSafety.lean");
    std::fs::write(&copy, model()).unwrap();
    // An elan proxy (`VESPER_LEAN=lean`, as on Windows) picks the toolchain
    // from `lean-toolchain`, so run from the repository root when it is there.
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
