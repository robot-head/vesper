//! Check `lean/SimdJsonQuotes.lean` (quote parity by carry-less multiply and
//! the byte classification tables) with the pinned Lean when `VESPER_LEAN` is
//! set (as in CI); otherwise report a skip. The proofs are about the Lean
//! definitions, not the compiled Rust; this test also checks that the
//! tables transcribed there are the ones `src/simd/kernels.rs` uses.
use std::path::PathBuf;
use std::process::Command;

fn crate_file(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(path)
        .canonicalize()
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(crate_file(path)).unwrap()
}

/// The 16 entries of `const NAME: [u8; 16] = [...]` in kernels.rs.
fn rust_table(name: &str) -> Vec<u32> {
    let src = read("src/simd/kernels.rs");
    let start = src.find(&format!("const {name}: [u8; 16] = [")).unwrap();
    let body = &src[start..];
    let body = &body[body.find("= [").unwrap() + 3..body.find("];").unwrap()];
    let mut out = Vec::new();
    let mut rest = body.trim_start();
    while !rest.is_empty() {
        let (value, tail) = if let Some(lit) = rest.strip_prefix("b'") {
            // A byte literal: one character or a two-character escape.
            let len = if lit.starts_with('\\') { 2 } else { 1 };
            let value = match &lit[..len] {
                "\\t" => 9,
                "\\n" => 10,
                "\\r" => 13,
                c => u32::from(c.as_bytes()[0]),
            };
            (value, &lit[len + 1..])
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            (rest[..end].trim().parse().unwrap(), &rest[end..])
        };
        out.push(value);
        rest = tail.trim_start().trim_start_matches(',').trim_start();
    }
    out
}

/// The values of `def name (n : BitVec 8)` in the Lean file, in order.
fn lean_table(name: &str) -> Vec<u32> {
    let src = read("lean/SimdJsonQuotes.lean");
    let start = src.find(&format!("def {name} (n : BitVec 8)")).unwrap();
    let body = &src[start..];
    let body = &body[..body.find("\n\n").unwrap()];
    body.lines()
        .filter_map(|l| {
            let l = l.trim();
            let v = l.rsplit(' ').next()?;
            if l.contains("then") || l.starts_with("else ") {
                v.strip_suffix("#8")?.parse().ok()
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn lean_tables_are_the_kernel_tables() {
    for (rust, lean) in [("WS_TABLE", "wsTable"), ("OP_TABLE", "opTable")] {
        let r = rust_table(rust);
        assert_eq!(r.len(), 16, "{rust}");
        assert_eq!(r, lean_table(lean), "{rust} vs {lean}");
    }
}

#[test]
fn proof_files_have_no_escape_hatches() {
    for file in ["lean/SimdJsonQuotes.lean", "lean/SimdJsonEscape.lean"] {
        let text = read(file);
        for word in [
            "sorry",
            "admit",
            "axiom",
            "native_decide",
            "unsafe",
            "partial",
        ] {
            assert!(
                !text
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|w| w == word),
                "{word} in {file}"
            );
        }
    }
    let quotes = read("lean/SimdJsonQuotes.lean");
    for theorem in [
        "clmul_ones_eq_shifts",
        "in_string_recurrence",
        "in_string_carry_out",
        "whitespace_x86",
        "whitespace_neon",
        "operator_x86",
        "operator_neon",
        "control_x86",
    ] {
        assert!(quotes.contains(&format!("theorem {theorem}")), "{theorem}");
    }
}

#[test]
fn lean_checks_quote_parity_and_classification_proofs() {
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; Lean proof check skipped");
        return;
    };
    let file = crate_file("lean/SimdJsonQuotes.lean");
    let output = Command::new(&lean)
        .arg(&file)
        .current_dir(file.parent().unwrap())
        .output()
        .unwrap_or_else(|e| panic!("running {lean}: {e}"));
    assert!(
        output.status.success(),
        "lean failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
