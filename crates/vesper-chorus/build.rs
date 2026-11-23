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
    ("util", "            if a < b {", "            if a > b {"),
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
    // simnet.ves: packet duplication is disabled (caught by
    // two_buyer_runs_under_simpulse_with_reordering_and_duplication).
    (
        "simnet",
        "    let copies: u64 = if sm_chance(inout net.rng, dup_pm) {",
        "    let copies: u64 = if false && sm_chance(inout net.rng, dup_pm) {",
    ),
    // session.ves: dropped sessions are counted twice on leak
    // (caught by dropping_a_session_before_end_is_detected_and_unblocks_the_peer).
    (
        "session",
        "    a.leaked = wrapping_add(a.leaked, 1);",
        "    a.leaked = wrapping_add(a.leaked, 2);",
    ),
    // wire.ves: majority and half-floor quorum rules are swapped
    // (caught by quorum_sizes).
    (
        "wire",
        "    if rule == QuorumKind::Majority {",
        "    if rule != QuorumKind::Majority {",
    ),
    // raft.ves: commits require more votes than nodes in the cluster
    // (caught by five_node_cluster_keeps_election_safety_and_log_matching).
    (
        "raft",
        "            if count >= q {",
        "            if count > rc.n {",
    ),
    // replog.ves: elections require strictly more votes than nodes
    // (caught by replicated_log_converges_over_lossy_partitioned_warp_links).
    (
        "replog",
        "    if votes > rp.n / 2 {",
        "    if votes > rp.n {",
    ),
];
