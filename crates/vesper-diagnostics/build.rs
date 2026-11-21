//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("diagnostics", "vesper")
        .binding("port/binding.rs")
        .rust_types("crate::port");
    // One divergent edit per ported area, one area at a time
    // (`VESPER_DIAGNOSTICS_DIVERGE=prepare`) or all of them;
    // port/test-port.sh asserts that the tests catch each one.
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_DIAGNOSTICS_DIVERGE");
        let only = std::env::var("VESPER_DIAGNOSTICS_DIVERGE").ok();
        EDITS
            .iter()
            .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence checks (asserted by port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // The prepare checks: the source's line count is not charged.
    (
        "prepare",
        "    bound(line_count(in t), caps.max_lines, Res::Lines);",
        "    bound(1, caps.max_lines, Res::Lines);",
    ),
    // The LSP JSON: a 4-byte char counts one UTF-16 unit.
    (
        "lsp",
        "            units = if b >= 240 {",
        "            units = if b >= 248 {",
    ),
    // The terminal report: a tab is drawn three columns wide at most.
    (
        "terminal",
        "        wrapping_sub(wrapping_mul(wrapping_add(col / TAB_WIDTH, 1), TAB_WIDTH), col)",
        "        wrapping_sub(wrapping_mul(wrapping_add(col / 3, 1), 3), col)",
    ),
];
