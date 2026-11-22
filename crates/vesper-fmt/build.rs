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
        let port = vesper_port_harness::Port::new("fmt", "vesper")
            .binding("port/binding.rs")
            .rust_types("crate::port");
        // One divergent edit per ported area (the document renderer, the
        // lexer gate, the layout), the same edits as BUILD.bazel;
        // port/test-port.sh asserts that the tests catch each one.
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_FMT_DIVERGE");
            let only = std::env::var("VESPER_FMT_DIVERGE").ok();
            EDITS
                .iter()
                .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

/// The divergence checks (mirrored in BUILD.bazel and asserted by
/// port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // The renderer: a 99-column ribbon.
    (
        "doc",
        "const LINE_WIDTH: u64 = 100;",
        "const LINE_WIDTH: u64 = 99;",
    ),
    // The lexer gate: the expression height ceiling drops to 16.
    (
        "gate",
        "const MAX_CHAIN: u64 = 160;",
        "const MAX_CHAIN: u64 = 16;",
    ),
    // The layout: a source trailing comma no longer keeps a list vertical.
    (
        "layout",
        "let trailing_comma = !is_block",
        "let trailing_comma = false",
    ),
];
