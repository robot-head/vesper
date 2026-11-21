//! Compile the Vesper library `flux` (the Loom package in vesper/) into a
//! static library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    {
        let port = vesper_port_harness::Port::new("flux", "vesper").binding("port/binding.rs");
        // The divergence check: one deliberate change per ported kernel,
        // which the tests must catch (CI asserts that they fail). One at a
        // time with `VESPER_FLUX_DIVERGE=<kernel>`, or all.
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_FLUX_DIVERGE");
            let only = std::env::var("VESPER_FLUX_DIVERGE").ok();
            DIVERGENT
                .iter()
                .filter(|(kernel, _, _)| only.as_deref().is_none_or(|k| k == *kernel))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

/// The divergence edits (mirrored in BUILD.bazel): each kernel's name, the
/// text it replaces and the wrong text (in whichever package file holds it).
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // union: the second set's weights are dropped.
    (
        "union",
        "            acc = acc_add(acc, cell(in b, wrapping_add(bj, width)));\n",
        "            acc = acc_add(acc, 0);\n",
    ),
    // filter: `AtLeast` becomes strictly greater.
    (
        "filter",
        "            x >= value\n",
        "            x > value\n",
    ),
    // consolidate: zero sums are kept.
    (
        "consolidate",
        "        if !acc_is_zero(acc) {\n            if !acc_fits(acc) {\n                overflow = true;\n            }\n            push_row(inout out, in cells, fat, width);",
        "        if true {\n            if !acc_fits(acc) {\n                overflow = true;\n            }\n            push_row(inout out, in cells, fat, width);",
    ),
    // join: the join product limit is off by one.
    (
        "join",
        "    if product > max_product {\n",
        "    if product >= max_product {\n",
    ),
    // frontier: an empty table publishes 0 instead of keeping its bound.
    (
        "frontier",
        "    let mut min = watermark;\n",
        "    let mut min: i64 = 0;\n",
    ),
    // accumulator (vesper/src/acc.ves): one row more than `max_rows` fits.
    (
        "accumulator",
        "    if count >= a.max_rows {\n",
        "    if count > a.max_rows {\n",
    ),
    // cdc (vesper/src/cdc.ves): flags 1 is accepted.
    (
        "cdc",
        "        if flags != 0 {\n",
        "        if flags > 1 {\n",
    ),
    // log (vesper/src/log.ves): the header says version 2.
    (
        "log",
        "    put_u16(inout h, 1);\n",
        "    put_u16(inout h, 2);\n",
    ),
];
