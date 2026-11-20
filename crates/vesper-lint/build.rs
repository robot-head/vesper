//! Under `vesper-port`, link the Vesper port in vesper/ and set
//! cfg(vesper_port); see "Crates the compiler links" in
//! docs/maintenance/porting-a-crate.md. Without it this package builds the
//! Rust code, like the seed package (seed/), which has no build script.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The harness sets the cfg under `vesper-port`; declare it either way.
    println!("cargo:rustc-check-cfg=cfg({})", vesper_port_harness::CFG);
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("lint", "vesper")
            .binding("port/binding.rs")
            .rust_types("crate::port");
        // One divergent edit per ported area (the effect rule, the
        // projection rule, the TOML reader, the source edits), the same
        // edits as BUILD.bazel; port/test-port.sh asserts that the tests
        // catch each one.
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_LINT_DIVERGE");
            let only = std::env::var("VESPER_LINT_DIVERGE").ok();
            EDITS
                .iter()
                .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

/// The divergence checks (mirrored in BUILD.bazel and asserted by
/// port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // The effect rule: a label on the request no longer narrows the match.
    (
        "effects",
        "(q.label == NONE || e.label == q.label)",
        "(q.label == NONE || e.label != NONE)",
    ),
    // The projection rule: reads under a written path are kept.
    (
        "projections",
        "if starts_with(\n                    in p,\n                    act_path(in p, f, r),",
        "if path_eq(\n                    in p,\n                    act_path(in p, f, r),",
    ),
    // The TOML reader: a header may name an existing implicit dotted table.
    (
        "toml",
        "if x.kind != N_TABLE || !x.implicit || x.dotted {",
        "if x.kind != N_TABLE || !x.implicit {",
    ),
    // The source edits: an oversized result reports a stale edit.
    ("edits", "fail(E_CANDIDATE);", "fail(E_STALE);"),
];
