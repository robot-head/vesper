//! Intel PT packet decoding and flow reconstruction against streams built by
//! an encoder written here from the SDM packet tables, independently of the
//! decoder. No hardware-captured trace is used (the host has no `intel_pt`).

use vesper_chrono::ipt::{
    IpCompression, IpPacketKind, PSB_BYTES, PtDecoder, PtError, PtPacket, Tnt, decode_packets,
};
use vesper_chrono::ipt_flow::{BranchKind, CodeImage, FlowError, FlowLimits, reconstruct};

/// Packet encoder (Intel SDM Vol. 3C, Intel PT packet definitions).
#[derive(Default)]
struct Enc(Vec<u8>);

const TIP: u8 = 0x0d;
const PGE: u8 = 0x11;
const PGD: u8 = 0x01;
const FUP: u8 = 0x1d;

impl Enc {
    fn psb(&mut self) -> &mut Self {
        for _ in 0..8 {
            self.0.extend_from_slice(&[0x02, 0x82]);
        }
        self
    }
    fn psbend(&mut self) -> &mut Self {
        self.0.extend_from_slice(&[0x02, 0x23]);
        self
    }
    fn pad(&mut self) -> &mut Self {
        self.0.push(0);
        self
    }
    fn ovf(&mut self) -> &mut Self {
        self.0.extend_from_slice(&[0x02, 0xf3]);
        self
    }
    fn stop(&mut self) -> &mut Self {
        self.0.extend_from_slice(&[0x02, 0x83]);
        self
    }
    /// Short TNT: up to 6 bits, oldest first, then a stop bit, shifted past
    /// the 0 header bit.
    fn tnt(&mut self, taken: &[bool]) -> &mut Self {
        assert!((1..=6).contains(&taken.len()));
        let mut v: u8 = 1;
        for t in taken {
            v = (v << 1) | u8::from(*t);
        }
        self.0.push(v << 1);
        self
    }
    fn long_tnt(&mut self, taken: &[bool]) -> &mut Self {
        assert!((1..=47).contains(&taken.len()));
        let mut v: u64 = 1;
        for t in taken {
            v = (v << 1) | u64::from(*t);
        }
        self.0.extend_from_slice(&[0x02, 0xa3]);
        self.0.extend_from_slice(&v.to_le_bytes()[..6]);
        self
    }
    fn ip(&mut self, opcode: u8, ip_bytes: u8, ip: u64) -> &mut Self {
        self.0.push((ip_bytes << 5) | opcode);
        let n = match ip_bytes {
            0 => 0,
            1 => 2,
            2 => 4,
            3 | 4 => 6,
            6 => 8,
            _ => panic!("reserved"),
        };
        self.0.extend_from_slice(&ip.to_le_bytes()[..n]);
        self
    }
    fn pip(&mut self, cr3: u64, nr: bool) -> &mut Self {
        let payload = ((cr3 >> 5) << 1) | u64::from(nr);
        self.0.extend_from_slice(&[0x02, 0x43]);
        self.0.extend_from_slice(&payload.to_le_bytes()[..6]);
        self
    }
    fn mode_exec(&mut self, l: bool, d: bool) -> &mut Self {
        self.0
            .extend_from_slice(&[0x99, u8::from(l) | (u8::from(d) << 1)]);
        self
    }
    fn mode_tsx(&mut self, intx: bool, abort: bool) -> &mut Self {
        self.0
            .extend_from_slice(&[0x99, 0x20 | u8::from(intx) | (u8::from(abort) << 1)]);
        self
    }
    fn cbr(&mut self, ratio: u8) -> &mut Self {
        self.0.extend_from_slice(&[0x02, 0x03, ratio, 0]);
        self
    }
    fn tsc(&mut self, tsc: u64) -> &mut Self {
        self.0.push(0x19);
        self.0.extend_from_slice(&tsc.to_le_bytes()[..7]);
        self
    }
    fn mtc(&mut self, ctc: u8) -> &mut Self {
        self.0.extend_from_slice(&[0x59, ctc]);
        self
    }
    fn cyc(&mut self, mut cycles: u64) -> &mut Self {
        let mut first = 0b11 | (((cycles & 0x1f) as u8) << 3);
        cycles >>= 5;
        if cycles != 0 {
            first |= 0b100;
        }
        self.0.push(first);
        while cycles != 0 {
            let mut b = ((cycles & 0x7f) as u8) << 1;
            cycles >>= 7;
            if cycles != 0 {
                b |= 1;
            }
            self.0.push(b);
        }
        self
    }
    fn tma(&mut self, ctc: u16, fc: u16) -> &mut Self {
        let c = ctc.to_le_bytes();
        self.0
            .extend_from_slice(&[0x02, 0x73, c[0], c[1], 0, fc as u8, (fc >> 8) as u8 & 1]);
        self
    }
    fn bytes(&self) -> Vec<u8> {
        self.0.clone()
    }
}

