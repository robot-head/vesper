//! Under `vesper-port`, compile the Vesper package in vesper/ (the
//! `@vesper/warp` Loom package) into a static library and check the
//! committed binding (see docs/maintenance/porting-a-crate.md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("warp", "vesper").binding("port/binding.rs");
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            // One module at a time (`VESPER_WARP_DIVERGE=frame`), or all.
            println!("cargo:rerun-if-env-changed=VESPER_WARP_DIVERGE");
            let only = std::env::var("VESPER_WARP_DIVERGE").ok();
            DIVERGENT_EDITS
                .iter()
                .filter(|(module, _, _)| only.as_deref().is_none_or(|m| m == *module))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

/// The divergence check: one wrong line per ported module (mirrored in
/// BUILD.bazel). `port/test-port.sh` asserts that the tests covering each
/// module fail and that a test touching none of them passes.
#[cfg(feature = "vesper-port-divergent")]
const DIVERGENT_EDITS: &[(&str, &str, &str)] = &[
    // varint.ves: a 2-byte varint keeps only 13 value bits.
    ("varint", "widen_u64(v) & VI2_MASK", "widen_u64(v) & 8191"),
    // pn.ves: the A.3 half-window shrinks to a quarter.
    ("pn", "let hwin = win / 2;", "let hwin = win / 4;"),
    // header.ves: the short-header packet number starts one byte late.
    (
        "header",
        "let pn_offset = wrapping_add(dcid_len, 1);",
        "let pn_offset = wrapping_add(dcid_len, 2);",
    ),
    // frame.ves: a STREAM frame's FIN bit reads inverted.
    (
        "frame",
        "fin: ty & stream_fin() != 0,",
        "fin: ty & stream_fin() == 0,",
    ),
    // ack.ves: an immediate ACK waits for three ack-eliciting packets.
    (
        "ack",
        "s.unacked_ack_eliciting >= ACK_ELICITING_THRESHOLD",
        "s.unacked_ack_eliciting >= 3",
    ),
    // recovery.ves: kPacketThreshold becomes 4.
    (
        "recovery",
        "sat_add(e.pn, PACKET_THRESHOLD)",
        "sat_add(e.pn, 4)",
    ),
    // stream.ves: an in-order insert no longer advances the readable prefix.
    ("stream", "if start <= s.contig {", "if start < s.contig {"),
    // lib.ves (lifecycle.rs): a Retry token's ODCID is read one byte early.
    (
        "lifecycle",
        "in token[(TK_ODCID)..wrapping_add(TK_ODCID, len)]",
        "in token[(TK_ODCID_LEN)..wrapping_add(TK_ODCID_LEN, len)]",
    ),
    // packet.ves (endpoint.rs): 1-RTT packets always carry key phase 0.
    ("packet", "let phase = key_phase;", "let phase = false;"),
];
