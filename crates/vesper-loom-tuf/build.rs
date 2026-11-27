//! Compile the Loom package in vesper/ into a static library and check the
//! committed binding (see docs/maintenance/porting-a-crate.md). Under Bazel
//! the object is prebuilt (VESPER_PORT_OBJECT).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("loom_tuf", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_LOOM_TUF_DIVERGE");
        let only = std::env::var("VESPER_LOOM_TUF_DIVERGE").ok();
        EDITS
            .iter()
            .filter(|(area, _, _)| only.as_deref().is_none_or(|want| want == *area))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence checks (mirrored in BUILD.bazel and asserted by
/// aspect crate loom-tuf test-port).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // Skip JSON depth/node limit enforcement.
    (
        "json",
        "            if depth > max_depth || nodes > max_nodes {",
        "            if false && (depth > max_depth || nodes > max_nodes) {",
    ),
    // Skip cycle detection in delegation graph traversal.
    (
        "graph",
        "                    if at_u8(in visit, child) == VISIT_ON_PATH {",
        "                    if false && at_u8(in visit, child) == VISIT_ON_PATH {",
    ),
    // Off-by-one in Ed25519 quorum threshold check.
    (
        "quorum",
        "                    if valid_count >= threshold {",
        "                    if valid_count > threshold {",
    ),
];
