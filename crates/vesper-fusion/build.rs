//! Compile the Loom package in vesper/ (the saturation engine and independent
//! witness checker) into a static library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md). Under Bazel the object is prebuilt
//! (VESPER_PORT_OBJECT).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("fusion", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_FUSION_DIVERGE");
        let only = std::env::var("VESPER_FUSION_DIVERGE").ok();
        EDITS
            .iter()
            .filter(|(area, _, _)| only.as_deref().is_none_or(|want| want == *area))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence checks (mirrored in BUILD.bazel and asserted by
/// aspect crate fusion test-port).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // engine.ves: explanation steps record the opposite direction.
    (
        "engine",
        "self.emit_step(j, at32(in self.a_l, w(j)) == src);",
        "self.emit_step(j, at32(in self.a_l, w(j)) != src);",
    ),
    // witness.ves: checker skips the final goal-equality check.
    (
        "witness",
        "    if cur != goal {",
        "    if false && cur != goal {",
    ),
];
