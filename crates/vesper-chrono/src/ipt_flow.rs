//! Intel PT control-flow reconstruction over a code image.
//!
//! Walks the instructions of a caller-provided code image (decoded with
//! `iced-x86`), consuming TNT bits at conditional branches and TIP packets at
//! indirect branches, returns and far transfers, to turn a packet stream into
//! an instruction-level branch trace. Supported:
//!
//! * TIP.PGE / TIP.PGD enable and disable, PSB+ FUP synchronisation;
//! * return compression (a taken TNT bit for a `ret` whose target is the top
//!   of the call stack), and uncompressed returns (a TIP);
//! * asynchronous events: a FUP naming the current IP followed by a TIP (the
//!   handler target) or TIP.PGD;
//! * OVF: the call stack is dropped and flow resumes at the next FUP or
//!   TIP.PGE;
//! * MODE.Exec, which selects 16/32/64-bit instruction decoding.
//!
//! Each branch is stamped with the TSC interval it lies in: the last TSC
//! packet before it and the next one after it. TSX (MODE.TSX transactions)
//! and `xbegin`/`xabort`/`xend` are rejected as unsupported. Mismatches
//! between packets and code (a TIP where a TNT bit is needed, an IP outside
//! the image, an undecodable instruction) are typed errors.

use std::collections::VecDeque;
use std::fmt;

use iced_x86::{Decoder, DecoderOptions, FlowControl, Mnemonic};

use crate::ipt::{IpPacketKind, PtPacket, PtPacketAt};

/// A contiguous mapped code region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeSegment<'a> {
    pub base: u64,
    pub bytes: &'a [u8],
}

/// The code the trace ran through (one or more non-overlapping segments).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeImage<'a> {
    segments: Vec<CodeSegment<'a>>,
}

impl<'a> CodeImage<'a> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn single(base: u64, bytes: &'a [u8]) -> Self {
        Self {
            segments: vec![CodeSegment { base, bytes }],
        }
    }
    /// Add a segment; overlapping or address-overflowing segments are rejected.
    pub fn add(&mut self, base: u64, bytes: &'a [u8]) -> Result<(), FlowError> {
        let end = base
            .checked_add(bytes.len() as u64)
            .ok_or(FlowError::BadSegment { base })?;
        if self
            .segments
            .iter()
            .any(|s| base < s.base + s.bytes.len() as u64 && s.base < end)
        {
            return Err(FlowError::BadSegment { base });
        }
        self.segments.push(CodeSegment { base, bytes });
        self.segments.sort_by_key(|s| s.base);
        Ok(())
    }
    /// The bytes from `ip` to the end of its segment.
    pub fn at(&self, ip: u64) -> Option<&'a [u8]> {
        self.segments.iter().find_map(|s| {
            let off = ip.checked_sub(s.base)?;
            (off < s.bytes.len() as u64).then(|| &s.bytes[off as usize..])
        })
    }
}

/// What kind of control transfer a [`Branch`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BranchKind {
    /// A conditional branch (TNT bit).
    Conditional {
        taken: bool,
    },
    DirectJump,
    DirectCall,
    IndirectJump,
    IndirectCall,
    /// A return; `compressed` when answered by a TNT bit.
    Return {
        compressed: bool,
    },
    /// Far transfer, software interrupt, system call or exception.
    Far,
    /// Asynchronous event (FUP + TIP).
    Async,
    /// Tracing enabled at `to` (TIP.PGE).
    Enable,
    /// Tracing disabled after `from` (TIP.PGD); `to` is the target if known.
    Disable,
}

/// One reconstructed control transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Branch {
    /// Address of the branching instruction (for `Enable`, the enable IP).
    pub from: u64,
    /// Where execution continued; `None` when tracing stopped without a
    /// target.
    pub to: Option<u64>,
    pub kind: BranchKind,
    /// Last TSC packet value before this branch (the branch ran at or after it).
    pub tsc_lo: Option<u64>,
    /// First TSC packet value after this branch (the branch ran before it).
    pub tsc_hi: Option<u64>,
    /// Index of the packet that resolved this branch.
    pub packet_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowLimits {
    /// Maximum instructions walked (straight-line code included).
    pub max_instructions: u64,
    /// Maximum return-stack depth kept for return compression.
    pub max_call_depth: usize,
}

