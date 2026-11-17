//! Under `vesper-port`, compile the Vesper library `tcp` (the files of
//! vesper/, concatenated in the order of [`PARTS`]) into a static library
//! and check the committed binding (see docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let mut source = String::new();
        for part in PARTS {
            let path = format!("vesper/{part}.ves");
            println!("cargo:rerun-if-changed={path}");
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
            source.push_str(&text);
            source.push('\n');
        }
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let tcp = out.join("tcp.ves");
        std::fs::write(&tcp, source).expect("write tcp.ves");
        let port = vesper_port_harness::Port::new("tcp", &tcp)
            .binding("port/binding.rs")
            .rust_types("crate::ported::types");
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            // One module at a time (`VESPER_TCP_DIVERGE=seq`), or all.
            println!("cargo:rerun-if-env-changed=VESPER_TCP_DIVERGE");
            let only = std::env::var("VESPER_TCP_DIVERGE").ok();
            DIVERGENT_EDITS
                .iter()
                .filter(|(module, _, _)| only.as_deref().is_none_or(|m| m == *module))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

/// The library's files in order (mirrored as `TCP_PARTS` in BUILD.bazel):
/// one per Rust module, the larger ones split by topic so that each stays
/// under the formatter's 8192-token cap.
#[cfg(feature = "vesper-port")]
const PARTS: &[&str] = &[
    "int",
    "seq",
    "rtt",
    "cc",
    "wire",
    "options",
    "segment",
    "checksum",
    "conn",
    "conn_user",
    "conn_helpers",
    "conn_input",
    "conn_ack",
    "conn_text",
    "conn_timers",
    "conn_transmit",
    "conn_emit",
];

/// The divergence check: one wrong line per ported module (mirrored in
/// BUILD.bazel). Each must make the tests that cover its module fail.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str, &str)] = &[
    // seq.rs: `>=` in sequence space becomes `>`.
    ("seq", "    sn_diff(a, b) >= 0\n", "    sn_diff(a, b) > 0\n"),
    // rtt.rs: RTTVAR starts at R instead of R/2.
    (
        "rtt",
        "        e.rttvar_ns = r_ns / 2;\n",
        "        e.rttvar_ns = r_ns;\n",
    ),
    // cc.rs: NewReno halves to a single segment floor.
    (
        "cc",
        "    cc.ssthresh = max32(cc.cwnd / 2, wrapping_mul(cc.mss, 2));\n    cc.cwnd = cc.ssthresh;",
        "    cc.ssthresh = max32(cc.cwnd / 2, cc.mss);\n    cc.cwnd = cc.ssthresh;",
    ),
    // wire.rs: the odd trailing checksum byte is dropped.
    (
        "wire",
        "        a = wrapping_add(a, widen_u64(bytes[i]) << 8);\n",
        "        a = wrapping_add(a, 0);\n",
    ),
    // conn.rs: the duplicate-ACK threshold drops from 3 to 2.
    (
        "conn",
        "fn dup_thresh() -> u32 {\n    3\n}",
        "fn dup_thresh() -> u32 {\n    2\n}",
    ),
];
