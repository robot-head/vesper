//! Intel Processor Trace packet decoder (§31.1 item 3).
//!
//! Decodes the packet layer of the Intel PT format (Intel SDM Vol. 3C,
//! chapter "Intel Processor Trace", packet definitions) into a typed packet
//! stream: PSB, PSBEND, short and long TNT, TIP / TIP.PGE / TIP.PGD / FUP with
//! all IP-compression modes, PIP, MODE.Exec / MODE.TSX, CBR, TSC, MTC, CYC,
//! TMA, OVF, TraceStop and PAD.
//!
//! Decoding starts at a PSB (`PtDecoder::new` requires one at offset 0 after
//! padding; `PtDecoder::sync` searches for the first). The "last IP" used by
//! IP compression is reset to zero at every PSB, as the format specifies.
//! Unknown, reserved, malformed or truncated packets are typed errors and end
//! the stream; the decoder never panics on untrusted bytes.
//!
//! The decoder has only been exercised on streams produced by the test
//! encoder in `tests/ipt.rs`, written independently from the SDM tables. No
//! trace from real hardware has been decoded (the development host has no
//! `intel_pt` PMU).

use std::fmt;

/// A PSB is `02 82` repeated eight times.
pub const PSB_BYTES: [u8; 16] = [
    0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82, 0x02, 0x82,
];

/// Which IP-carrying packet a [`PtPacket::Ip`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpPacketKind {
    /// Target of an indirect branch, return or far transfer.
    Tip,
    /// Tracing enabled (packet generation enable) at this IP.
    TipPge,
    /// Tracing disabled; the IP, when present, is the branch target.
    TipPgd,
    /// Flow update: source IP of an asynchronous event, or the current IP
    /// inside PSB+.
    Fup,
}

/// How the IP of an IP packet was compressed (the IPBytes field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpCompression {
    /// `000`: IP suppressed (out of context).
    Suppressed,
    /// `001`: IP\[15:0\], upper bits from the last IP.
    Update16,
    /// `010`: IP\[31:0\], upper bits from the last IP.
    Update32,
    /// `011`: IP\[47:0\], sign-extended from bit 47.
    Sext48,
    /// `100`: IP\[47:0\], upper 16 bits from the last IP.
    Update48,
    /// `110`: the full 64-bit IP.
    Full,
}

impl IpCompression {
    pub fn from_ip_bytes(bits: u8) -> Option<Self> {
        Some(match bits {
            0b000 => Self::Suppressed,
            0b001 => Self::Update16,
            0b010 => Self::Update32,
            0b011 => Self::Sext48,
            0b100 => Self::Update48,
            0b110 => Self::Full,
            _ => return None,
        })
    }
    pub fn ip_bytes(self) -> u8 {
        match self {
            Self::Suppressed => 0b000,
            Self::Update16 => 0b001,
            Self::Update32 => 0b010,
            Self::Sext48 => 0b011,
            Self::Update48 => 0b100,
            Self::Full => 0b110,
        }
    }
    /// Payload bytes following the header.
    pub fn payload_len(self) -> usize {
        match self {
            Self::Suppressed => 0,
            Self::Update16 => 2,
            Self::Update32 => 4,
            Self::Sext48 | Self::Update48 => 6,
            Self::Full => 8,
        }
    }
    /// Reconstruct the IP from the payload and the last IP.
    pub fn apply(self, payload: u64, last_ip: u64) -> Option<u64> {
        Some(match self {
            Self::Suppressed => return None,
            Self::Update16 => (last_ip & !0xffff) | (payload & 0xffff),
            Self::Update32 => (last_ip & !0xffff_ffff) | (payload & 0xffff_ffff),
            Self::Sext48 => (((payload << 16) as i64) >> 16) as u64,
            Self::Update48 => (last_ip & !0xffff_ffff_ffff) | (payload & 0xffff_ffff_ffff),
            Self::Full => payload,
        })
    }
}

/// Taken / not-taken bits of a TNT packet, oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tnt {
    /// Bit `count - 1` is the oldest branch, bit 0 the youngest.
    pub bits: u64,
    pub count: u8,
}