fn packets(bytes: &[u8]) -> Vec<PtPacket> {
    decode_packets(bytes)
        .unwrap()
        .into_iter()
        .map(|p| p.packet)
        .collect()
}

#[test]
fn every_packet_kind_decodes() {
    let mut e = Enc::default();
    e.pad()
        .psb()
        .pip(0x1234_5000, true)
        .mode_exec(true, false)
        .mode_tsx(false, false)
        .cbr(42)
        .tsc(0x00ab_cdef_0123_4567)
        .tma(0xbeef, 0x1ff)
        .psbend()
        .ip(PGE, 6, 0xffff_8000_0040_1000)
        .tnt(&[true, false, true])
        .long_tnt(&[true; 40])
        .mtc(7)
        .cyc(3)
        .cyc(0x1234_5678)
        .ip(TIP, 1, 0x2222)
        .ip(FUP, 2, 0x3333_4444)
        .ip(PGD, 0, 0)
        .ovf()
        .stop();
    let got = packets(&e.bytes());
    let expected = vec![
        PtPacket::Pad,
        PtPacket::Psb,
        PtPacket::Pip {
            cr3: 0x1234_5000,
            non_root: true,
        },
        PtPacket::ModeExec {
            cs_l: true,
            cs_d: false,
        },
        PtPacket::ModeTsx {
            in_tx: false,
            tx_abort: false,
        },
        PtPacket::Cbr { ratio: 42 },
        PtPacket::Tsc {
            tsc: 0x00ab_cdef_0123_4567,
        },
        PtPacket::Tma {
            ctc: 0xbeef,
            fast_counter: 0x1ff,
        },
        PtPacket::PsbEnd,
        PtPacket::Ip {
            kind: IpPacketKind::TipPge,
            compression: IpCompression::Full,
            ip: Some(0xffff_8000_0040_1000),
        },
        PtPacket::Tnt {
            tnt: Tnt::from_bools(&[true, false, true]).unwrap(),
            long: false,
        },
        PtPacket::Tnt {
            tnt: Tnt::from_bools(&[true; 40]).unwrap(),
            long: true,
        },
        PtPacket::Mtc { ctc: 7 },
        PtPacket::Cyc { cycles: 3 },
        PtPacket::Cyc {
            cycles: 0x1234_5678,
        },
        PtPacket::Ip {
            kind: IpPacketKind::Tip,
            compression: IpCompression::Update16,
            ip: Some(0xffff_8000_0040_2222),
        },
        PtPacket::Ip {
            kind: IpPacketKind::Fup,
            compression: IpCompression::Update32,
            ip: Some(0xffff_8000_3333_4444),
        },
        PtPacket::Ip {
            kind: IpPacketKind::TipPgd,
            compression: IpCompression::Suppressed,
            ip: None,
        },
        PtPacket::Ovf,
        PtPacket::TraceStop,
    ];
    assert_eq!(got, expected);
    // TNT bit order is oldest first.
    let PtPacket::Tnt { tnt, .. } = got[10] else {
        unreachable!()
    };
    assert_eq!(tnt.iter().collect::<Vec<_>>(), vec![true, false, true]);
}