impl Default for FlowLimits {
    fn default() -> Self {
        Self {
            max_instructions: 10_000_000,
            max_call_depth: 64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowError {
    BadSegment {
        base: u64,
    },
    /// Execution reached an address outside the image.
    IpOutsideImage {
        ip: u64,
        packet_index: usize,
    },
    /// The bytes at `ip` do not decode as an instruction.
    BadInstruction {
        ip: u64,
    },
    /// The instruction at `ip` needs a TNT bit; packet `found` is something else.
    ExpectedTnt {
        ip: u64,
        found: Option<usize>,
    },
    /// The instruction at `ip` needs a TIP; packet `found` is something else.
    ExpectedTip {
        ip: u64,
        found: Option<usize>,
    },
    /// A compressed return (taken TNT bit) with an empty call stack, or a
    /// not-taken bit for a return.
    BadCompressedReturn {
        ip: u64,
    },
    /// A packet that cannot occur in the current state.
    UnexpectedPacket {
        packet_index: usize,
    },
    /// A PSB+ FUP names a different IP than the reconstructed one.
    Desynchronized {
        expected: u64,
        found: u64,
        packet_index: usize,
    },
    /// A TIP carries no IP where a target is needed.
    SuppressedTarget {
        packet_index: usize,
    },
    /// TSX or another feature this reconstructor does not model.
    Unsupported {
        ip: u64,
        what: &'static str,
    },
    InstructionBudget {
        limit: u64,
    },
    /// TNT bits left over at the end of the packets; the code image does not
    /// match the trace.
    UnusedTnt {
        bits: usize,
    },
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "intel pt flow: {self:?}")
    }
}
impl std::error::Error for FlowError {}

/// A reconstructed trace.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlowTrace {
    pub branches: Vec<Branch>,
    /// Instructions walked (each executed instruction once).
    pub instructions: u64,
    /// Where reconstruction stopped (more packets were needed), if tracing
    /// was still enabled.
    pub stopped_at: Option<u64>,
    /// OVF packets seen (flow before each was lost).
    pub overflows: u32,
}

struct Walker<'p, 'i> {
    packets: &'p [PtPacketAt],
    next: usize,
    image: &'p CodeImage<'i>,
    limits: FlowLimits,
    tnt: VecDeque<(bool, usize)>,
    calls: Vec<u64>,
    bitness: u32,
    tsc: Option<u64>,
    trace: FlowTrace,
    /// Index into `trace.branches` of the first branch not yet given a
    /// following TSC.
    open_tsc_from: usize,
}

impl Walker<'_, '_> {
    fn note_tsc(&mut self, tsc: u64) {
        for b in &mut self.trace.branches[self.open_tsc_from..] {
            b.tsc_hi = Some(tsc);
        }
        self.open_tsc_from = self.trace.branches.len();
        self.tsc = Some(tsc);
    }

    fn push(&mut self, from: u64, to: Option<u64>, kind: BranchKind, packet_index: usize) {
        self.trace.branches.push(Branch {
            from,
            to,
            kind,
            tsc_lo: self.tsc,
            tsc_hi: None,
            packet_index,
        });
    }

