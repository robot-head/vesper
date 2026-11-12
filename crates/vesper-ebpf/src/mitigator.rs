//! The §27.3 XDP SYN-flood mitigator, assembled with [`Asm`].
//!
//! Behaviour, matching `mitigate_syn_flood` in the design:
//!
//! 1. Packets shorter than the 54-byte [`EthIpv4TcpWire`] prefix pass
//!    (the canonical `data + 54 <= data_end` guard).
//! 2. Non-IPv4 or non-TCP packets pass.
//! 3. Unless the TCP flags (low 6 bits of `data_offset_flags`) have SYN set
//!    and ACK clear, the packet passes.
//! 4. The per-source SYN counter (per-CPU hash keyed by the source address
//!    in network byte order) is incremented; a source not yet in the map
//!    is inserted with count 1.
//! 5. While the count is at most the threshold, the packet passes.
//! 6. Above the threshold the packet is dropped. A [`SynDropAlertWire`]
//!    event is written to the ring buffer when a record can be reserved;
//!    when the ring is full the packet is still dropped.
//!
//! Deviations from the design text, recorded in the lane report:
//! * the threshold is a build-time constant baked into the program as a
//!   64-bit immediate rather than a runtime argument (XDP programs take
//!   only the context);
//! * the design's `BpfMaps::percpu_increment_u64` is open-coded as
//!   lookup, in-place add, or insert of 1;
//! * if the counter map is full, a new source cannot be inserted and its
//!   SYNs pass (fail open); counts are never reset by the program (user
//!   space owns the measurement window).
//!
//! The program is hand-assembled; the release of the ring reservation on
//! every path is checked by the verifier's `ref.leak` rule, not by a typed
//! builder (see the lane report).
//!
//! [`EthIpv4TcpWire`]: crate::wire::EthIpv4TcpWire
//! [`SynDropAlertWire`]: crate::wire::SynDropAlertWire

use crate::helpers as h;
use crate::insn::{AluOp, Endian, JmpOp, Reg, Size};
use crate::program::{Asm, AsmError, MapDef, MapKind, Program, ProgramType};
use crate::wire::{
    ETH_P_IPV4, EthIpv4TcpWire, IPPROTO_TCP, SynDropAlertWire, TCP_FLAG_ACK, TCP_FLAG_SYN,
    WireFrame, XdpAction, eth_ipv4_tcp as o, syn_drop_alert as a,
};

/// Index of the per-source counter map in [`Program::maps`].
pub const COUNTS_MAP: usize = 0;
/// Index of the alert ring buffer in [`Program::maps`].
pub const ALERTS_MAP: usize = 1;

/// Build-time parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MitigatorConfig {
    /// SYNs per source allowed before dropping (`pps_threshold`).
    pub pps_threshold: u64,
    /// Capacity of the per-source counter map.
    pub max_sources: u32,
    /// Ring buffer size in bytes (power of two, at least 4096).
    pub ring_bytes: u32,
}

impl Default for MitigatorConfig {
    fn default() -> Self {
        Self {
            pps_threshold: 64,
            max_sources: 65_536,
            ring_bytes: 1 << 16,
        }
    }
}

