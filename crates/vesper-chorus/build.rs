//! Compile the Vesper library `chorus` (the Loom package in vesper/) into a
//! static library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md). Under Bazel the object is prebuilt
//! (VESPER_PORT_OBJECT).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("chorus", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        // One module at a time (`VESPER_CHORUS_DIVERGE=parse`), or all.
        println!("cargo:rerun-if-env-changed=VESPER_CHORUS_DIVERGE");
        let only = std::env::var("VESPER_CHORUS_DIVERGE").ok();
        DIVERGENT
            .iter()
            .filter(|(module, _, _)| only.as_deref().is_none_or(|m| m == *module))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence checks, one wrong line per ported module (mirrored in
/// BUILD.bazel and asserted by port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT: &[(&str, &str, &str)] = &[
    // util.ves: symbols order in reverse (caught by
    // informed_third_party_merges_into_an_offer).
    // tree.ves: unused roles are never reported (well_formedness_errors_are_typed).
    // local.ves: one-branch choices stay Select/Offer
    // (two_buyer_projects_to_the_textbook_local_types).
    // project.ves: the absent-role loop rule is inverted
    // (role_outside_a_loop_projects_through_it).
    // explore.ves: the queue bound is never reported (recursion_projects_and_runs).
    // parse.ves: brace nesting is unbounded (hostile_inputs_are_bounded).
    (
        "util",
        "                return Order::Less;\n            }\n            return Order::Greater;",
        "                return Order::Greater;\n            }\n            return Order::Less;",
    ),
    ("tree", "    if nr > 1 {", "    if nr > 99 {"),
    ("local", "    if n == 1 {", "    if n == 0 {"),
    (
        "project",
        "        if !involves(in c, body, c.target) {",
        "        if involves(in c, body, c.target) {",
    ),
    (
        "explore",
        "            bound_hit = true;",
        "            bound_hit = false;",
    ),
    (
        "parse",
        "    if depth > Limit::Nesting {",
        "    if depth > 100000 {",
    ),
];
