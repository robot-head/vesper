//! Under `vesper-port`, compile the Vesper port in port/ into a static
//! library and check the committed binding (see
//! docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("sim", "port")
            .binding("port/binding.rs")
            .rust_types("crate::port");
        // The divergent build applies the edit of one module, picked by
        // VESPER_SIM_DIVERGE (port/test-port.sh runs each).
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_SIM_DIVERGE");
            let module = std::env::var("VESPER_SIM_DIVERGE")
                .expect("vesper-port-divergent needs VESPER_SIM_DIVERGE=<module>");
            let (_, from, to) = DIVERGENT_EDITS
                .iter()
                .find(|(name, _, _)| *name == module)
                .unwrap_or_else(|| panic!("no divergent edit for module {module}"));
            port.edit(from, to)
        };
        port.build();
    }
}

/// One deliberate bug per ported module, `(module, from, to)` (asserted by
/// port/test-port.sh). Each must make its tests fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str, &str)] = &[
    // link: a datagram that finds one free buffer slot is tail-dropped.
    (
        "link",
        "    if link_state_queued_at(in l, at_ns) >= p.buffer_packets {",
        "    if wrapping_add(link_state_queued_at(in l, at_ns), 1) >= p.buffer_packets {",
    ),
    // wal: UTF-8 surrogates (ED A0..BF) are accepted.
    (
        "wal",
        "        } else if b0 == 237 {\n            (128, 159)",
        "        } else if b0 == 237 {\n            (128, 191)",
    ),
    // net: an endpoint's inbox takes one message past its bound.
    (
        "net",
        "    if wrapping_add(reserved, at(in n.queued, e)) >= n.max_inbox {",
        "    if wrapping_add(reserved, at(in n.queued, e)) > n.max_inbox {",
    ),
];
