//! The hybrid Stage 2 build (self-hosting SH0 lane 1, #1123) compiles the root package's CLI sources (src/main.rs and its
//! command modules) unchanged, so it must list every dependency of the root
//! package with the same specification. It must also stay out of the
//! default build.

/// `path` relative to the repository root.
fn read(path: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The `name = spec` lines of a manifest's `[header]` table, with
/// `crates/` paths rewritten relative to the crates directory.
fn table(manifest: &str, header: &str, prefix: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == header;
            continue;
        }
        if inside && !line.is_empty() && !line.starts_with('#') {
            lines.push(line.replace(prefix, "path = \"../"));
        }
    }
    lines
}

#[test]
fn hybrid_lists_every_root_dependency() {
    let root = read("Cargo.toml");
    let hybrid = read("crates/vesper-hybrid/Cargo.toml");
    for header in [
        "[dependencies]",
        "[target.'cfg(unix)'.dependencies]",
        "[target.'cfg(windows)'.dependencies]",
    ] {
        let theirs = table(&hybrid, header, "path = \"../");
        // The root's `[dependencies]` header is also the first table of
        // its `[package]`; the workspace has none of its own.
        for line in table(&root, header, "path = \"crates/") {
            assert!(
                theirs.contains(&line),
                "crates/vesper-hybrid/Cargo.toml {header} lacks the root package's `{line}`; \
                 the hybrid build compiles the root CLI sources and needs it too"
            );
        }
    }
}

#[test]
fn hybrid_is_not_a_default_member() {
    let root = read("Cargo.toml");
    let defaults = root
        .split("default-members = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("default-members");
    assert!(
        !defaults.contains("crates/vesper-hybrid"),
        "the hybrid compiler is a non-default build; keep it out of default-members"
    );
    assert!(root.contains("\"crates/vesper-hybrid\","));
}