    /// Apply packet `i` if it only updates timing, mode or paging state.
    fn bookkeep(&mut self, i: usize) -> Result<bool, FlowError> {
        match self.packets[i].packet {
            PtPacket::Tsc { tsc } => self.note_tsc(tsc),
            PtPacket::ModeExec { cs_l, cs_d } => {
                self.bitness = if cs_l {
                    64
                } else if cs_d {
                    32
                } else {
                    16
                }
            }
            PtPacket::ModeTsx { in_tx, tx_abort } => {
                if in_tx || tx_abort {
                    return Err(FlowError::Unsupported {
                        ip: 0,
                        what: "TSX transactions",
                    });
                }
            }
            PtPacket::Pip { .. } | PtPacket::PsbEnd => {}
            ref t if t.is_timing() => {}
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Consume bookkeeping packets; return the index of the next
    /// flow-relevant one (without consuming it).
    fn peek_flow(&mut self) -> Result<Option<usize>, FlowError> {
        while self.next < self.packets.len() {
            if !self.bookkeep(self.next)? {
                return Ok(Some(self.next));
            }
            self.next += 1;
        }
        Ok(None)
    }

    /// Next TNT bit, refilling from packets.
    fn take_tnt(&mut self, ip: u64) -> Result<Option<(bool, usize)>, FlowError> {
        if let Some(bit) = self.tnt.pop_front() {
            return Ok(Some(bit));
        }
        match self.peek_flow()? {
            None => Ok(None),
            Some(i) => match self.packets[i].packet {
                PtPacket::Tnt { tnt, .. } => {
                    self.next += 1;
                    self.tnt.extend(tnt.iter().map(|t| (t, i)));
                    Ok(self.tnt.pop_front())
                }
                _ => Err(FlowError::ExpectedTnt { ip, found: Some(i) }),
            },
        }
    }

    /// Next TIP or TIP.PGD (for indirect transfers).
    fn take_tip(
        &mut self,
        ip: u64,
    ) -> Result<Option<(IpPacketKind, Option<u64>, usize)>, FlowError> {
        if !self.tnt.is_empty() {
            return Err(FlowError::ExpectedTip {
                ip,
                found: Some(self.tnt[0].1),
            });
        }
        match self.peek_flow()? {
            None => Ok(None),
            Some(i) => match self.packets[i].packet {
                PtPacket::Ip {
                    kind: kind @ (IpPacketKind::Tip | IpPacketKind::TipPgd),
                    ip: target,
                    ..
                } => {
                    self.next += 1;
                    Ok(Some((kind, target, i)))
                }
                _ => Err(FlowError::ExpectedTip { ip, found: Some(i) }),
            },
        }
    }

    /// While tracing is disabled or unsynchronised: find where it resumes.
    fn resume_point(&mut self) -> Result<Option<u64>, FlowError> {
        loop {
            let Some(i) = self.peek_flow()? else {
                return Ok(None);
            };
            self.next += 1;
            match self.packets[i].packet {
                PtPacket::Psb | PtPacket::Ovf | PtPacket::TraceStop => {
                    if self.packets[i].packet == PtPacket::Ovf {
                        self.trace.overflows += 1;
                        self.calls.clear();
                        self.tnt.clear();
                    }
                }
                PtPacket::Ip {
                    kind: IpPacketKind::TipPge,
                    ip: Some(ip),
                    ..
                } => {
                    self.push(ip, Some(ip), BranchKind::Enable, i);
                    return Ok(Some(ip));
                }
                PtPacket::Ip {
                    kind: IpPacketKind::Fup,
                    ip: Some(ip),
                    ..
                } => return Ok(Some(ip)),
                // A disable while already disabled carries no flow.
                PtPacket::Ip {
                    kind: IpPacketKind::TipPgd,
                    ..
                } => {}
                _ => return Err(FlowError::UnexpectedPacket { packet_index: i }),
            }
        }
    }

    /// Handle packets that can interrupt straight-line flow at `ip`: PSB+
    /// resynchronisation, asynchronous FUP+TIP, OVF and a TIP.PGD naming
    /// the current IP (tracing filtered out at a branch target). Returns the new IP (or `None` when tracing stopped);
    /// `Ok(Some(ip))` unchanged when nothing applies.
    fn interrupts(&mut self, ip: u64) -> Result<Option<u64>, FlowError> {
        if !self.tnt.is_empty() {
            return Ok(Some(ip));
        }
        loop {
            let Some(i) = self.peek_flow()? else {
                return Ok(Some(ip));
            };
            match self.packets[i].packet {
                PtPacket::Psb => {
                    self.next += 1;
                    // PSB+ up to PSBEND: an optional FUP restates the current IP.
                    while let Some(p) = self.packets.get(self.next) {
                        let j = self.next;
                        match p.packet {
                            PtPacket::PsbEnd => {
                                self.next += 1;
                                break;
                            }
                            PtPacket::Ip {
                                kind: IpPacketKind::Fup,
                                ip: found,
                                ..
                            } => {
                                if let Some(found) = found
                                    && found != ip
                                {
                                    return Err(FlowError::Desynchronized {
                                        expected: ip,
                                        found,
                                        packet_index: j,
                                    });
                                }
                            }
                            _ => {
                                if !self.bookkeep(j)? {
                                    return Err(FlowError::UnexpectedPacket { packet_index: j });
                                }
                            }
                        }
                        self.next += 1;
                    }
                }
                PtPacket::Ip {
                    kind: IpPacketKind::Fup,
                    ip: Some(at),
                    ..
                } if at == ip => {
                    self.next += 1;
                    match self.take_tip(ip)? {
                        Some((IpPacketKind::Tip, Some(to), j)) => {
                            self.push(ip, Some(to), BranchKind::Async, j);
                            return Ok(Some(to));
                        }
                        Some((IpPacketKind::TipPgd, to, j)) => {
                            self.push(ip, to, BranchKind::Disable, j);
                            return self.resume_point();
                        }
                        Some((_, None, j)) => {
                            return Err(FlowError::SuppressedTarget { packet_index: j });
                        }
                        Some((_, _, j)) => {
                            return Err(FlowError::UnexpectedPacket { packet_index: j });
                        }
                        None => return Ok(None),
                    }
                }
                PtPacket::Ip {
                    kind: IpPacketKind::TipPgd,
                    ip: to,
                    ..
                } if to == Some(ip) => {
                    self.next += 1;
                    self.push(ip, to, BranchKind::Disable, i);
                    return self.resume_point();
                }
                PtPacket::Ovf => {
                    self.next += 1;
                    self.trace.overflows += 1;
                    self.calls.clear();
                    return self.resume_point();
                }
                _ => return Ok(Some(ip)),
            }
        }
    }

    fn transfer(
        &mut self,
        ip: u64,
        kind: BranchKind,
        tip: Option<(IpPacketKind, Option<u64>, usize)>,
    ) -> Result<Option<u64>, FlowError> {
        match tip {
            None => {
                self.trace.stopped_at = Some(ip);
                Ok(None)
            }
            Some((IpPacketKind::Tip, Some(to), i)) => {
                self.push(ip, Some(to), kind, i);
                Ok(Some(to))
            }
            Some((IpPacketKind::Tip, None, i)) => {
                Err(FlowError::SuppressedTarget { packet_index: i })
            }
            Some((_, to, i)) => {
                self.push(ip, to, BranchKind::Disable, i);
                self.resume_point()
            }
        }
    }

    fn call_push(&mut self, ret: u64) {
        if self.calls.len() == self.limits.max_call_depth {
            self.calls.remove(0);
        }
        self.calls.push(ret);
    }

    fn run(mut self) -> Result<FlowTrace, FlowError> {
        let mut ip = self.resume_point()?;
        'walk: while let Some(mut cur) = ip {
            // Apply every interruption at this IP (an async event may land
            // where an overflow or another event follows).
            loop {
                match self.interrupts(cur)? {
                    None => break 'walk,
                    Some(next) if next == cur => break,
                    Some(next) => cur = next,
                }
            }
            if self.trace.instructions == self.limits.max_instructions {
                return Err(FlowError::InstructionBudget {
                    limit: self.limits.max_instructions,
                });
            }
            let bytes = self.image.at(cur).ok_or(FlowError::IpOutsideImage {
                ip: cur,
                packet_index: self.next,
            })?;
            let mut decoder = Decoder::with_ip(self.bitness, bytes, cur, DecoderOptions::NONE);
            let insn = decoder.decode();
            if insn.is_invalid() {
                return Err(FlowError::BadInstruction { ip: cur });
            }
            self.trace.instructions += 1;
            let next_ip = insn.next_ip();
            let far = insn.is_jmp_far()
                || insn.is_call_far()
                || insn.is_jmp_far_indirect()
                || insn.is_call_far_indirect()
                || matches!(
                    insn.mnemonic(),
                    Mnemonic::Retf
                        | Mnemonic::Iret
                        | Mnemonic::Iretd
                        | Mnemonic::Iretq
                        | Mnemonic::Syscall
                        | Mnemonic::Sysret
                        | Mnemonic::Sysretq
                        | Mnemonic::Sysenter
                        | Mnemonic::Sysexit
                        | Mnemonic::Sysexitq
                );
            ip = if far {
                let tip = self.take_tip(cur)?;
                self.transfer(cur, BranchKind::Far, tip)?
            } else {
                match insn.flow_control() {
                    FlowControl::Next => Some(next_ip),
                    FlowControl::UnconditionalBranch => {
                        let to = insn.near_branch_target();
                        self.push(cur, Some(to), BranchKind::DirectJump, self.next);
                        Some(to)
                    }
                    FlowControl::Call => {
                        let to = insn.near_branch_target();
                        self.call_push(next_ip);
                        self.push(cur, Some(to), BranchKind::DirectCall, self.next);
                        Some(to)
                    }
                    FlowControl::ConditionalBranch => match self.take_tnt(cur)? {
                        None => {
                            self.trace.stopped_at = Some(cur);
                            None
                        }
                        Some((taken, i)) => {
                            let to = if taken {
                                insn.near_branch_target()
                            } else {
                                next_ip
                            };
                            self.push(cur, Some(to), BranchKind::Conditional { taken }, i);
                            Some(to)
                        }
                    },
                    FlowControl::IndirectBranch => {
                        let tip = self.take_tip(cur)?;
                        self.transfer(cur, BranchKind::IndirectJump, tip)?
                    }
                    FlowControl::IndirectCall => {
                        let tip = self.take_tip(cur)?;
                        if tip.is_some() {
                            self.call_push(next_ip);
                        }
                        self.transfer(cur, BranchKind::IndirectCall, tip)?
                    }
                    FlowControl::Return => {
                        // A pending or next TNT bit answers a compressed return.
                        let compressed = !self.tnt.is_empty()
                            || matches!(
                                self.peek_flow()?.map(|i| self.packets[i].packet),
                                Some(PtPacket::Tnt { .. })
                            );
                        if compressed {
                            let (taken, i) = self
                                .take_tnt(cur)?
                                .ok_or(FlowError::BadCompressedReturn { ip: cur })?;
                            let to = self.calls.pop();
                            match (taken, to) {
                                (true, Some(to)) => {
                                    self.push(
                                        cur,
                                        Some(to),
                                        BranchKind::Return { compressed: true },
                                        i,
                                    );
                                    Some(to)
                                }
                                _ => return Err(FlowError::BadCompressedReturn { ip: cur }),
                            }
                        } else {
                            let tip = self.take_tip(cur)?;
                            if tip.is_some() {
                                self.calls.pop();
                            }
                            self.transfer(cur, BranchKind::Return { compressed: false }, tip)?
                        }
                    }
                    FlowControl::Interrupt | FlowControl::Exception => {
                        let tip = self.take_tip(cur)?;
                        self.transfer(cur, BranchKind::Far, tip)?
                    }
                    FlowControl::XbeginXabortXend => {
                        return Err(FlowError::Unsupported {
                            ip: cur,
                            what: "xbegin/xabort/xend",
                        });
                    }
                }
            };
        }
        // Drain trailing bookkeeping so a final TSC closes open intervals.
        while self
            .peek_flow()?
            .is_some_and(|i| matches!(self.packets[i].packet, PtPacket::Psb | PtPacket::TraceStop))
        {
            self.next += 1;
        }
        if !self.tnt.is_empty() {
            return Err(FlowError::UnusedTnt {
                bits: self.tnt.len(),
            });
        }
        Ok(self.trace)
    }
}

/// Reconstruct the branch trace of `packets` over `image`.
///
/// Reconstruction starts at the first TIP.PGE or PSB+ FUP and stops when
/// the packets run out at an instruction that needs one.
pub fn reconstruct(
    packets: &[PtPacketAt],
    image: &CodeImage<'_>,
    limits: FlowLimits,
) -> Result<FlowTrace, FlowError> {
    Walker {
        packets,
        next: 0,
        image,
        limits,
        tnt: VecDeque::new(),
        calls: Vec::new(),
        bitness: 64,
        tsc: None,
        trace: FlowTrace::default(),
        open_tsc_from: 0,
    }
    .run()
}
