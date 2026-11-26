//! With the `vesper-port` feature, compile the Vesper port (`vesper/`,
//! #1969) into a static library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("simd", "vesper").binding("port/binding.rs");
        #[cfg(feature = "vesper-port-divergent")]
        let port = EDITS
            .iter()
            .fold(port, |port, (from, to)| port.edit(from, to));
        port.build();
    }
}

/// One deliberate bug (mirrored in BUILD.bazel): it must make the crate's
/// tests fail.
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str)] = &[
    // Each delimiter in a full chunk counts three times.
    (
        "let bit = ins_band_imm(inout b, bit, 1);",
        "let bit = ins_band_imm(inout b, bit, 3);",
    ),
];
