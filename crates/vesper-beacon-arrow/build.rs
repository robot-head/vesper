//! Compile the Loom package in vesper/ (the span stream writer) into a static
//! library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md). Under Bazel the object is prebuilt
//! (VESPER_PORT_OBJECT).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("beacon_arrow", "vesper").binding("port/binding.rs");
    #[cfg(feature = "vesper-port-divergent")]
    let port = EDITS
        .iter()
        .fold(port, |port, (from, to)| port.edit(from, to));
    port.build();
}

/// The divergence check (mirrored in BUILD.bazel and asserted by
/// crates/vesper-beacon-arrow/port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // spans.ves: every u64 column is shifted up by one row.
    (
        "word(in wide, wrapping_add(base, i))",
        "word(in wide, wrapping_add(base, wrapping_add(i, 1)))",
    ),
];
