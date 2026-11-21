//! Under `vesper-port`, compile the Vesper port in port/ into a static
//! library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("sim", "port")
            .binding("port/binding.rs")
            .rust_types("crate::model::link::port");
        #[cfg(feature = "vesper-port-divergent")]
        let port = DIVERGENT_EDITS
            .iter()
            .fold(port, |port, (from, to)| port.edit(from, to));
        port.build();
    }
}

/// One deliberate bug per ported model (mirrored in
/// port/test-port.sh's expectations). Each must make its tests fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str)] = &[
    // link: a datagram that finds one free buffer slot is tail-dropped.
    (
        "    if link_state_queued_at(in l, at_ns) >= p.buffer_packets {",
        "    if wrapping_add(link_state_queued_at(in l, at_ns), 1) >= p.buffer_packets {",
    ),
];
