//! Arm CoreSight ETE / ETMv4 instruction-trace element decoder (§31.1 item 3).
//!
//! Decodes the instruction-trace subset of the ETMv4 / ETE trace stream
//! (Arm IHI 0064 "ETM Architecture Specification ETMv4", Arm IHI 0101 "ETE")
//! into trace elements:
//!
//! | Header | Packet | Element |
//! | --- | --- | --- |
//! | `00 ×11, 80` | A-Sync | [`EteElement::Sync`] |
//! | `00 03`, `00 05` | Discard, Overflow | [`EteElement::Discard`], [`EteElement::Overflow`] |
//! | `01` | Trace Info | [`EteElement::TraceInfo`] |
//! | `02`, `03` | Timestamp (`03`: with cycle count) | [`EteElement::Timestamp`] |
//! | `04` | Trace On | [`EteElement::TraceOn`] |
//! | `06` + address | Exception | [`EteElement::Exception`] |
//! | `07` | Exception Return | [`EteElement::ExceptionReturn`] |
//! | `70`, `71`-`7F` | Ignore, Event | dropped, [`EteElement::Event`] |
//! | `80`, `81` | Context (same, new) | [`EteElement::Context`] |
//! | `82` `83` `85` `86` | Address with context | [`EteElement::Context`] + [`EteElement::Address`] |
//! | `90`-`92` | Exact-match address | [`EteElement::Address`] |
//! | `95`, `96` | Short address IS0 / IS1 | [`EteElement::Address`] |
//! | `9A` `9B` `9D` `9E` | Long address (32/64-bit, IS0/IS1) | [`EteElement::Address`] |
//! | `C0`-`FF` (atom ranges) | Atom formats 1-6 | one [`EteElement::Atom`] per atom |
//!
//! The decoder keeps the three-entry address history used by exact-match
//! packets and the previous timestamp and context, which later packets
//! update in part. Cycle-count, conditional, commit, speculation and data
//! packets (used only when those trace features are enabled) are
//! [`EteError::Unsupported`]. Decoding requires an A-Sync first.
//!
//! The byte encodings follow this module's reading of the specifications;
//! they are tested against streams from an independently written test
//! encoder, never against a trace captured on Arm hardware.

use std::fmt;

/// Instruction set state of an address: IS0 is A64 (or A32), IS1 is T32.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstructionSet {
    Is0,
    Is1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EteContext {
    /// Exception level 0-3.
    pub el: u8,
    /// AArch64 (`true`) or AArch32.
    pub sixty_four: bool,
    /// Non-secure state.
    pub non_secure: bool,
    pub vmid: Option<u32>,
    pub context_id: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EteElement {
    Sync,
    TraceInfo {
        info: u64,
        key: u64,
        spec: u64,
        cyct: u64,
    },
    TraceOn,
    /// Full reconstructed timestamp and the cycle count, if present.
    Timestamp {
        timestamp: u64,
        cycle_count: Option<u64>,
    },
    Context(EteContext),
    Address {
        address: u64,
        isa: InstructionSet,
    },
    /// One instruction-trace atom: `executed` is E (branch taken / condition
    /// passed), otherwise N.
    Atom {
        executed: bool,
    },
    /// An exception; the preferred return address comes from the address
    /// packet that follows the exception header.
    Exception {
        exception_type: u16,
        preferred_return: u64,
        isa: InstructionSet,
    },
    ExceptionReturn,
    Event {
        mask: u8,
    },
    Overflow,
    Discard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EteError {
    Truncated {
        offset: usize,
        needed: usize,
        available: usize,
    },
    /// The stream does not start with A-Sync.
    NotSynchronized { offset: usize },
    /// No A-Sync anywhere in the buffer.
    NoSync,
    /// A run of zero bytes that is not a valid A-Sync or extension packet.
    MalformedExtension { offset: usize },
    /// A header of a packet kind this decoder does not implement.
    Unsupported { offset: usize, header: u8 },
    /// A reserved header value.
    Reserved { offset: usize, header: u8 },
    /// A continuation-encoded field that does not terminate within its
    /// maximum length.
    FieldTooLong { offset: usize },
    /// An exception header not followed by an address packet.
    ExceptionWithoutAddress { offset: usize },
    /// An exact-match packet naming an address register never written.
    EmptyAddressHistory { offset: usize },
}

impl fmt::Display for EteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "etm/ete: {self:?}")
    }
}
impl std::error::Error for EteError {}

/// An element and the offset of the packet that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EteElementAt {
    pub offset: usize,
    pub element: EteElement,
}

/// Length of an A-Sync packet.
pub const ASYNC_LEN: usize = 12;

#[derive(Debug, Clone)]
pub struct EteDecoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    synced: bool,
    failed: bool,
    /// Most recent first.
    history: [Option<(u64, InstructionSet)>; 3],
    timestamp: u64,
    context: EteContext,
    pending: std::collections::VecDeque<EteElementAt>,
}