impl Tnt {
    /// Iterate the branches oldest first (`true` = taken).
    pub fn iter(self) -> impl Iterator<Item = bool> {
        (0..self.count)
            .rev()
            .map(move |i| (self.bits >> i) & 1 == 1)
    }
    pub fn from_bools(taken: &[bool]) -> Option<Self> {
        if taken.is_empty() || taken.len() > 47 {
            return None;
        }
        let bits = taken.iter().fold(0u64, |acc, t| (acc << 1) | u64::from(*t));
        Some(Self {
            bits,
            count: taken.len() as u8,
        })
    }
}

/// One decoded packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PtPacket {
    Pad,
    Psb,
    PsbEnd,
    /// Short (1..=6 bits) or long (1..=47 bits) TNT.
    Tnt {
        tnt: Tnt,
        long: bool,
    },
    /// TIP, TIP.PGE, TIP.PGD or FUP. `ip` is `None` when suppressed.
    Ip {
        kind: IpPacketKind,
        compression: IpCompression,
        ip: Option<u64>,
    },
    /// Paging information: CR3 and the non-root (VMX) bit.
    Pip {
        cr3: u64,
        non_root: bool,
    },
    /// MODE.Exec: `cs_l` (64-bit) and `cs_d` (32-bit default size).
    ModeExec {
        cs_l: bool,
        cs_d: bool,
    },
    /// MODE.TSX.
    ModeTsx {
        in_tx: bool,
        tx_abort: bool,
    },
    /// Core:bus ratio.
    Cbr {
        ratio: u8,
    },
    /// Low 56 bits of the time-stamp counter.
    Tsc {
        tsc: u64,
    },
    /// Low 8 bits of the crystal clock (CTC) counter.
    Mtc {
        ctc: u8,
    },
    /// Cycle count since the last CYC.
    Cyc {
        cycles: u64,
    },
    /// TSC/MTC alignment: CTC\[15:0\] and the 9-bit fast counter.
    Tma {
        ctc: u16,
        fast_counter: u16,
    },
    /// Internal buffer overflow: packets were lost.
    Ovf,
    TraceStop,
}

impl PtPacket {
    /// Timing and padding packets that do not affect control flow.
    pub fn is_timing(&self) -> bool {
        matches!(
            self,
            Self::Pad
                | Self::Cbr { .. }
                | Self::Tsc { .. }
                | Self::Mtc { .. }
                | Self::Cyc { .. }
                | Self::Tma { .. }
        )
    }
}

/// A packet and the byte offset of its first byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PtPacketAt {
    pub offset: usize,
    pub len: usize,
    pub packet: PtPacket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtError {
    /// The packet at `offset` needs `needed` bytes; only `available` remain.
    Truncated {
        offset: usize,
        needed: usize,
        available: usize,
    },
    /// The first packet (after padding) is not a PSB.
    NotSynchronized { offset: usize },
    /// No PSB anywhere in the buffer.
    NoPsb,
    /// A header byte that is reserved or not a supported packet.
    UnknownPacket { offset: usize, byte: u8 },
    /// `02 xx` with an unsupported extended opcode.
    UnknownExtended { offset: usize, opcode: u8 },
    /// A PSB whose sixteen bytes are not `02 82` repeated.
    MalformedPsb { offset: usize },
    /// An IP packet with a reserved IPBytes value (`101` or `111`).
    ReservedIpBytes { offset: usize, ip_bytes: u8 },
    /// A long TNT whose payload has no stop bit.
    EmptyTnt { offset: usize },
    /// A MODE packet with a reserved leaf.
    ReservedMode { offset: usize, leaf: u8 },
    /// A CYC packet whose count does not fit in 64 bits.
    CycOverflow { offset: usize },
    /// A non-zero reserved field.
    ReservedBits { offset: usize },
}

impl fmt::Display for PtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "intel pt: {self:?}")
    }
}
impl std::error::Error for PtError {}

/// Streaming packet decoder. Iteration stops after the first error.
#[derive(Debug, Clone)]
pub struct PtDecoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    last_ip: u64,
    synced: bool,
    failed: bool,
}