#[test]
fn ip_compression_modes_and_psb_reset() {
    let last = 0xffff_8000_1234_5678u64;
    let mut e = Enc::default();
    e.psb()
        .ip(TIP, 6, last)
        .ip(TIP, 1, 0xabcd) // upper 48 from last
        .ip(TIP, 2, 0x1111_2222) // upper 32 from last
        .ip(TIP, 3, 0x8000_0000_1000) // sign-extended 48
        .ip(TIP, 3, 0x7fff_0000_1000)
        .ip(TIP, 4, 0x0000_9999_0000) // upper 16 from last (0x0000 now)
        .psb() // resets last IP to 0
        .ip(TIP, 1, 0x4242);
    let ips: Vec<u64> = packets(&e.bytes())
        .into_iter()
        .filter_map(|p| match p {
            PtPacket::Ip { ip, .. } => ip,
            _ => None,
        })
        .collect();
    assert_eq!(
        ips,
        vec![
            last,
            0xffff_8000_1234_abcd,
            0xffff_8000_1111_2222,
            0xffff_8000_0000_1000,
            0x0000_7fff_0000_1000,
            0x0000_9999_0000,
            0x4242,
        ]
    );
    // Suppressed IPs leave the last IP alone.
    let mut e = Enc::default();
    e.psb().ip(TIP, 6, 0x5000).ip(PGD, 0, 0).ip(TIP, 1, 0x10);
    let mut d = PtDecoder::new(&e.0);
    let all: Vec<_> = d.by_ref().collect::<Result<_, _>>().unwrap();
    assert_eq!(all.len(), 4);
    assert_eq!(d.last_ip(), 0x0010);
}

#[test]
fn malformed_and_truncated_packets_are_typed_errors() {
    let psb = PSB_BYTES.to_vec();
    let with = |tail: &[u8]| {
        let mut v = psb.clone();
        v.extend_from_slice(tail);
        decode_packets(&v).unwrap_err()
    };
    let at = psb.len();
    assert_eq!(
        decode_packets(&[0x00, 0x19, 0, 0, 0, 0, 0, 0, 0]).unwrap_err(),
        PtError::NotSynchronized { offset: 1 }
    );
    assert_eq!(
        with(&[0x19, 1, 2]),
        PtError::Truncated {
            offset: at,
            needed: 8,
            available: 3
        }
    );
    assert_eq!(
        with(&[(0b101 << 5) | TIP]),
        PtError::ReservedIpBytes {
            offset: at,
            ip_bytes: 0b101
        }
    );
    assert_eq!(
        with(&[(0b111 << 5) | FUP, 0, 0, 0, 0, 0, 0, 0, 0]),
        PtError::ReservedIpBytes {
            offset: at,
            ip_bytes: 0b111
        }
    );
    assert!(matches!(
        with(&[(6 << 5) | TIP, 1, 2, 3]),
        PtError::Truncated { .. }
    ));
    assert_eq!(
        with(&[
            0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82,
            0x02, 0x83
        ]),
        PtError::MalformedPsb { offset: at }
    );
    assert_eq!(
        with(&[0x02, 0x55]),
        PtError::UnknownExtended {
            offset: at,
            opcode: 0x55
        }
    );
    assert_eq!(
        with(&[0x02, 0xa3, 0, 0, 0, 0, 0, 0]),
        PtError::EmptyTnt { offset: at }
    );
    assert_eq!(
        with(&[0x02, 0xa3, 1, 0, 0, 0, 0, 0]),
        PtError::EmptyTnt { offset: at }
    );
    assert_eq!(
        with(&[0x99, 0x40]),
        PtError::ReservedMode {
            offset: at,
            leaf: 2
        }
    );
    assert_eq!(with(&[0x99, 0x04]), PtError::ReservedBits { offset: at });
    assert_eq!(
        with(&[0x02, 0x03, 9, 1]),
        PtError::ReservedBits { offset: at }
    );
    assert_eq!(
        with(&[0x02, 0x73, 0, 0, 1, 0, 0]),
        PtError::ReservedBits { offset: at }
    );
    assert_eq!(
        with(&[0x05]),
        PtError::UnknownPacket {
            offset: at,
            byte: 0x05
        }
    );
    // A CYC longer than 64 bits.
    let mut cyc = vec![0b111];
    cyc.extend(std::iter::repeat_n(0xff, 9));
    cyc.push(0xfe);
    assert_eq!(with(&cyc), PtError::CycOverflow { offset: at });
    assert!(matches!(with(&[0b111]), PtError::Truncated { .. }));
    for n in 1..PSB_BYTES.len() {
        assert!(matches!(
            decode_packets(&PSB_BYTES[..n]),
            Err(PtError::Truncated { .. })
        ));
    }
    assert_eq!(PtDecoder::sync(&[1, 2, 3]).unwrap_err(), PtError::NoPsb);
    let mut junk = vec![0x77, 0x13];
    junk.extend_from_slice(&PSB_BYTES);
    let d = PtDecoder::sync(&junk).unwrap();
    assert_eq!(d.offset(), 2);
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for len in 0..400usize {
        let mut v = PSB_BYTES.to_vec();
        for _ in 0..len % 64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            v.push(state as u8);
        }
        let _ = decode_packets(&v);
        let _ = PtDecoder::sync(&v).map(|d| d.count());
    }
}