fn is_async(bytes: &[u8]) -> bool {
    bytes.len() >= ASYNC_LEN && bytes[..11].iter().all(|b| *b == 0) && bytes[11] == 0x80
}

impl<'a> EteDecoder<'a> {
    /// Decode from offset 0, which must hold an A-Sync.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            synced: false,
            failed: false,
            history: [None; 3],
            timestamp: 0,
            context: EteContext::default(),
            pending: Default::default(),
        }
    }

    /// Start at the first A-Sync in `bytes`.
    pub fn sync(bytes: &'a [u8]) -> Result<Self, EteError> {
        let at = (0..bytes.len())
            .find(|i| is_async(&bytes[*i..]))
            .ok_or(EteError::NoSync)?;
        let mut d = Self::new(bytes);
        d.pos = at;
        Ok(d)
    }

    fn need(&self, offset: usize, needed: usize) -> Result<&'a [u8], EteError> {
        let available = self.bytes.len().saturating_sub(offset);
        if available < needed {
            return Err(EteError::Truncated {
                offset,
                needed,
                available,
            });
        }
        Ok(&self.bytes[offset..offset + needed])
    }

    /// Continuation-encoded unsigned field (bit 7 = more), at most `max`
    /// bytes. Returns (value, bytes used).
    fn uleb(&self, at: usize, max: usize, offset: usize) -> Result<(u64, usize), EteError> {
        let mut value = 0u64;
        for i in 0..max {
            let b = self.need(at, i + 1)?[i];
            value |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok((value, i + 1));
            }
        }
        Err(EteError::FieldTooLong { offset })
    }

    fn push_address(&mut self, address: u64, isa: InstructionSet) {
        self.history = [Some((address, isa)), self.history[0], self.history[1]];
    }

    fn latest(&self) -> u64 {
        self.history[0].map_or(0, |(a, _)| a)
    }

    /// Decode the address payload of header `h` at `at`; returns the length.
    fn address(
        &mut self,
        h: u8,
        at: usize,
        offset: usize,
    ) -> Result<(u64, InstructionSet, usize), EteError> {
        let prev = self.latest();
        let (isa, len, addr) = match h {
            // Short address: one byte, or two when bit 7 continues.
            0x95 | 0x96 => {
                let b1 = self.need(at, 1)?[0];
                let two = b1 & 0x80 != 0;
                let b2 = if two { self.need(at, 2)?[1] } else { 0 };
                if h == 0x95 {
                    let (mask, low) = if two {
                        (0x1_fffc, (u64::from(b1 & 0x7f) << 2) | (u64::from(b2) << 9))
                    } else {
                        (0x1fc, u64::from(b1 & 0x7f) << 2)
                    };
                    (
                        InstructionSet::Is0,
                        1 + usize::from(two),
                        (prev & !mask) | low,
                    )
                } else {
                    let (mask, low) = if two {
                        (0xfffe, (u64::from(b1 & 0x7f) << 1) | (u64::from(b2) << 8))
                    } else {
                        (0xfe, u64::from(b1 & 0x7f) << 1)
                    };
                    (
                        InstructionSet::Is1,
                        1 + usize::from(two),
                        (prev & !mask) | low,
                    )
                }
            }
            0x9a | 0x82 | 0x9d | 0x85 => {
                let wide = matches!(h, 0x9d | 0x85);
                let n = if wide { 8 } else { 4 };
                let p = self.need(at, n)?;
                let mut a = (u64::from(p[0] & 0x7f) << 2) | (u64::from(p[1] & 0x7f) << 9);
                for (i, b) in p.iter().enumerate().skip(2) {
                    a |= u64::from(*b) << (8 * i);
                }
                let addr = if wide { a } else { (prev & !0xffff_ffff) | a };
                (InstructionSet::Is0, n, addr)
            }
            0x9b | 0x83 | 0x9e | 0x86 => {
                let wide = matches!(h, 0x9e | 0x86);
                let n = if wide { 8 } else { 4 };
                let p = self.need(at, n)?;
                let mut a = u64::from(p[0] & 0x7f) << 1;
                for (i, b) in p.iter().enumerate().skip(1) {
                    a |= u64::from(*b) << (8 * i);
                }
                let addr = if wide { a } else { (prev & !0xffff_ffff) | a };
                (InstructionSet::Is1, n, addr)
            }
            0x90..=0x92 => {
                let (a, isa) = self.history[usize::from(h - 0x90)]
                    .ok_or(EteError::EmptyAddressHistory { offset })?;
                return {
                    // Exact match re-pushes the matched entry.
                    self.push_address(a, isa);
                    Ok((a, isa, 0))
                };
            }
            _ => unreachable!("caller passes only address headers"),
        };
        self.push_address(addr, isa);
        Ok((addr, isa, len))
    }

    /// Context info payload (after `81`, or after the address of an
    /// address-with-context packet).
    fn context_info(&mut self, at: usize) -> Result<usize, EteError> {
        let info = self.need(at, 1)?[0];
        let mut len = 1;
        let mut ctx = EteContext {
            el: info & 0b11,
            sixty_four: info & 0x10 != 0,
            non_secure: info & 0x20 != 0,
            vmid: self.context.vmid,
            context_id: self.context.context_id,
        };
        if info & 0x40 != 0 {
            ctx.vmid = Some(u32::from(self.need(at, len + 1)?[len]));
            len += 1;
        }
        if info & 0x80 != 0 {
            let p = self.need(at, len + 4)?;
            ctx.context_id = Some(u32::from_le_bytes([
                p[len],
                p[len + 1],
                p[len + 2],
                p[len + 3],
            ]));
            len += 4;
        }
        self.context = ctx;
        Ok(len)
    }

    fn atoms(&mut self, offset: usize, pattern: u32, count: u32) {
        for i in 0..count {
            self.pending.push_back(EteElementAt {
                offset,
                element: EteElement::Atom {
                    executed: (pattern >> i) & 1 == 1,
                },
            });
        }
    }

    fn emit(&mut self, offset: usize, element: EteElement) {
        self.pending.push_back(EteElementAt { offset, element });
    }

    /// Decode one packet, queueing its elements; returns its length.
    fn packet(&mut self) -> Result<usize, EteError> {
        let offset = self.pos;
        let h = self.bytes[offset];
        let at = offset + 1;
        if !self.synced {
            if is_async(&self.bytes[offset..]) {
                self.synced = true;
                self.emit(offset, EteElement::Sync);
                return Ok(ASYNC_LEN);
            }
            return Err(EteError::NotSynchronized { offset });
        }
        Ok(match h {
            0x00 => {
                if is_async(&self.bytes[offset..]) {
                    self.emit(offset, EteElement::Sync);
                    ASYNC_LEN
                } else {
                    match self.need(offset, 2)?[1] {
                        0x03 => {
                            self.emit(offset, EteElement::Discard);
                            2
                        }
                        0x05 => {
                            self.emit(offset, EteElement::Overflow);
                            2
                        }
                        0x00 => {
                            // A run of zeros must complete an A-Sync.
                            self.need(offset, ASYNC_LEN)?;
                            return Err(EteError::MalformedExtension { offset });
                        }
                        _ => return Err(EteError::MalformedExtension { offset }),
                    }
                }
            }
            0x01 => {
                let (plctl, mut len) = self.uleb(at, 1, offset)?;
                let mut fields = [0u64; 4];
                for (bit, field) in fields.iter_mut().enumerate() {
                    if plctl & (1 << bit) != 0 {
                        let (v, n) = self.uleb(at + len, 5, offset)?;
                        *field = v;
                        len += n;
                    }
                }
                self.emit(
                    offset,
                    EteElement::TraceInfo {
                        info: fields[0],
                        key: fields[1],
                        spec: fields[2],
                        cyct: fields[3],
                    },
                );
                1 + len
            }
            0x02 | 0x03 => {
                // Up to 8 continuation bytes of 7 bits, then a full 9th byte.
                let mut value = 0u64;
                let mut len = 0usize;
                let mut bits = 0u32;
                loop {
                    let b = self.need(at, len + 1)?[len];
                    len += 1;
                    if len == 9 {
                        value |= u64::from(b) << 56;
                        bits = 64;
                        break;
                    }
                    value |= u64::from(b & 0x7f) << bits;
                    bits += 7;
                    if b & 0x80 == 0 {
                        break;
                    }
                }
                self.timestamp = if bits >= 64 {
                    value
                } else {
                    (self.timestamp & !((1u64 << bits) - 1)) | value
                };
                let cycle_count = if h == 0x03 {
                    let (cc, n) = self.uleb(at + len, 3, offset)?;
                    len += n;
                    Some(cc)
                } else {
                    None
                };
                self.emit(
                    offset,
                    EteElement::Timestamp {
                        timestamp: self.timestamp,
                        cycle_count,
                    },
                );
                1 + len
            }
            0x04 => {
                self.emit(offset, EteElement::TraceOn);
                1
            }
            0x06 => {
                let b1 = self.need(at, 1)?[0];
                let mut exception_type = u16::from((b1 >> 1) & 0x1f);
                let mut len = 1;
                if b1 & 0x80 != 0 {
                    let b2 = self.need(at, 2)?[1];
                    if b2 & 0x80 != 0 {
                        return Err(EteError::FieldTooLong { offset });
                    }
                    exception_type |= u16::from(b2 & 0x1f) << 5;
                    len = 2;
                }
                let addr_at = at + len;
                let ah = *self
                    .bytes
                    .get(addr_at)
                    .ok_or(EteError::ExceptionWithoutAddress { offset })?;
                let with_context = matches!(ah, 0x82 | 0x83 | 0x85 | 0x86);
                if !matches!(ah, 0x90..=0x92 | 0x95 | 0x96 | 0x9a | 0x9b | 0x9d | 0x9e)
                    && !with_context
                {
                    return Err(EteError::ExceptionWithoutAddress { offset });
                }
                let (preferred_return, isa, n) = self.address(ah, addr_at + 1, offset)?;
                let mut total = len + 1 + n;
                if with_context {
                    total += self.context_info(addr_at + 1 + n)?;
                    let ctx = self.context;
                    self.emit(offset, EteElement::Context(ctx));
                }
                self.emit(
                    offset,
                    EteElement::Exception {
                        exception_type,
                        preferred_return,
                        isa,
                    },
                );
                1 + total
            }
            0x07 => {
                self.emit(offset, EteElement::ExceptionReturn);
                1
            }
            0x70 => 1,
            0x71..=0x7f => {
                self.emit(offset, EteElement::Event { mask: h & 0x0f });
                1
            }
            0x80 => {
                let ctx = self.context;
                self.emit(offset, EteElement::Context(ctx));
                1
            }
            0x81 => {
                let n = self.context_info(at)?;
                let ctx = self.context;
                self.emit(offset, EteElement::Context(ctx));
                1 + n
            }
            0x82 | 0x83 | 0x85 | 0x86 => {
                let (address, isa, n) = self.address(h, at, offset)?;
                let c = self.context_info(at + n)?;
                let ctx = self.context;
                self.emit(offset, EteElement::Context(ctx));
                self.emit(offset, EteElement::Address { address, isa });
                1 + n + c
            }
            0x90..=0x92 | 0x95 | 0x96 | 0x9a | 0x9b | 0x9d | 0x9e => {
                let (address, isa, n) = self.address(h, at, offset)?;
                self.emit(offset, EteElement::Address { address, isa });
                1 + n
            }
            // Format 1: one atom, bit 0.
            0xf6 | 0xf7 => {
                self.atoms(offset, u32::from(h & 1), 1);
                1
            }
            // Format 2: two atoms, bits [1:0] oldest first.
            0xd8..=0xdb => {
                self.atoms(offset, u32::from(h & 0b11), 2);
                1
            }
            // Format 3: three atoms, bits [2:0].
            0xf8..=0xff => {
                self.atoms(offset, u32::from(h & 0b111), 3);
                1
            }
            // Format 4: four atoms from a fixed pattern table.
            0xdc..=0xdf => {
                const F4: [u32; 4] = [0b1110, 0b0000, 0b1010, 0b0101];
                self.atoms(offset, F4[usize::from(h & 0b11)], 4);
                1
            }
            // Format 5: five atoms.
            0xd5..=0xd7 | 0xf5 => {
                let pattern = match h {
                    0xf5 => 0b01111, // EEEEN
                    0xd5 => 0b00000, // NNNNN
                    0xd6 => 0b01010, // NENEN
                    _ => 0b10101,    // ENENE
                };
                self.atoms(offset, pattern, 5);
                1
            }
            // Format 6: COUNT+3 E atoms, then E (A = 0) or N (A = 1).
            0xc0..=0xd4 | 0xe0..=0xf4 => {
                let count = u32::from(h & 0x1f) + 3;
                let last_n = h & 0x20 != 0;
                let pattern = ((1u32 << count) - 1) | if last_n { 0 } else { 1 << count };
                self.atoms(offset, pattern, count + 1);
                1
            }
            0x05
            | 0x08..=0x6f
            | 0x84
            | 0x87..=0x8f
            | 0x93
            | 0x94
            | 0x97..=0x99
            | 0x9c
            | 0xa0..=0xbf => {
                return Err(EteError::Unsupported { offset, header: h });
            }
            _ => return Err(EteError::Reserved { offset, header: h }),
        })
    }
}

impl Iterator for EteDecoder<'_> {
    type Item = Result<EteElementAt, EteError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(e) = self.pending.pop_front() {
                return Some(Ok(e));
            }
            if self.failed || self.pos >= self.bytes.len() {
                return None;
            }
            match self.packet() {
                Ok(len) => self.pos += len,
                Err(e) => {
                    self.failed = true;
                    self.pending.clear();
                    return Some(Err(e));
                }
            }
        }
    }
}

/// Decode a whole buffer that starts with an A-Sync.
pub fn decode_elements(bytes: &[u8]) -> Result<Vec<EteElementAt>, EteError> {
    EteDecoder::new(bytes).collect()
}
