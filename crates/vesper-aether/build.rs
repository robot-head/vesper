//! Under `vesper-port`, compile the Vesper port into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
//!
//! The port is one library written as several files (`PARTS`), which this
//! script joins into `$OUT_DIR/aether.ves` in order (BUILD.bazel's
//! `:aether_source` does the same).

/// The files of the port, in order.
#[cfg(feature = "vesper-port")]
const PARTS: &[&str] = &["util", "slot_plan", "pvh", "hotset", "packed"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let mut source = String::new();
        for part in PARTS {
            let path = format!("vesper/src/{part}.ves");
            println!("cargo:rerun-if-changed={path}");
            source.push_str(&std::fs::read_to_string(&path).expect("read a port file"));
            source.push('\n');
        }
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let joined = out.join("aether.ves");
        std::fs::write(&joined, source).expect("write the joined port");
        let port = vesper_port_harness::Port::new("aether", &joined)
            .binding("port/binding.rs")
            .rust_types("crate::port_types");
        #[cfg(feature = "vesper-port-divergent")]
        let port = DIVERGENT_EDITS
            .iter()
            .fold(port, |port, (from, to)| port.edit(from, to));
        port.build();
    }
}

/// One deliberate bug per ported module (mirrored in BUILD.bazel). Each
/// must make its module's tests fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str)] = &[
    // slot_plan: touching ranges count as overlapping.
    (
        "            if start < reach_end {",
        "            if start <= reach_end {",
    ),
    // pvh: a segment may end one byte past the file.
    (
        "        if add_overflows(offset, filesz) || wrapping_add(offset, filesz) > n {",
        "        if add_overflows(offset, filesz) || wrapping_add(offset, filesz) > wrapping_add(n, 1) {",
    ),
    // hotset: equal indices count as increasing.
    (
        "        if have && last >= i {",
        "        if have && last > i {",
    ),
    // packed: a used descriptor counts as available.
    ("        if !avail || used {", "        if !avail {"),
];
