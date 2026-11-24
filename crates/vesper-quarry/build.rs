//! Compile the Loom package in vesper/ into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("quarry", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = DIVERGENT
        .iter()
        .fold(port, |port, (from, to)| port.edit(from, to));
    port.build();
}

/// One deliberately wrong edit per ported Rust module (BUILD.bazel builds
/// each on its own); the port test run must fail on each. The edits apply to
/// whichever file of the package holds them.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str)] = &[
    // parse.rs, lexer (vesper/src/lex.ves): `<=` lexes as `<`;
    // planner_spec front_end_transcript_matches_golden fails.
    ("op = 3;", "op = 2;"),
    // lib.rs, catalog DDL (vesper/src/ddl.ves): a duplicate table is not
    // ErrorKind::Duplicate; sql
    // ddl_atomic_deterministic_and_stale_revision_or_content_rejected fails.
    (
        "fail_name(inout q, 3, \"table \"",
        "fail_name(inout q, 2, \"table \"",
    ),
    // plan.rs, checker and optimizer (vesper/src/plan.ves): LEFT JOIN no
    // longer makes the right side nullable, and predicates cross the
    // nullable side of a FULL join; sql
    // outer_join_null_extension_and_filter_placement and
    // outer_join_pushdown_differential_for_many_predicates_and_null_inputs
    // fail.
    ("if b.kind == 1 || b.kind == 3 {", "if b.kind == 3 {"),
    (
        "if (x.a == 0 || x.a == 1) && deps_within(in q, p, x.b) {",
        "if (x.a == 0 || x.a == 1 || x.a == 3) && deps_within(in q, p, x.b) {",
    ),
    // execute.rs, row executor (vesper/src/values.ves): three-valued AND is
    // wrong when one side is false; sql sql_null_three_valued_logic_exhaustive
    // fails.
    ("if a == 1 || b == 1 {", "if a == 1 && b == 1 {"),
    // The access planner (vesper/src/stats.ves): `v > c` is not flipped to
    // `c < v`; planner_spec planner_chooses_the_golden_access_paths fails.
    (
        "                    } else if op == 4 {\n                        2",
        "                    } else if op == 4 {\n                        4",
    ),
    // page.rs (vesper/src/page.ves): unsupported header check disabled;
    // pages corruption_and_wrong_sizes_are_rejected_before_trusting_offsets fails.
    ("if kind != 1 || level != 0 || flags != 0 {", "if false {"),
];