/// The program, at 0x401000 (hand-assembled x86-64):
///
/// ```text
/// 401000: 31 c0             xor  eax, eax
/// 401002: ff c0             inc  eax          ; loop:
/// 401004: 83 f8 03          cmp  eax, 3
/// 401007: 75 f9             jne  401002
/// 401009: e8 0a 00 00 00    call 401018
/// 40100e: ff e3             jmp  rbx
/// 401010: 90 ...            (padding)
/// 401018: 48 83 c0 01       add  rax, 1
/// 40101c: c3                ret
/// 401020: 74 02             je   401024
/// 401022: 90                nop
/// 401023: 90                nop
/// 401024: ff e0             jmp  rax
/// ```
const BASE: u64 = 0x401000;
fn program() -> Vec<u8> {
    let mut code = vec![0x90u8; 0x26];
    code[0x00..0x02].copy_from_slice(&[0x31, 0xc0]);
    code[0x02..0x04].copy_from_slice(&[0xff, 0xc0]);
    code[0x04..0x07].copy_from_slice(&[0x83, 0xf8, 0x03]);
    code[0x07..0x09].copy_from_slice(&[0x75, 0xf9]);
    code[0x09..0x0e].copy_from_slice(&[0xe8, 0x0a, 0, 0, 0]);
    code[0x0e..0x10].copy_from_slice(&[0xff, 0xe3]);
    code[0x18..0x1c].copy_from_slice(&[0x48, 0x83, 0xc0, 0x01]);
    code[0x1c] = 0xc3;
    code[0x20..0x22].copy_from_slice(&[0x74, 0x02]);
    code[0x24..0x26].copy_from_slice(&[0xff, 0xe0]);
    code
}

fn summary(trace: &vesper_chrono::ipt_flow::FlowTrace) -> Vec<(u64, Option<u64>, BranchKind)> {
    trace
        .branches
        .iter()
        .map(|b| (b.from, b.to, b.kind))
        .collect()
}

