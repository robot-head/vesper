//! Under `vesper-port`, compile the Vesper port into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
//!
//! The port is one library written as several files (`PARTS`), which this
//! script joins into `$OUT_DIR/umem.ves` in order (BUILD.bazel's
//! `:umem_source` does the same).

/// The files of the port, in order.
#[cfg(feature = "vesper-port")]
const PARTS: &[&str] = &["types", "util", "ring", "config", "soft", "driver", "audit"];

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
        let joined = out.join("umem.ves");
        std::fs::write(&joined, source).expect("write the joined port");
        let port = vesper_port_harness::Port::new("umem", &joined)
            .binding("port/binding.rs")
            .rust_types("crate");
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
    // ring: a reservation may take one slot too many.
    (
        "        if n == 0 || self.free_entries(n) < n {",
        "        if n == 0 || wrapping_add(self.free_entries(n), 1) < n {",
    ),
    // soft: loopback no longer delivers.
    (
        "            if valid && n.mode == SoftNicMode::Loopback {",
        "            if valid && n.mode == SoftNicMode::Scripted {",
    ),
    // driver: a zero-length rx descriptor is accepted.
    (
        "        if d.len == 0 {",
        "        if d.len == 4294967295 {",
    ),
];
