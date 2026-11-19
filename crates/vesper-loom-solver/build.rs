//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script: it builds its frozen Rust engine.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("loom_solver", "vesper").binding("port/binding.rs");
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
    // input.ves: a catalog of schema 2 is accepted (resolve schema_unknown_fields_and_root_collisions_are_errors).
    ("input", "if s.cschema != 1 {", "if s.cschema == 0 {"),
    // closure.ves: late features never revisit a processed package (resolve late_feature_requests_revisit_nodes_through_cycles).
    (
        "closure",
        "if sw(in s.pcnt, d.name) == none() || grew {",
        "if sw(in s.pcnt, d.name) == none() {",
    ),
    // search.ves: the budget allows one decision too many (locked valid_older_selection_is_preserved_without_a_search_budget).
    ("search", "if s.sdec == s.maxd {", "if s.sdec > s.maxd {"),
    // snap.ves: partial validation rejects missing native selections (resolve native_constraints_unify_and_pick_latest_satisfying_version).
    (
        "snap",
        "if d.code != 10 && d.code != 11 {",
        "if d.code != 10 {",
    ),
    // locked.ves: snapshot metadata is never compared (locked manifest_metadata_and_build_metadata_must_match).
    (
        "locked",
        "if !ex.has_json || ex.json != json {",
        "if !ex.has_json {",
    ),
    // metrics.ves: CVE counts are dropped (optimize distinguishes_missing_candidate_metadata_from_zero_cost).
    (
        "metrics",
        "cves = sat(cves, m.cve);",
        "cves = sat(cves, 0);",
    ),
    // diag.ves: messages print the prerelease instead of the version (differential resolve_corpus_matches_the_rust_build).
    (
        "diag",
        "let t = ver(in s.l, vi).text;",
        "let t = ver(in s.l, vi).pre;",
    ),
    // output.ves: errors report zero decisions (resolve distinguishes_exhaustive_failure_from_search_budget).
    ("output", "    w(inout s, decisions);", "    w(inout s, 0);"),
];