#[test]
fn reconstructs_branches_over_a_code_image() {
    let code = program();
    let image = CodeImage::single(BASE, &code);
    let mut e = Enc::default();
    e.psb()
        .mode_exec(true, false)
        .psbend()
        .ip(PGE, 6, BASE)
        .tsc(100)
        // jne taken, taken, not taken; then the compressed ret.
        .tnt(&[true, true, false, true])
        .ip(TIP, 1, 0x1020)
        .tsc(200)
        .tnt(&[false])
        .ip(PGD, 0, 0)
        .tsc(300);
    let packets = decode_packets(&e.bytes()).unwrap();
    let trace = reconstruct(&packets, &image, FlowLimits::default()).unwrap();
    use BranchKind::*;
    assert_eq!(
        summary(&trace),
        vec![
            (BASE, Some(BASE), Enable),
            (0x401007, Some(0x401002), Conditional { taken: true }),
            (0x401007, Some(0x401002), Conditional { taken: true }),
            (0x401007, Some(0x401009), Conditional { taken: false }),
            (0x401009, Some(0x401018), DirectCall),
            (0x40101c, Some(0x40100e), Return { compressed: true }),
            (0x40100e, Some(0x401020), IndirectJump),
            (0x401020, Some(0x401022), Conditional { taken: false }),
            (0x401024, None, Disable),
        ]
    );
    // xor + 3*(inc,cmp,jne) + call + add + ret + jmp rbx + je + nop + nop + jmp rax
    assert_eq!(trace.instructions, 1 + 9 + 1 + 2 + 1 + 1 + 2 + 1);
    // TSC intervals: everything before the TSC 200 packet lies in [100, 200).
    let b = &trace.branches;
    assert_eq!((b[1].tsc_lo, b[1].tsc_hi), (Some(100), Some(200)));
    assert_eq!((b[6].tsc_lo, b[6].tsc_hi), (Some(100), Some(200)));
    assert_eq!((b[7].tsc_lo, b[7].tsc_hi), (Some(200), Some(300)));
    assert_eq!(b[0].tsc_lo, None);
}

#[test]
fn uncompressed_return_async_event_and_overflow() {
    let code = program();
    let image = CodeImage::single(BASE, &code);
    let mut e = Enc::default();
    e.psb()
        .psbend()
        .ip(PGE, 6, BASE)
        .long_tnt(&[true, true, false])
        // The return is answered by a TIP (return compression disabled).
        .ip(TIP, 1, 0x100e)
        // An interrupt arrives at the jmp rbx: FUP(source), TIP(handler).
        .ip(FUP, 1, 0x100e)
        .ip(TIP, 1, 0x1020)
        // Overflow: flow is lost, resumes at the next FUP.
        .ovf()
        .ip(FUP, 1, 0x1022)
        .ip(PGD, 0, 0);
    let packets = decode_packets(&e.bytes()).unwrap();
    let trace = reconstruct(&packets, &image, FlowLimits::default()).unwrap();
    use BranchKind::*;
    let s = summary(&trace);
    assert_eq!(
        s[5],
        (0x40101c, Some(0x40100e), Return { compressed: false })
    );
    assert_eq!(s[6], (0x40100e, Some(0x401020), Async));
    assert_eq!(trace.overflows, 1);
    // After the OVF, flow resumed at 0x401022: nop, nop, jmp rax -> PGD.
    assert_eq!(*s.last().unwrap(), (0x401024, None, Disable));
}

#[test]
fn psb_plus_resynchronises_and_detects_desync() {
    let code = program();
    let image = CodeImage::single(BASE, &code);
    let mut e = Enc::default();
    e.psb()
        .psbend()
        .ip(PGE, 6, BASE)
        .tnt(&[true, true, false, true])
        // A periodic PSB+ restating the current IP (the jmp rbx).
        .psb()
        .ip(FUP, 6, 0x40100e)
        .tsc(5)
        .psbend()
        .ip(TIP, 6, 0x401020);
    let packets = decode_packets(&e.bytes()).unwrap();
    let trace = reconstruct(&packets, &image, FlowLimits::default()).unwrap();
    assert_eq!(trace.branches.last().unwrap().to, Some(0x401020));
    // The packets end at je: reconstruction stops there.
    assert_eq!(trace.stopped_at, Some(0x401020));

    let mut e = Enc::default();
    e.psb()
        .psbend()
        .ip(PGE, 6, BASE)
        .tnt(&[true, true, false, true])
        .psb()
        .ip(FUP, 6, 0x401000)
        .psbend();
    let packets = decode_packets(&e.bytes()).unwrap();
    assert!(matches!(
        reconstruct(&packets, &image, FlowLimits::default()),
        Err(FlowError::Desynchronized {
            expected: 0x40100e,
            found: 0x401000,
            ..
        })
    ));
}

