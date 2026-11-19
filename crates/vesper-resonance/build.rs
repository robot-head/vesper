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
        let port = vesper_port_harness::Port::new("resonance", "vesper").binding("port/binding.rs");
        #[cfg(feature = "vesper-port-divergent")]
        let port = EDITS
            .iter()
            .fold(port, |port, (from, to)| port.edit(from, to));
        port.build();
    }
}

/// The divergence checks, one per ported module (mirrored in BUILD.bazel
/// and asserted by port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // kernels.ves: the right channel mixes with the left pan weights.
    ("r = r + y * f(in pan_r, v);", "r = r + y * f(in pan_l, v);"),
    // events.ves: decreasing offsets are accepted after the first event.
    ("if offset < prev {", "if offset < prev && index == 0 {"),
    // pdc.ves: a node's arrival is its earliest forward input, not its latest.
    ("if l > a {", "if l < a || a == 0 {"),
    // slab.ves: every delay line's head returns to 0 when it wraps.
    ("slots.set(m + 4, next % cap);", "slots.set(m + 4, 0);"),
    // lease.ves: frames may be written out of order.
    ("if frame != written {", "if frame > frames {"),
    // confine.ves: allowed effects are reported instead of forbidden ones.
    (
        "if !earlier && !is_allowed(in allowed, code) {",
        "if !earlier && is_allowed(in allowed, code) {",
    ),
    // graph.ves: the Vesper graph's gain nodes ignore their gain.
    (
        "let v = from_bits_f32(out[k]) * self.gain;",
        "let v = from_bits_f32(out[k]);",
    ),
];
