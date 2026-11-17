//! Under `vesper-port`, compile the Vesper port into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
//!
//! The port is one library written as several files (`PARTS`), which this
//! script joins into `$OUT_DIR/wheel.ves` in order (BUILD.bazel's
//! `:wheel_source` does the same). It is not a Loom package: the types that
//! cross the boundary must be declared in the root module to keep their
//! names, and other modules cannot import the root module. It is not one
//! file either: `vesper fmt` refuses sources over 8192 tokens.

/// The files of the port, in order.
#[cfg(feature = "vesper-port")]
const PARTS: &[&str] = &[
    "types",
    "util",
    "wheel",
    "wheel_lists",
    "wheel_api",
    "leases",
    "edt",
    "clock",
];

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
        let joined = out.join("wheel.ves");
        std::fs::write(&joined, source).expect("write the joined port");
        let port = vesper_port_harness::Port::new("wheel", &joined)
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
    // wheel: an entry fires 1 ns early.
    (
        "        if deadline > self.now_ns {",
        "        if deadline > wrapping_add(self.now_ns, 1) {",
    ),
    // leases: packet number 2^62 is accepted.
    (
        "        if pn >= max_packet_number() {",
        "        if pn > max_packet_number() {",
    ),
    // edt: an EDT exactly at the horizon is refused.
    (
        "    if edt_ns > saturating_add(now_ns, q.horizon_ns) {",
        "    if edt_ns >= saturating_add(now_ns, q.horizon_ns) {",
    ),
    // clock: moving back by 1 ns is accepted.
    (
        "    if requested_ns < now_ns {",
        "    if requested_ns < wrapping_sub(now_ns, 1) {",
    ),
];
