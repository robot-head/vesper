//! Under `vesper-port`, compile the Vesper port into a static library and
//! check the committed binding (see docs/maintenance/porting-a-crate.md).
//!
//! The port is one library written as several files (`PARTS`), which this
//! script joins into `$OUT_DIR/shard.ves` in order (BUILD.bazel's
//! `:shard_source` does the same): the types that cross the boundary must be
//! declared in the root module to keep their names, and `vesper fmt`
//! refuses sources over 8192 tokens.

/// The files of the port, in order.
#[cfg(feature = "vesper-port")]
const PARTS: &[&str] = &[
    "types",
    "util",
    "kernels",
    "table",
    "table_api",
    "batch",
    "rss",
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
        let joined = out.join("shard.ves");
        std::fs::write(&joined, source).expect("write the joined port");
        let port = vesper_port_harness::Port::new("shard", &joined)
            .binding("port/binding.rs")
            .rust_types("crate::port::types");
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
    // kernels: the scalar equality mask drops lane 15.
    (
        "                if widen_u64(v) == key {",
        "                if widen_u64(v) == key && i < 15 {",
    ),
    // table: a next packet number of 2^62 + 1 is accepted.
    (
        "    if s.next_tx_pn > PN_LIMIT {",
        "    if s.next_tx_pn > wrapping_add(PN_LIMIT, 1) {",
    ),
    // batch: a cwnd update is ignored.
    (
        "                if cwnd_set & bit(l) != 0 {",
        "                if cwnd_set & bit(l) == 65536 {",
    ),
    // rss: the Toeplitz hash skips the first input byte.
    (
        "    for i in 0..n {\n        let byte = match input.try_get(i) {\n            Option::Some(b) => widen_u64(b),\n            Option::None => 0,\n        };\n        h = h",
        "    for i in 1..n {\n        let byte = match input.try_get(i) {\n            Option::Some(b) => widen_u64(b),\n            Option::None => 0,\n        };\n        h = h",
    ),
];