impl<'a> PtDecoder<'a> {
    /// Decode from offset 0; the first non-PAD packet must be a PSB.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            last_ip: 0,
            synced: false,
            failed: false,
        }
    }

    /// Start at the first PSB in `bytes`.
    pub fn sync(bytes: &'a [u8]) -> Result<Self, PtError> {
        let at = bytes
            .windows(PSB_BYTES.len())
            .position(|w| w == PSB_BYTES)
            .ok_or(PtError::NoPsb)?;
        Ok(Self {
            bytes,
            pos: at,
            last_ip: 0,
            synced: false,
            failed: false,
        })
    }

    /// Byte offset of the next packet.
    pub fn offset(&self) -> usize {
        self.pos
    }

    /// The last IP used for IP compression.
    pub fn last_ip(&self) -> u64 {
        self.last_ip
    }

    fn need(&self, offset: usize, needed: usize) -> Result<&'a [u8], PtError> {
        let available = self.bytes.len() - offset;
        if available < needed {
            return Err(PtError::Truncated {
                offset,
                needed,
                available,
            });
        }
        Ok(&self.bytes[offset..offset + needed])
    }

    fn le(bytes: &[u8]) -> u64 {
        bytes
            .iter()
            .rev()
            .fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
    }

    fn decode_one(&mut self) -> Result<PtPacketAt, PtError> {
        let offset = self.pos;
        let b0 = self.bytes[offset];
        let (packet, len) = match b0 {
            0x00 => (PtPacket::Pad, 1),
            0x02 => self.decode_extended(offset)?,
            b if b & 1 == 0 => {
                // Short TNT: payload bits [7:1], stop bit is the highest set
                // bit; `b != 0x02` and `b != 0` leave at least one TNT bit.
                let payload = b >> 1;
                let stop = 7 - payload.leading_zeros() as u8; // bit index in payload
                if stop == 0 {
                    return Err(PtError::UnknownPacket { offset, byte: b });
                }
                let tnt = Tnt {
                    bits: u64::from(payload) & ((1u64 << stop) - 1),
                    count: stop,
                };
                (PtPacket::Tnt { tnt, long: false }, 1)
            }
            b if b & 0b11 == 0b11 => self.decode_cyc(offset)?,
            0x19 => {
                let p = self.need(offset, 8)?;
                (
                    PtPacket::Tsc {
                        tsc: Self::le(&p[1..8]),
                    },
                    8,
                )
            }
            0x59 => {
                let p = self.need(offset, 2)?;
                (PtPacket::Mtc { ctc: p[1] }, 2)
            }
            0x99 => {
                let p = self.need(offset, 2)?;
                let leaf = p[1] >> 5;
                let packet = match leaf {
                    0b000 => {
                        if p[1] & 0b1_1100 != 0 {
                            return Err(PtError::ReservedBits { offset });
                        }
                        PtPacket::ModeExec {
                            cs_l: p[1] & 1 == 1,
                            cs_d: p[1] & 2 == 2,
                        }
                    }
                    0b001 => {
                        if p[1] & 0b1_1100 != 0 {
                            return Err(PtError::ReservedBits { offset });
                        }
                        PtPacket::ModeTsx {
                            in_tx: p[1] & 1 == 1,
                            tx_abort: p[1] & 2 == 2,
                        }
                    }
                    leaf => return Err(PtError::ReservedMode { offset, leaf }),
                };
                (packet, 2)
            }
            b => {
                let kind = match b & 0x1f {
                    0x0d => IpPacketKind::Tip,
                    0x11 => IpPacketKind::TipPge,
                    0x01 => IpPacketKind::TipPgd,
                    0x1d => IpPacketKind::Fup,
                    _ => return Err(PtError::UnknownPacket { offset, byte: b }),
                };
                let ip_bytes = b >> 5;
                let compression = IpCompression::from_ip_bytes(ip_bytes)
                    .ok_or(PtError::ReservedIpBytes { offset, ip_bytes })?;
                let n = compression.payload_len();
                let p = self.need(offset, 1 + n)?;
                let ip = compression.apply(Self::le(&p[1..]), self.last_ip);
                if let Some(ip) = ip {
                    self.last_ip = ip;
                }
                (
                    PtPacket::Ip {
                        kind,
                        compression,
                        ip,
                    },
                    1 + n,
                )
            }
        };
        Ok(PtPacketAt {
            offset,
            len,
            packet,
        })
    }

    fn decode_extended(&mut self, offset: usize) -> Result<(PtPacket, usize), PtError> {
        let p = self.need(offset, 2)?;
        Ok(match p[1] {
            0x82 => {
                let p = self.need(offset, 16)?;
                if p != PSB_BYTES {
                    return Err(PtError::MalformedPsb { offset });
                }
                self.last_ip = 0;
                (PtPacket::Psb, 16)
            }
            0x23 => (PtPacket::PsbEnd, 2),
            0xf3 => (PtPacket::Ovf, 2),
            0x83 => (PtPacket::TraceStop, 2),
            0xa3 => {
                let p = self.need(offset, 8)?;
                let payload = Self::le(&p[2..8]);
                if payload == 0 {
                    return Err(PtError::EmptyTnt { offset });
                }
                let stop = 63 - payload.leading_zeros() as u8;
                if stop == 0 {
                    return Err(PtError::EmptyTnt { offset });
                }
                let tnt = Tnt {
                    bits: payload & ((1u64 << stop) - 1),
                    count: stop,
                };
                (PtPacket::Tnt { tnt, long: true }, 8)
            }
            0x43 => {
                let p = self.need(offset, 8)?;
                let payload = Self::le(&p[2..8]);
                (
                    PtPacket::Pip {
                        cr3: (payload & !1) << 4,
                        non_root: payload & 1 == 1,
                    },
                    8,
                )
            }
            0x03 => {
                let p = self.need(offset, 4)?;
                if p[3] != 0 {
                    return Err(PtError::ReservedBits { offset });
                }
                (PtPacket::Cbr { ratio: p[2] }, 4)
            }
            0x73 => {
                let p = self.need(offset, 7)?;
                if p[4] != 0 || p[6] & !1 != 0 {
                    return Err(PtError::ReservedBits { offset });
                }
                (
                    PtPacket::Tma {
                        ctc: u16::from_le_bytes([p[2], p[3]]),
                        fast_counter: u16::from(p[5]) | (u16::from(p[6] & 1) << 8),
                    },
                    7,
                )
            }
            opcode => return Err(PtError::UnknownExtended { offset, opcode }),
        })
    }

    fn decode_cyc(&self, offset: usize) -> Result<(PtPacket, usize), PtError> {
        let b0 = self.bytes[offset];
        let mut cycles = u64::from(b0 >> 3);
        let mut shift = 5u32;
        let mut len = 1usize;
        let mut more = b0 & 0b100 != 0;
        while more {
            let p = self.need(offset, len + 1)?;
            let b = p[len];
            let chunk = u64::from(b >> 1);
            if shift >= 64 || (shift > 57 && chunk >> (64 - shift) != 0) {
                return Err(PtError::CycOverflow { offset });
            }
            cycles |= chunk << shift;
            shift += 7;
            len += 1;
            more = b & 1 == 1;
        }
        Ok((PtPacket::Cyc { cycles }, len))
    }
}

impl Iterator for PtDecoder<'_> {
    type Item = Result<PtPacketAt, PtError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.bytes.len() {
            return None;
        }
        let result = self.decode_one();
        match &result {
            Ok(p) => {
                if !self.synced {
                    match p.packet {
                        PtPacket::Pad => {}
                        PtPacket::Psb => self.synced = true,
                        _ => {
                            self.failed = true;
                            return Some(Err(PtError::NotSynchronized { offset: p.offset }));
                        }
                    }
                }
                self.pos += p.len;
            }
            Err(_) => self.failed = true,
        }
        Some(result)
    }
}

/// Decode a whole buffer that starts at a PSB.
pub fn decode_packets(bytes: &[u8]) -> Result<Vec<PtPacketAt>, PtError> {
    PtDecoder::new(bytes).collect()
}
