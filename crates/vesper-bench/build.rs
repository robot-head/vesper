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
        let port = vesper_port_harness::Port::new("bench", "vesper")
            .binding("port/binding.rs")
            .rust_types("crate::port");
        // The divergent build applies the edit of one module, picked by
        // VESPER_BENCH_DIVERGE (port/check.sh runs each).
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_BENCH_DIVERGE");
            let module = std::env::var("VESPER_BENCH_DIVERGE")
                .expect("vesper-port-divergent needs VESPER_BENCH_DIVERGE=<module>");
            let (_, from, to) = EDITS
                .iter()
                .find(|(name, _, _)| *name == module)
                .unwrap_or_else(|| panic!("no divergent edit for module {module}"));
            port.edit(from, to)
        };
        port.build();
    }
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel
/// and asserted by scripts/test-port-harness.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // arithmetic.rs: the full-width residue runs backwards.
    (
        "arithmetic",
        "return wrapping_sub(end, start);",
        "return wrapping_sub(start, end);",
    ),
    // paired_difference.rs: a fractional difference reports out of range.
    (
        "paired_difference",
        "return (Exactness::Fractional, 0);",
        "return (Exactness::OutOfRange, 0);",
    ),
    // walsh.rs: odd and even medians swap.
    (
        "walsh",
        "if rem(count, 2) == 0 {",
        "if rem(count, 2) == 1 {",
    ),
    // signed_rank.rs: each tied group counts twice.
    (
        "signed_rank",
        "tied_groups = inc(tied_groups);",
        "tied_groups = add(tied_groups, 2);",
    ),
    // signed_rank_confidence.rs: the inversion tests at 10%, not 5%.
    (
        "signed_rank_confidence",
        "mul(TAIL_LIMIT_SCALE, tail) > pow2(n)",
        "mul(20, tail) > pow2(n)",
    ),
    // hodges_lehmann.rs: the interval's critical rank is the 10% one.
    (
        "hodges_lehmann",
        "if mul(TAIL_LIMIT_SCALE, tail) <= total {",
        "if mul(20, tail) <= total {",
    ),
    // statistics.rs: the median interval's binomial tail is 2.5%, not 5%.
    (
        "statistics",
        "w_mul_small(tail, ALPHA_TAIL_DENOMINATOR)",
        "w_mul_small(tail, 100)",
    ),
    // statistics.rs (Mann-Whitney): each tie counts twice.
    ("mann_whitney", "tied = inc(tied);", "tied = add(tied, 2);"),
    // contract.rs: the stabilizer field is misspelled.
    ("contract", "\"stabilizer_pages\"", "\"stabilizer_page\""),
    // contract_gate.rs: a count exactly at the limit fails.
    (
        "contract_gate",
        "if w_ult(right, left) {",
        "if !w_ult(left, right) {",
    ),
    // wire.rs: 10001 basis points are accepted.
    ("wire", "if bp > BP_SCALE {", "if bp > 10001 {"),
    // layout.rs: heap offsets skip the last step.
    (
        "layout",
        "mul(rem(heap, HEAP_OFFSET_STEPS), HEAP_OFFSET_STEP)",
        "mul(rem(heap, 255), HEAP_OFFSET_STEP)",
    ),
    // execution.rs: an epoch's sample counts three windows of operations.
    (
        "execution",
        "do_paired_sample_new(a, b, cmul(operations, WINDOWS_PER_ARM))",
        "do_paired_sample_new(a, b, cmul(operations, 3))",
    ),
    // causal.rs: every predicted speedup is 100% too high.
    (
        "causal",
        "let mut result = w_sub(w_u64(BP_SCALE), q);",
        "let mut result = w_sub(w_u64(20000), q);",
    ),
];