#[test]
fn trace_and_image_mismatches_are_typed_errors() {
    let code = program();
    let image = CodeImage::single(BASE, &code);
    let run = |e: &Enc| {
        reconstruct(
            &decode_packets(&e.0).unwrap(),
            &image,
            FlowLimits::default(),
        )
    };

    // A TIP where the conditional branch needs a TNT bit.
    let mut e = Enc::default();
    e.psb().ip(PGE, 6, BASE).ip(TIP, 6, BASE);
    assert!(matches!(
        run(&e),
        Err(FlowError::ExpectedTnt { ip: 0x401007, .. })
    ));

    // A TNT where the indirect jump needs a TIP.
    let mut e = Enc::default();
    e.psb()
        .ip(PGE, 6, BASE)
        .tnt(&[true, true, false, true])
        .tnt(&[true]);
    assert!(matches!(
        run(&e),
        Err(FlowError::ExpectedTip { ip: 0x40100e, .. })
    ));

    // A not-taken bit for a compressed return.
    let mut e = Enc::default();
    e.psb().ip(PGE, 6, BASE).tnt(&[true, true, false, false]);
    assert_eq!(
        run(&e),
        Err(FlowError::BadCompressedReturn { ip: 0x40101c })
    );

    // Execution leaving the image.
    let mut e = Enc::default();
    e.psb()
        .ip(PGE, 6, BASE)
        .tnt(&[true, true, false, true])
        .ip(TIP, 6, 0x500000);
    assert!(matches!(
        run(&e),
        Err(FlowError::IpOutsideImage { ip: 0x500000, .. })
    ));

    // Leftover TNT bits: the trace took more branches than the code has.
    let mut e = Enc::default();
    e.psb()
        .ip(PGE, 6, BASE)
        .tnt(&[true, true, false, true, true]);
    assert!(matches!(
        run(&e),
        Err(FlowError::ExpectedTip { .. }) | Err(FlowError::UnusedTnt { .. })
    ));

    // A suppressed TIP where a target is needed.
    let mut e = Enc::default();
    e.psb()
        .ip(PGE, 6, BASE)
        .tnt(&[true, true, false, true])
        .ip(TIP, 0, 0);
    assert!(matches!(run(&e), Err(FlowError::SuppressedTarget { .. })));

    // Undecodable bytes.
    let bad = [0x0f, 0x0b, 0xff, 0xff];
    let image2 = CodeImage::single(0x1000, &bad[2..]);
    let mut e = Enc::default();
    e.psb().ip(PGE, 6, 0x1000);
    assert!(matches!(
        reconstruct(
            &decode_packets(&e.0).unwrap(),
            &image2,
            FlowLimits::default()
        ),
        Err(FlowError::BadInstruction { ip: 0x1000 })
    ));

    // A self-loop `jmp $` exhausts the instruction budget.
    let spin = [0xeb, 0xfe];
    let image3 = CodeImage::single(0x2000, &spin);
    let mut e = Enc::default();
    e.psb().ip(PGE, 6, 0x2000);
    assert_eq!(
        reconstruct(
            &decode_packets(&e.0).unwrap(),
            &image3,
            FlowLimits {
                max_instructions: 1000,
                ..FlowLimits::default()
            }
        ),
        Err(FlowError::InstructionBudget { limit: 1000 })
    );

    // TSX is rejected.
    let mut e = Enc::default();
    e.psb().ip(PGE, 6, BASE).mode_tsx(true, false);
    assert!(matches!(run(&e), Err(FlowError::Unsupported { .. })));

    // Overlapping segments are rejected.
    let mut image4 = CodeImage::new();
    image4.add(0x1000, &code).unwrap();
    assert!(image4.add(0x1010, &code).is_err());
    assert!(image4.add(u64::MAX - 2, &code).is_err());
}
