//! `lean/WalSafety.lean` (§8.3): verified structurally and kernel-checked when
//! `VESPER_LEAN` is set.
use std::{path::PathBuf, process::Command};

fn model() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/WalSafety.lean");
    std::fs::read_to_string(path).expect("read WalSafety.lean")
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
        "Broker.config_preserved_by_setIndexMetrics",
        "Broker.get_set_indexMetrics",
        "appendBatch_crc_err_aborts_cleanly",
        "appendBatch_ok_commits_and_releases",
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
        eprintln!("VESPER_LEAN unset; WalSafety.lean not kernel-checked locally");
        return;
    };
    let dir = std::env::temp_dir().join(format!("vesper-wal-safety-lean-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("WalSafety.lean");
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
