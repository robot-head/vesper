//! Link the Vesper port in vesper/ and set cfg(vesper_port); see "Crates
//! the compiler links" in docs/maintenance/porting-a-crate.md. The seed
//! package (seed/) has no build script, so it builds the Rust code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("lean_syntax", "vesper")
        .binding("port/binding.rs")
        .rust_types("crate::port");
    // One divergent edit per ported area (the printer, the checks, the
    // reference walks), the same edits as BUILD.bazel; port/test-port.sh
    // asserts that the tests catch each one.
    #[cfg(feature = "vesper-port-divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_LEAN_DIVERGE");
        let only = std::env::var("VESPER_LEAN_DIVERGE").ok();
        EDITS
            .iter()
            .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
            .fold(port, |port, (_, from, to)| port.edit(from, to))
    };
    port.build();
}

/// The divergence checks (mirrored in BUILD.bazel and asserted by
/// port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // The printer: a newline escapes as a tab.
    (
        "printer",
        "p.out.push_str(\"\\\\n\");",
        "p.out.push_str(\"\\\\t\");",
    ),
    // The checks: `_root_` is accepted as an identifier.
    ("checks", "if n == 0 || v == \"_root_\" {", "if n == 0 {"),
    // The walks: a do `let` binds its own value.
    (
        "walks",
        "            truncate_scope(inout r, depth);\n            let typed = take(in s, inout at);",
        "            let typed = take(in s, inout at);",
    ),
];
