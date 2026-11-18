//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("loom_audit", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = DIVERGENT
        .iter()
        .fold(port, |port, (_, from, to)| port.edit(from, to));
    port.build();
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel
/// and asserted by port/test-port.sh), with the test that catches each.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // input.ves: the path depth budget reads one too high (audit graph_catalog_path_and_work_budgets_fail_closed).
    (
        "input",
        "s.m_pdepth = rd(inout s.l);",
        "s.m_pdepth = wrapping_add(rd(inout s.l), 1);",
    ),
    // license.ves: AND evaluates like OR (audit spdx_boolean_precedence_and_explicit_exception_grants).
    (
        "license",
        "ok: left.ok && right.ok,",
        "ok: left.ok || right.ok,",
    ),
    // check.ves: overlapping intervals pass (audit malformed_or_ambiguous_catalogs_never_produce_clean_results).
    (
        "check",
        "if !p.has_fixed || prec_ord(in s.l, p.fixed, r.intro) == 2 {",
        "if !p.has_fixed {",
    ),
    // paths.ves: a longer native path replaces a shorter one (audit native_shared_dependency_chooses_shortest_path_per_root).
    (
        "paths",
        "if shorter(in s, nat_at, n, p) {",
        "if !shorter(in s, nat_at, n, p) {",
    ),
    // report.ves: the severity threshold is ignored (audit severity_threshold_and_mixed_unknown_violations_are_explicit).
    (
        "report",
        "if affected && x.sev >= s.minsev {",
        "if affected {",
    ),
    // util.ves: precedence ignores prereleases (audit semver_intervals_include_lower_exclude_fixed_and_ignore_build_metadata).
    ("util", "    pre_ord(in l, x.pre, y.pre)\n}", "    1\n}"),
];
