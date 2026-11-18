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
        let port = vesper_port_harness::Port::new("pact", "vesper").binding("port/binding.rs");
        #[cfg(feature = "vesper-port-divergent")]
        let port = DIVERGENT
            .iter()
            .fold(port, |port, (_, from, to)| port.edit(from, to));
        port.build();
    }
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel
/// and asserted by scripts/test-port-harness.sh), with the test that
/// catches each.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // term.ves: caught by missing_impl_is_no_solution_with_deterministic_explanation.
    (
        "term",
        "            if same {",
        "            if same && tag != 14 {",
    ),
    // fold.ves: caught by merging_duplicate_effect_labels_fails_closed.
    (
        "fold",
        "        if a == label_key(in s, sw(in s.tmp, k)) {",
        "        if a == none() {",
    ),
    // infer.ves: caught by open_row_impl_extends_rows.
    (
        "infer",
        "        if !has_key(in s, other, label_key(in s, l)) {",
        "        if !has_key(in s, other, none()) {",
    ),
    // canon.ves: caught by implicit_brand_uses_lexical_model_scopes.
    (
        "canon",
        "        if nlen(in s, sc) > 0 {",
        "        if nlen(in s, sc) > 1 {",
    ),
    // mode.ves: caught by mode_bounds_follow_the_lattice.
    (
        "mode",
        "            if av <= bv {",
        "            if av < bv {",
    ),
    // render.ves: caught by associated_value_must_satisfy_alias_bounds.
    (
        "render",
        "        r_id(inout s, \"type\", h43);",
        "        r_id(inout s, \"ty\", h43);",
    ),
    // diag.ves: caught by malformed_declarations_are_rejected_with_sorted_diagnostics.
    (
        "diag",
        "            if cmp_diag(in s, dec(j), j) != 2 {",
        "            if cmp_diag(in s, dec(j), j) != 0 {",
    ),
    // program.ves: caught by schema_version_is_enforced.
    (
        "program",
        "    if maj != 0 || min > 1 {",
        "    if maj != 0 || min > 2 {",
    ),
    // solver.ves: caught by step_budget_overflow_is_indeterminate.
    (
        "solver",
        "    s.steps > s.max_steps",
        "    s.steps > wrapping_add(s.max_steps, 1000)",
    ),
    // query.ves: caught by query_dto_from_fixture_solves.
    (
        "query",
        "    if r == 0 {
        n
    } else {
        r
    }",
        "    if r == 0 {
        n
    } else {
        n
    }",
    ),
    // coherence.ves: caught by coherence_violations_are_reported_deterministically.
    (
        "coherence",
        "    if sw(in s.tr_pkg, t) == pkg {",
        "    if sw(in s.tr_pkg, t) != pkg {",
    ),
    // deriv.ves: caught by tampered_derivations_are_rejected.
    (
        "deriv",
        "        if nlen(in s, d) != nlen(in s, sw(in s.cf_where, ci)) {",
        "        if nlen(in s, d) > nlen(in s, sw(in s.cf_where, ci)) {",
    ),
    // out.ves: caught by coinductive_cycle_is_proven_and_marked.
    (
        "out",
        "        let co: u64 = if uses_coinduction(in s, d) {",
        "        let co: u64 = if !uses_coinduction(in s, d) {",
    ),
];
