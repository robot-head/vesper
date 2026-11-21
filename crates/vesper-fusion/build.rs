//! Compile the Loom package in vesper/ (the saturation engine) into a static
//! library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md). Under Bazel the object is prebuilt
//! (VESPER_PORT_OBJECT).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("fusion", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = EDITS
        .iter()
        .fold(port, |port, (from, to)| port.edit(from, to));
    port.build();
}

/// The divergence check (mirrored in BUILD.bazel and asserted by
/// crates/vesper-fusion/port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // engine.ves: explanation steps record the opposite direction.
    (
        "self.emit_step(j, at32(in self.a_l, w(j)) == src);",
        "self.emit_step(j, at32(in self.a_l, w(j)) != src);",
    ),
];
