//! Link the Vesper port of vesper-loom-cas (the Loom package in ../vesper)
//! and set cfg(vesper_port), so ../src forwards to it (see
//! docs/maintenance/porting-a-crate.md and Cargo.toml).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let port = vesper_port_harness::Port::new("loom_cas", "../vesper").binding("binding.rs");
    // The divergent build applies one module's edit, picked by
    // VESPER_CAS_DIVERGE (test-port.sh runs each).
    #[cfg(feature = "divergent")]
    let port = {
        println!("cargo:rerun-if-env-changed=VESPER_CAS_DIVERGE");
        let module = std::env::var("VESPER_CAS_DIVERGE")
            .expect("the divergent feature needs VESPER_CAS_DIVERGE=<module>");
        let (_, from, to) = DIVERGENT_EDITS
            .iter()
            .find(|(m, _, _)| *m == module)
            .unwrap_or_else(|| panic!("no divergent edit for module {module}"));
        port.edit(from, to)
    };
    port.build();
}

/// One deliberate bug per ported module: (module, text, wrong text).
#[cfg(feature = "divergent")]
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
    (
        "drv",
        "            false,\n            F_VALUE,",
        "            true,\n            F_KEY,",
    ),
    // store.ves: an occupied object path is trusted without rehashing it.
    (
        "store",
        "if seen == size && same(in actual.as_slice(), in id) {",
        "if seen == size {",
    ),
    // fold.ves: a word-final capital sigma folds to the medial form.
    ("fold", "encode(inout out, 962);", "encode(inout out, 963);"),
    // store.ves: ingesting a directory skips the case-collision check.
    ("walk", "if prev != 0 {", "if prev == 0 && prev != 0 {"),
    // store.ves: an import accepts records in any closure order.
    ("pack", "    if index != next {", "    if index != next && index == next {"),
];