/// Assemble the mitigator.
pub fn build(cfg: &MitigatorConfig) -> Result<Program, AsmError> {
    let mut asm = Asm::new();
    let counts = asm.map(MapDef::new(
        "syn_counts",
        MapKind::PerCpuHash,
        4,
        8,
        cfg.max_sources,
    ))?;
    let alerts = asm.map(MapDef::ringbuf("syn_alerts", cfg.ring_bytes))?;
    debug_assert_eq!((counts, alerts), (COUNTS_MAP, ALERTS_MAP));

    let pass = asm.label();
    let drop = asm.label();
    let have_count = asm.label();

    // r2 = data, r3 = data_end
    asm.ldx(Size::W, Reg::R2, Reg::R1, 0);
    asm.ldx(Size::W, Reg::R3, Reg::R1, 4);
    // if data + 54 > data_end: pass
    asm.mov64(Reg::R4, Reg::R2);
    asm.add64(Reg::R4, EthIpv4TcpWire::SIZE as i32);
    asm.jmp(JmpOp::Gt, Reg::R4, Reg::R3, pass);
    // ether_type (big-endian on the wire)
    asm.ldx(Size::H, Reg::R5, Reg::R2, o::ETHER_TYPE as i16);
    asm.endian(Endian::ToBe, 16, Reg::R5);
    asm.jmp(JmpOp::Ne, Reg::R5, ETH_P_IPV4 as i32, pass);
    asm.ldx(Size::B, Reg::R5, Reg::R2, o::PROTO as i16);
    asm.jmp(JmpOp::Ne, Reg::R5, IPPROTO_TCP as i32, pass);
    // r7 = flags = data_offset_flags & 0x3f (host order)
    asm.ldx(Size::H, Reg::R7, Reg::R2, o::DATA_OFFSET_FLAGS as i16);
    asm.endian(Endian::ToBe, 16, Reg::R7);
    asm.alu64(AluOp::And, Reg::R7, 0x3f);
    // SYN set and ACK clear, else pass
    asm.mov64(Reg::R5, Reg::R7);
    asm.alu64(AluOp::And, Reg::R5, (TCP_FLAG_SYN | TCP_FLAG_ACK) as i32);
    asm.jmp(JmpOp::Ne, Reg::R5, TCP_FLAG_SYN as i32, pass);
    // r6 = src_ipv4 and r8 = dst_port, kept in wire byte order
    asm.ldx(Size::W, Reg::R6, Reg::R2, o::SRC_IPV4 as i16);
    asm.ldx(Size::H, Reg::R8, Reg::R2, o::DST_PORT as i16);
    // key at fp-4
    asm.stx(Size::W, Reg::R10, -4, Reg::R6);
    // r0 = lookup(&syn_counts, fp-4)
    asm.ld_map(Reg::R1, counts);
    asm.mov64(Reg::R2, Reg::R10);
    asm.add64(Reg::R2, -4);
    asm.call(h::MAP_LOOKUP_ELEM);
    let missing = asm.label();
    asm.jmp(JmpOp::Eq, Reg::R0, 0, missing);
    // existing: *r0 += 1; r9 = *r0
    asm.ldx(Size::DW, Reg::R9, Reg::R0, 0);
    asm.add64(Reg::R9, 1);
    asm.stx(Size::DW, Reg::R0, 0, Reg::R9);
    asm.ja(have_count);
    // missing: value 1 at fp-16, update(&syn_counts, fp-4, fp-16, BPF_ANY)
    asm.bind(missing)?;
    asm.mov64(Reg::R9, 1);
    asm.stx(Size::DW, Reg::R10, -16, Reg::R9);
    asm.ld_map(Reg::R1, counts);
    asm.mov64(Reg::R2, Reg::R10);
    asm.add64(Reg::R2, -4);
    asm.mov64(Reg::R3, Reg::R10);
    asm.add64(Reg::R3, -16);
    asm.mov64(Reg::R4, 0);
    asm.call(h::MAP_UPDATE_ELEM);
    asm.bind(have_count)?;
    // if count <= threshold: pass
    asm.ld_imm64(Reg::R1, cfg.pps_threshold);
    asm.jmp(JmpOp::Le, Reg::R9, Reg::R1, pass);
    // reserve an alert record
    asm.ld_map(Reg::R1, alerts);
    asm.mov64(Reg::R2, SynDropAlertWire::SIZE as i32);
    asm.mov64(Reg::R3, 0);
    asm.call(h::RINGBUF_RESERVE);
    asm.jmp(JmpOp::Eq, Reg::R0, 0, drop);
    asm.stx(Size::W, Reg::R0, a::SRC_IPV4 as i16, Reg::R6);
    asm.stx(Size::H, Reg::R0, a::DST_PORT as i16, Reg::R8);
    asm.endian(Endian::ToBe, 16, Reg::R7);
    asm.stx(Size::H, Reg::R0, a::TCP_FLAGS as i16, Reg::R7);
    // observed_pps is little-endian on the wire (the BPF target is LE)
    asm.stx(Size::DW, Reg::R0, a::OBSERVED_PPS as i16, Reg::R9);
    asm.mov64(Reg::R1, Reg::R0);
    asm.mov64(Reg::R2, 0);
    asm.call(h::RINGBUF_SUBMIT);
    asm.bind(drop)?;
    asm.mov64(Reg::R0, XdpAction::Drop as i32);
    asm.exit();
    asm.bind(pass)?;
    asm.mov64(Reg::R0, XdpAction::Pass as i32);
    asm.exit();
    asm.finish("mitigate_syn_flood", ProgramType::Xdp)
}

/// Reference model of the program over a packet and the counter of its
/// source *before* the packet, returning the action, the new counter value
/// (if the packet is counted) and whether an alert is due. Used by the
/// fixture tests; the Lean model in `lean/EbpfFirewall.lean` states the
/// same decision.
pub fn reference_decision(
    packet: &[u8],
    prior_count: Option<u64>,
    threshold: u64,
) -> (XdpAction, Option<u64>) {
    let Ok(hdr) = EthIpv4TcpWire::decode(packet) else {
        return (XdpAction::Pass, None);
    };
    if hdr.ether_type != ETH_P_IPV4 || hdr.proto != IPPROTO_TCP {
        return (XdpAction::Pass, None);
    }
    let flags = hdr.data_offset_flags & 0x3f;
    if flags & TCP_FLAG_SYN == 0 || flags & TCP_FLAG_ACK != 0 {
        return (XdpAction::Pass, None);
    }
    let count = prior_count.map_or(1, |c| c.wrapping_add(1));
    if count <= threshold {
        (XdpAction::Pass, Some(count))
    } else {
        (XdpAction::Drop, Some(count))
    }
}
