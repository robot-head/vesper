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
        let port = vesper_port_harness::Port::new("beacon", "vesper")
            .binding("port/binding.rs")
            .rust_types("crate::port");
        #[cfg(feature = "vesper-port-divergent")]
        let port = EDITS
            .iter()
            .fold(port, |port, (from, to)| port.edit(from, to));
        port.build();
    }
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel
/// and asserted by scripts/test-port-harness.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // histogram.rs: the last bin of each octave lands in the next octave.
    (
        "            + ((value >> (octave - SUB_BITS)) - OCTAVE_BINS)",
        "            + ((value >> (octave - SUB_BITS)) - (OCTAVE_BINS - 1))",
    ),
    // exponential.rs: the window keeps only half the buckets it may.
    (
        "if (hi >> shift) - (lo >> shift) + 1 <= EXP_BUCKETS {",
        "if (hi >> shift) - (lo >> shift) + 1 <= 80 {",
    ),
    // traceparent.rs: uppercase hex letters are accepted.
    (
        "} else if c >= Ascii::LowerA && c <= Ascii::LowerF {",
        "} else if (c >= Ascii::LowerA && c <= Ascii::LowerF) || (c >= 65 && c <= 70) {",
    ),
    // spans.rs: a child may get its imported parent's span ID.
    (
        "            if p.span_id == id {",
        "            if p.span_id == 0 {",
    ),
];
