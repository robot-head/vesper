//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("loom_calculus", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = DIVERGENT
        .iter()
        .fold(port, |port, (_, from, to)| port.edit(from, to));
    port.build();
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel
/// and asserted by scripts/test-port-harness.sh), with the test that
/// catches each.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // check.ves: a `:` no longer rejects a build input path
    // (differential manifest_corpus_matches_the_rust_build).
    (
        "check",
        "if c == 92 || c == 58 || c == 0 {",
        "if c == 92 || c == 0 {",
    ),
    // semver.ves: `^1.2` stops matching 1.2.x
    // (differential semver_matrix_matches_the_rust_build).
    (
        "semver",
        "return v.minor >= c.minor;",
        "return v.minor > c.minor;",
    ),
    // solve.ves: authority makes one backwards pass only (calculus
    // authority_reaches_ancestors_that_sort_before_their_dependencies).
    (
        "solve",
        "if grew && !sbool(in l.queued, parent) {",
        "if false && grew {",
    ),
    // upgrade.ves: relaxing forbid_unsafe_ffi is allowed (upgrade
    // unsafe_ffi_policy_relaxation_is_rejected_for_existing_and_new_packages).
    (
        "upgrade",
        "let relaxed = !new_forbid;",
        "let relaxed = false;",
    ),
];
