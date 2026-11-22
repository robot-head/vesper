//! Under `vesper-port`, link the Vesper port of the codecs in vesper/ and set
//! cfg(vesper_port) (see docs/maintenance/porting-a-crate.md). Without it
//! the crate builds the Rust code and resolves no port harness, so the
//! compiler's closure stays free of port-backed packages.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(vesper_port)");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("loom_cas", "vesper").binding("port/binding.rs");
        // The divergent build applies one module's edit, picked by
        // VESPER_CAS_DIVERGE (port/test-port.sh runs each).
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_CAS_DIVERGE");
            let module = std::env::var("VESPER_CAS_DIVERGE")
                .expect("vesper-port-divergent needs VESPER_CAS_DIVERGE=<module>");
            let (_, from, to) = DIVERGENT_EDITS
                .iter()
                .find(|(m, _, _)| *m == module)
                .unwrap_or_else(|| panic!("no divergent edit for module {module}"));
            port.edit(from, to)
        };
        port.build();
    }
}

/// One deliberate bug per ported module: (module, text, wrong text).
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str, &str)] = &[
    // name.ves: a backslash is no longer a separator.
    ("name", "if b == 47 || b == 92 {", "if b == 47 {"),
    // id.ves: the kind name "tree" is accepted as a blob.
    ("id", "        kind = 2;\n", "        kind = 1;\n"),
    // tree.ves: directories may have a size.
    (
        "tree",
        "} else if tag == 3 && size != 0 {",
        "} else if tag == 4 && size != 0 {",
    ),
    // drv.ves: an option value may hold control characters.
    ("drv", "false, F_VALUE,", "true, F_KEY,"),
];
