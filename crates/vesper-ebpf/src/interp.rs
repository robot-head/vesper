//! A concrete userspace eBPF interpreter for XDP programs, used to execute
//! verified programs over packet fixtures and to check the verifier: every
//! value carries its provenance, and any access outside the region a
//! pointer was derived from, any read of uninitialised stack, any use of a
//! released reference and any reference still held at exit is an
//! [`InterpError`]. An accepted program that faults here is a verifier bug.
//!
//! Single-CPU model: per-CPU maps hold the value of CPU [`XdpEnv::cpu`].
//! No real clock is read; `bpf_ktime_get_ns` returns [`XdpEnv::now_ns`].

use crate::helpers as h;
use crate::insn::{
    AluOp, AtomicOp, CallTarget, Endian, Imm64, Insn, JmpOp, Operand, Reg, Size, decode_all,
};
use crate::program::{MapDef, MapKind, Program};
use crate::scalar::sext_const;
use crate::verifier::XDP_MD_SIZE;
use std::collections::HashMap;
use std::fmt;

/// Concrete ALU semantics (RFC 9669 §4.1) on 64-bit register values; for
/// 32-bit ops the operands are truncated and the result zero-extended.
pub fn alu_concrete(op: AluOp, wide: bool, a: u64, b: u64) -> u64 {
    if !wide {
        let x = a as u32;
        let y = b as u32;
        let r: u32 = match op {
            AluOp::Add => x.wrapping_add(y),
            AluOp::Sub => x.wrapping_sub(y),
            AluOp::Mul => x.wrapping_mul(y),
            AluOp::Div => x.checked_div(y).unwrap_or(0),
            AluOp::SDiv => {
                if y == 0 {
                    0
                } else {
                    (x as i32).wrapping_div(y as i32) as u32
                }
            }
            AluOp::Mod => x.checked_rem(y).unwrap_or(x),
            AluOp::SMod => {
                if y == 0 {
                    x
                } else {
                    (x as i32).wrapping_rem(y as i32) as u32
                }
            }
            AluOp::Or => x | y,
            AluOp::And => x & y,
            AluOp::Xor => x ^ y,
            AluOp::Lsh => x.wrapping_shl(y & 31),
            AluOp::Rsh => x.wrapping_shr(y & 31),
            AluOp::Arsh => ((x as i32).wrapping_shr(y & 31)) as u32,
            AluOp::Neg => x.wrapping_neg(),
            AluOp::Mov => y,
            AluOp::MovSx(bits) => sext_const(y as u64, bits as u32) as u32,
        };
        return r as u64;
    }
    match op {
        AluOp::Add => a.wrapping_add(b),
        AluOp::Sub => a.wrapping_sub(b),
        AluOp::Mul => a.wrapping_mul(b),
        AluOp::Div => a.checked_div(b).unwrap_or(0),
        AluOp::SDiv => {
            if b == 0 {
                0
            } else {
                (a as i64).wrapping_div(b as i64) as u64
            }
        }
        AluOp::Mod => a.checked_rem(b).unwrap_or(a),
        AluOp::SMod => {
            if b == 0 {
                a
            } else {
                (a as i64).wrapping_rem(b as i64) as u64
            }
        }
        AluOp::Or => a | b,
        AluOp::And => a & b,
        AluOp::Xor => a ^ b,
        AluOp::Lsh => a.wrapping_shl((b & 63) as u32),
        AluOp::Rsh => a.wrapping_shr((b & 63) as u32),
        AluOp::Arsh => ((a as i64).wrapping_shr((b & 63) as u32)) as u64,
        AluOp::Neg => a.wrapping_neg(),
        AluOp::Mov => b,
        AluOp::MovSx(bits) => sext_const(b, bits as u32),
    }
}

/// Concrete conditional-jump semantics.
pub fn cmp_concrete(op: JmpOp, wide: bool, a: u64, b: u64) -> bool {
    let (a, b) = if wide { (a, b) } else { (a as u32 as u64, b as u32 as u64) };
    let (sa, sb) = if wide {
        (a as i64, b as i64)
    } else {
        (a as u32 as i32 as i64, b as u32 as i32 as i64)
    };
    match op {
        JmpOp::Eq => a == b,
        JmpOp::Ne => a != b,
        JmpOp::Gt => a > b,
        JmpOp::Ge => a >= b,
        JmpOp::Lt => a < b,
        JmpOp::Le => a <= b,
        JmpOp::Set => a & b != 0,
        JmpOp::SGt => sa > sb,
        JmpOp::SGe => sa >= sb,
        JmpOp::SLt => sa < sb,
        JmpOp::SLe => sa <= sb,
    }
}

/// Concrete `BPF_END` semantics on a little-endian target.
pub fn endian(kind: Endian, bits: u8, v: u64) -> u64 {
    match (kind, bits) {
        (Endian::ToLe, 16) => v as u16 as u64,
        (Endian::ToLe, 32) => v as u32 as u64,
        (Endian::ToLe, _) => v,
        (_, 16) => (v as u16).swap_bytes() as u64,
        (_, 32) => (v as u32).swap_bytes() as u64,
        (_, _) => v.swap_bytes(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpError {
    Decode(String),
    UninitRegister { pc: usize, reg: u8 },
    InvalidAccess { pc: usize, detail: String },
    UninitStackRead { pc: usize, off: i64 },
    BadHelperArg { pc: usize, detail: String },
    UnsupportedInsn { pc: usize },
    ReferenceLeak { pc: usize, count: usize },
    InsnBudget,
    CallDepth { pc: usize },
    PcOutOfRange { pc: usize },
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for InterpError {}

type RegionId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Value {
    v: u64,
    prov: Option<RegionId>,
}

impl Value {
    fn scalar(v: u64) -> Value {
        Value { v, prov: None }
    }
}

#[derive(Clone, Debug)]
enum Kind {
    Ctx,
    Packet,
    Stack,
    MapValue,
    Ring { map: usize, data_off: usize, hdr_off: usize },
    Socket,
}

#[derive(Clone, Debug)]
struct Region {
    kind: Kind,
    base: u64,
    bytes: Vec<u8>,
    init: Vec<bool>,
    /// Provenance of pointers spilled at aligned offsets.
    spills: HashMap<usize, RegionId>,
    live: bool,
}

/// A listening TCP socket `bpf_sk_lookup_tcp` can find (IPv4 tuple, all
/// fields in network byte order as they appear in `struct bpf_sock_tuple`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListeningSocket {
    pub daddr: [u8; 4],
    pub dport: [u8; 2],
}

/// Concrete state of one map.
#[derive(Clone, Debug)]
pub enum MapState {
    Kv {
        def: MapDef,
        entries: HashMap<Vec<u8>, RegionId>,
    },
    Ring(RingBuf),
}

/// The kernel BPF ring buffer byte layout (`kernel/bpf/ringbuf.c`): each
/// record is an 8-byte header (`len` with `BUSY`/`DISCARD` bits, then the
/// page offset) followed by the payload, padded to 8 bytes.
#[derive(Clone, Debug)]
pub struct RingBuf {
    pub data: Vec<u8>,
    pub producer: u64,
    pub consumer: u64,
}

pub const RINGBUF_BUSY_BIT: u32 = 1 << 31;
pub const RINGBUF_DISCARD_BIT: u32 = 1 << 30;
pub const RINGBUF_HDR_SZ: usize = 8;

impl RingBuf {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            producer: 0,
            consumer: 0,
        }
    }

    fn mask(&self) -> u64 {
        self.data.len() as u64 - 1
    }

    /// Reserve `len` payload bytes; `None` when full.
    fn reserve(&mut self, len: usize) -> Option<(usize, usize)> {
        let total = (len + RINGBUF_HDR_SZ).div_ceil(8) * 8;
        let used = self.producer - self.consumer;
        if used + total as u64 > self.data.len() as u64 {
            return None;
        }
        let hdr = (self.producer & self.mask()) as usize;
        // records never wrap in this model: the kernel maps the data area
        // twice so a record is contiguous; emulate that by refusing.
        if hdr + total > self.data.len() {
            return None;
        }
        let hlen = (len as u32) | RINGBUF_BUSY_BIT;
        self.data[hdr..hdr + 4].copy_from_slice(&hlen.to_le_bytes());
        self.data[hdr + 4..hdr + 8].copy_from_slice(&0u32.to_le_bytes());
        self.producer += total as u64;
        Some((hdr, hdr + RINGBUF_HDR_SZ))
    }

    fn commit(&mut self, hdr: usize, discard: bool) {
        let mut l = u32::from_le_bytes(self.data[hdr..hdr + 4].try_into().expect("4 bytes"));
        l &= !RINGBUF_BUSY_BIT;
        if discard {
            l |= RINGBUF_DISCARD_BIT;
        }
        self.data[hdr..hdr + 4].copy_from_slice(&l.to_le_bytes());
    }
}

/// Error from [`RingBufReader`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RingReadError {
    /// The data area length is not a power of two.
    BadSize,
    /// Positions are inconsistent (consumer ahead of producer, or a record
    /// longer than the data area).
    Corrupt,
}

/// A consumer over the ring's data area: the same code reads this
/// crate's interpreter ring and a kernel ring buffer mapped into memory.
pub struct RingBufReader<'a> {
    data: &'a [u8],
    producer: u64,
    consumer: u64,
}

impl<'a> RingBufReader<'a> {
    pub fn new(data: &'a [u8], consumer: u64, producer: u64) -> Result<Self, RingReadError> {
        if data.is_empty() || !data.len().is_power_of_two() {
            return Err(RingReadError::BadSize);
        }
        if consumer > producer || producer - consumer > data.len() as u64 {
            return Err(RingReadError::Corrupt);
        }
        Ok(Self {
            data,
            producer,
            consumer,
        })
    }

    pub fn consumer(&self) -> u64 {
        self.consumer
    }

    /// Next committed record payload, skipping discarded ones. Stops at a
    /// busy (not yet committed) record.
    pub fn next_record(&mut self) -> Result<Option<Vec<u8>>, RingReadError> {
        let mask = self.data.len() as u64 - 1;
        while self.consumer < self.producer {
            let at = (self.consumer & mask) as usize;
            let mut h = [0u8; 4];
            for (i, b) in h.iter_mut().enumerate() {
                *b = self.data[(at + i) & mask as usize];
            }
            let hl = u32::from_le_bytes(h);
            if hl & RINGBUF_BUSY_BIT != 0 {
                return Ok(None);
            }
            let len = (hl & !(RINGBUF_BUSY_BIT | RINGBUF_DISCARD_BIT)) as usize;
            let total = (len + RINGBUF_HDR_SZ).div_ceil(8) * 8;
            if total as u64 > self.producer - self.consumer {
                return Err(RingReadError::Corrupt);
            }
            let payload: Vec<u8> = (0..len)
                .map(|i| self.data[(at + RINGBUF_HDR_SZ + i) & mask as usize])
                .collect();
            self.consumer += total as u64;
            if hl & RINGBUF_DISCARD_BIT == 0 {
                return Ok(Some(payload));
            }
        }
        Ok(None)
    }
}

/// Everything outside the program: maps, the CPU, the virtual clock and
/// the sockets `bpf_sk_lookup_tcp` can find.
#[derive(Clone, Debug)]
pub struct XdpEnv {
    pub maps: Vec<MapState>,
    pub cpu: u32,
    pub now_ns: u64,
    pub prandom_state: u64,
    pub sockets: Vec<ListeningSocket>,
    pub insn_budget: u64,
    regions: Vec<Region>,
    next_base: u64,
    persistent: usize,
    /// Offset of `data` inside the packet region (headroom before it).
    pkt_start: usize,
}

impl XdpEnv {
    pub fn new(prog: &Program) -> Self {
        let mut env = XdpEnv {
            maps: Vec::new(),
            cpu: 0,
            now_ns: 0,
            prandom_state: 0x5eed,
            sockets: Vec::new(),
            insn_budget: 1_000_000,
            regions: Vec::new(),
            next_base: 0x1000_0000,
            persistent: 0,
            pkt_start: 0,
        };
        for def in &prog.maps {
            let st = match def.kind {
                MapKind::RingBuf => MapState::Ring(RingBuf::new(def.max_entries as usize)),
                _ => MapState::Kv {
                    def: def.clone(),
                    entries: HashMap::new(),
                },
            };
            env.maps.push(st);
        }
        for m in 0..env.maps.len() {
            let MapState::Kv { def, .. } = &env.maps[m] else {
                continue;
            };
            if def.kind.is_array() {
                let (n, vs) = (def.max_entries, def.value_size as usize);
                for k in 0..n {
                    let rid = env.alloc(Kind::MapValue, vec![0; vs], true);
                    if let MapState::Kv { entries, .. } = &mut env.maps[m] {
                        entries.insert(k.to_le_bytes().to_vec(), rid);
                    }
                }
            }
        }
        env.persistent = env.regions.len();
        env
    }

    fn alloc(&mut self, kind: Kind, bytes: Vec<u8>, init: bool) -> RegionId {
        let base = self.next_base;
        self.next_base += (bytes.len() as u64 + 0x1000).div_ceil(0x1000) * 0x1000 + 0x1000;
        let n = bytes.len();
        self.regions.push(Region {
            kind,
            base,
            bytes,
            init: vec![init; n],
            spills: HashMap::new(),
            live: true,
        });
        (self.regions.len() - 1) as RegionId
    }

    /// Value bytes stored under `key` in a hash or array map.
    pub fn map_get(&self, map: usize, key: &[u8]) -> Option<Vec<u8>> {
        match &self.maps[map] {
            MapState::Kv { entries, .. } => entries
                .get(key)
                .map(|&r| self.regions[r as usize].bytes.clone()),
            MapState::Ring(_) => None,
        }
    }

    pub fn ring(&self, map: usize) -> Option<&RingBuf> {
        match &self.maps[map] {
            MapState::Ring(r) => Some(r),
            _ => None,
        }
    }

    pub fn ring_mut(&mut self, map: usize) -> Option<&mut RingBuf> {
        match &mut self.maps[map] {
            MapState::Ring(r) => Some(r),
            _ => None,
        }
    }

    fn reset_transient(&mut self) {
        // map values allocated by updates stay; everything else is per run
        let keep: Vec<bool> = self
            .regions
            .iter()
            .map(|r| matches!(r.kind, Kind::MapValue))
            .collect();
        for (r, k) in self.regions.iter_mut().zip(keep) {
            if !k {
                r.live = false;
                r.bytes.clear();
                r.init.clear();
            }
        }
    }
}

/// Result of one program run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    pub ret: u64,
    pub insns: u64,
    /// The packet after the run (programs may write it).
    pub packet: Vec<u8>,
}

struct Frame {
    saved: [Value; 4],
    fp: Value,
    ret_pc: usize,
}

const HEADROOM: usize = 256;

/// Execute an XDP program over `packet`.
pub fn run_xdp(prog: &Program, env: &mut XdpEnv, packet: &[u8]) -> Result<RunResult, InterpError> {
    let insns = decode_all(&prog.insns).map_err(|e| InterpError::Decode(e.to_string()))?;
    let mut buf = vec![0u8; HEADROOM];
    buf.extend_from_slice(packet);
    let pkt = env.alloc(Kind::Packet, buf, true);
    let mut data_off = HEADROOM;
    env.pkt_start = HEADROOM;
    let ctx = env.alloc(Kind::Ctx, vec![0; XDP_MD_SIZE as usize], true);
    let new_stack = |env: &mut XdpEnv| {
        let r = env.alloc(Kind::Stack, vec![0; 512], false);
        Value {
            v: env.regions[r as usize].base + 512,
            prov: Some(r),
        }
    };
    let mut regs = [Value::scalar(0); 11];
    let mut init = [false; 11];
    regs[1] = Value {
        v: env.regions[ctx as usize].base,
        prov: Some(ctx),
    };
    init[1] = true;
    regs[10] = new_stack(env);
    init[10] = true;
    let mut frames: Vec<Frame> = Vec::new();
    let mut held: Vec<RegionId> = Vec::new();
    let mut pc = 0usize;
    let mut count = 0u64;
    let result = loop {
        count += 1;
        if count > env.insn_budget {
            break Err(InterpError::InsnBudget);
        }
        let Some(Some(insn)) = insns.get(pc).copied() else {
            break Err(InterpError::PcOutOfRange { pc });
        };
        let next = pc + insn.slots();
        macro_rules! get {
            ($r:expr) => {{
                let r: Reg = $r;
                if !init[r.index()] {
                    break Err(InterpError::UninitRegister { pc, reg: r.num() });
                }
                regs[r.index()]
            }};
        }
        macro_rules! set {
            ($r:expr, $v:expr) => {{
                let r: Reg = $r;
                regs[r.index()] = $v;
                init[r.index()] = true;
            }};
        }
        macro_rules! tri {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => break Err(e),
                }
            };
        }
        match insn {
            Insn::Alu { wide, op, dst, src } => {
                let b = match src {
                    Operand::Imm(i) => Value::scalar(if wide { i as i64 as u64 } else { i as u32 as u64 }),
                    Operand::Reg(r) => get!(r),
                };
                let a = if matches!(op, AluOp::Mov | AluOp::MovSx(_)) {
                    Value::scalar(0)
                } else {
                    get!(dst)
                };
                let v = alu_concrete(op, wide, a.v, b.v);
                let prov = match (op, wide, a.prov, b.prov) {
                    (AluOp::Mov, true, _, p) => p,
                    (AluOp::Add, true, Some(p), None) | (AluOp::Add, true, None, Some(p)) => Some(p),
                    (AluOp::Sub, true, Some(p), None) => Some(p),
                    _ => None,
                };
                set!(dst, Value { v, prov });
            }
            Insn::Endian { kind, bits, dst } => {
                let a = get!(dst);
                set!(dst, Value::scalar(endian(kind, bits, a.v)));
            }
            Insn::Ja { off } => {
                pc = (pc as i64 + 1 + off as i64) as usize;
                continue;
            }
            Insn::Jcond {
                wide,
                op,
                dst,
                src,
                off,
            } => {
                let a = get!(dst);
                let b = match src {
                    Operand::Imm(i) => if wide { i as i64 as u64 } else { i as u32 as u64 },
                    Operand::Reg(r) => get!(r).v,
                };
                if cmp_concrete(op, wide, a.v, b) {
                    pc = (pc as i64 + 1 + off as i64) as usize;
                    continue;
                }
            }
            Insn::LdImm64 { dst, imm } => match imm {
                Imm64::Value(v) => set!(dst, Value::scalar(v)),
                // map "pointers" are tagged scalars: the map index
                Imm64::MapFd(i) => set!(dst, Value::scalar(MAP_TAG | i as u32 as u64)),
                Imm64::Pseudo { .. } => break Err(InterpError::UnsupportedInsn { pc }),
            },
            Insn::Ldx {
                size,
                dst,
                src,
                off,
                sign_extend,
            } => {
                let p = get!(src);
                let n = size.bytes() as usize;
                let (rid, at) = tri!(resolve(env, pc, p, off, n));
                let mut v = {
                    let r = &env.regions[rid as usize];
                    if let Kind::Ctx = r.kind {
                        // xdp_md: data, data_end, data_meta, ifindex, rxq, egress
                        let pr = &env.regions[pkt as usize];
                        match at {
                            0 => Value {
                                v: pr.base + data_off as u64,
                                prov: Some(pkt),
                            },
                            4 => Value {
                                v: pr.base + pr.bytes.len() as u64,
                                prov: Some(pkt),
                            },
                            12 => Value::scalar(1),
                            16 => Value::scalar(0),
                            _ => break Err(InterpError::InvalidAccess {
                                pc,
                                detail: format!("ctx field {at}"),
                            }),
                        }
                    } else {
                        if r.init[at..at + n].iter().any(|b| !b) {
                            break Err(InterpError::UninitStackRead {
                                pc,
                                off: at as i64 - 512,
                            });
                        }
                        let mut b = [0u8; 8];
                        b[..n].copy_from_slice(&r.bytes[at..at + n]);
                        let v = u64::from_le_bytes(b);
                        let prov = if n == 8 { r.spills.get(&at).copied() } else { None };
                        Value { v, prov }
                    }
                };
                if sign_extend {
                    v = Value::scalar(sext_const(v.v, size.bytes() * 8));
                }
                set!(dst, v);
            }
            Insn::St {
                size,
                dst,
                off,
                imm,
            } => {
                let p = get!(dst);
                tri!(store(env, pc, p, off, size, Value::scalar(imm as i64 as u64)));
            }
            Insn::Stx {
                size,
                dst,
                src,
                off,
            } => {
                let p = get!(dst);
                let v = get!(src);
                tri!(store(env, pc, p, off, size, v));
            }
            Insn::Atomic {
                size,
                op,
                dst,
                src,
                off,
            } => {
                let p = get!(dst);
                let s = get!(src);
                let n = size.bytes() as usize;
                let (rid, at) = tri!(resolve(env, pc, p, off, n));
                let old = {
                    let r = &env.regions[rid as usize];
                    if r.init[at..at + n].iter().any(|b| !b) {
                        break Err(InterpError::UninitStackRead { pc, off: at as i64 });
                    }
                    let mut b = [0u8; 8];
                    b[..n].copy_from_slice(&r.bytes[at..at + n]);
                    u64::from_le_bytes(b)
                };
                let trunc = |v: u64| if n == 4 { v as u32 as u64 } else { v };
                let sv = trunc(s.v);
                let (new, fetched_to) = match op {
                    AtomicOp::Add { fetch } => (old.wrapping_add(sv), fetch.then_some(src)),
                    AtomicOp::Or { fetch } => (old | sv, fetch.then_some(src)),
                    AtomicOp::And { fetch } => (old & sv, fetch.then_some(src)),
                    AtomicOp::Xor { fetch } => (old ^ sv, fetch.then_some(src)),
                    AtomicOp::Xchg => (sv, Some(src)),
                    AtomicOp::CmpXchg => {
                        let r0 = get!(Reg::R0);
                        (if trunc(r0.v) == old { sv } else { old }, Some(Reg::R0))
                    }
                };
                let r = &mut env.regions[rid as usize];
                r.bytes[at..at + n].copy_from_slice(&trunc(new).to_le_bytes()[..n]);
                r.spills.remove(&(at & !7));
                if let Some(d) = fetched_to {
                    set!(d, Value::scalar(old));
                }
            }
            Insn::Call(CallTarget::Local(off)) => {
                if frames.len() + 1 >= 8 {
                    break Err(InterpError::CallDepth { pc });
                }
                frames.push(Frame {
                    saved: [regs[6], regs[7], regs[8], regs[9]],
                    fp: regs[10],
                    ret_pc: next,
                });
                regs[10] = new_stack(env);
                for i in 6..=9 {
                    init[i] = false;
                }
                init[0] = false;
                pc = (pc as i64 + 1 + off as i64) as usize;
                continue;
            }
            Insn::Call(CallTarget::Helper(id)) => {
                let a: [Value; 5] = [regs[1], regs[2], regs[3], regs[4], regs[5]];
                let r = tri!(call_helper(
                    env,
                    pc,
                    id,
                    &a,
                    ctx,
                    pkt,
                    &mut data_off,
                    &mut held
                ));
                set!(Reg::R0, r);
                for i in 1..=5 {
                    init[i] = false;
                }
            }
            Insn::Call(CallTarget::Kfunc(_)) | Insn::LdLegacy { .. } => {
                break Err(InterpError::UnsupportedInsn { pc });
            }
            Insn::Exit => {
                let r0 = get!(Reg::R0);
                match frames.pop() {
                    Some(f) => {
                        if let Some(p) = regs[10].prov {
                            env.regions[p as usize].live = false;
                        }
                        regs[6..=9].copy_from_slice(&f.saved);
                        for i in 6..=9 {
                            init[i] = true;
                        }
                        regs[10] = f.fp;
                        for i in 1..=5 {
                            init[i] = false;
                        }
                        set!(Reg::R0, r0);
                        pc = f.ret_pc;
                        continue;
                    }
                    None => {
                        if !held.is_empty() {
                            break Err(InterpError::ReferenceLeak {
                                pc,
                                count: held.len(),
                            });
                        }
                        let r = &env.regions[pkt as usize];
                        break Ok(RunResult {
                            ret: r0.v,
                            insns: count,
                            packet: r.bytes[data_off..].to_vec(),
                        });
                    }
                }
            }
        }
        pc = next;
    };
    env.reset_transient();
    result
}

const MAP_TAG: u64 = 0xbbf0_0000_0000_0000;

fn resolve(env: &XdpEnv, pc: usize, p: Value, off: i16, n: usize) -> Result<(RegionId, usize), InterpError> {
    let Some(rid) = p.prov else {
        return Err(InterpError::InvalidAccess {
            pc,
            detail: format!("dereference of scalar {:#x}", p.v),
        });
    };
    let r = &env.regions[rid as usize];
    if !r.live {
        return Err(InterpError::InvalidAccess {
            pc,
            detail: "use of released or dead memory".into(),
        });
    }
    let addr = p.v.wrapping_add(off as i64 as u64);
    let at = addr.wrapping_sub(r.base);
    if at > r.bytes.len() as u64 || at + n as u64 > r.bytes.len() as u64 {
        return Err(InterpError::InvalidAccess {
            pc,
            detail: format!("access at {at} size {n} outside region of {} bytes", r.bytes.len()),
        });
    }
    if matches!(r.kind, Kind::Packet) && (at as usize) < env.pkt_start {
        return Err(InterpError::InvalidAccess {
            pc,
            detail: "packet access before data".into(),
        });
    }
    if matches!(r.kind, Kind::Socket) {
        return Err(InterpError::InvalidAccess {
            pc,
            detail: "socket fields".into(),
        });
    }
    Ok((rid, at as usize))
}

fn store(env: &mut XdpEnv, pc: usize, p: Value, off: i16, size: Size, v: Value) -> Result<(), InterpError> {
    let n = size.bytes() as usize;
    let (rid, at) = resolve(env, pc, p, off, n)?;
    let r = &mut env.regions[rid as usize];
    if matches!(r.kind, Kind::Ctx) {
        return Err(InterpError::InvalidAccess {
            pc,
            detail: "ctx write".into(),
        });
    }
    r.bytes[at..at + n].copy_from_slice(&v.v.to_le_bytes()[..n]);
    for b in &mut r.init[at..at + n] {
        *b = true;
    }
    r.spills.remove(&(at & !7));
    if n == 8 && at % 8 == 0 {
        if let Some(pr) = v.prov {
            r.spills.insert(at, pr);
        }
    } else if v.prov.is_some() {
        return Err(InterpError::InvalidAccess {
            pc,
            detail: "partial pointer store".into(),
        });
    }
    Ok(())
}

fn read_mem(env: &XdpEnv, pc: usize, p: Value, n: usize) -> Result<Vec<u8>, InterpError> {
    let (rid, at) = resolve(env, pc, p, 0, n)?;
    let r = &env.regions[rid as usize];
    if r.init[at..at + n].iter().any(|b| !b) {
        return Err(InterpError::UninitStackRead { pc, off: at as i64 });
    }
    Ok(r.bytes[at..at + n].to_vec())
}

fn map_of(env: &XdpEnv, pc: usize, v: Value) -> Result<usize, InterpError> {
    if v.prov.is_none() && v.v & 0xffff_ffff_0000_0000 == MAP_TAG {
        let i = (v.v & 0xffff_ffff) as usize;
        if i < env.maps.len() {
            return Ok(i);
        }
    }
    Err(InterpError::BadHelperArg {
        pc,
        detail: "not a map".into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn call_helper(
    env: &mut XdpEnv,
    pc: usize,
    id: i32,
    a: &[Value; 5],
    _ctx: RegionId,
    pkt: RegionId,
    data_off: &mut usize,
    held: &mut Vec<RegionId>,
) -> Result<Value, InterpError> {
    let bad = |d: &str| InterpError::BadHelperArg {
        pc,
        detail: d.to_owned(),
    };
    match id {
        h::MAP_LOOKUP_ELEM | h::MAP_UPDATE_ELEM | h::MAP_DELETE_ELEM => {
            let m = map_of(env, pc, a[0])?;
            let MapState::Kv { def, .. } = &env.maps[m] else {
                return Err(bad("ringbuf map"));
            };
            let def = def.clone();
            let key = read_mem(env, pc, a[1], def.key_size as usize)?;
            match id {
                h::MAP_LOOKUP_ELEM => {
                    let MapState::Kv { entries, .. } = &env.maps[m] else {
                        unreachable!()
                    };
                    Ok(match entries.get(&key) {
                        Some(&r) => Value {
                            v: env.regions[r as usize].base,
                            prov: Some(r),
                        },
                        None => Value::scalar(0),
                    })
                }
                h::MAP_UPDATE_ELEM => {
                    let val = read_mem(env, pc, a[2], def.value_size as usize)?;
                    let exists = matches!(&env.maps[m], MapState::Kv { entries, .. } if entries.contains_key(&key));
                    if def.kind.is_array() && !exists {
                        return Ok(Value::scalar((-7i64) as u64)); // -E2BIG
                    }
                    let len = matches!(&env.maps[m], MapState::Kv { entries, .. } if entries.len() >= def.max_entries as usize);
                    if !exists && len {
                        return Ok(Value::scalar((-7i64) as u64));
                    }
                    if let MapState::Kv { entries, .. } = &env.maps[m]
                        && let Some(&r) = entries.get(&key)
                    {
                        env.regions[r as usize].bytes.copy_from_slice(&val);
                        return Ok(Value::scalar(0));
                    }
                    let r = env.alloc(Kind::MapValue, val, true);
                    if let MapState::Kv { entries, .. } = &mut env.maps[m] {
                        entries.insert(key, r);
                    }
                    Ok(Value::scalar(0))
                }
                _ => {
                    if let MapState::Kv { entries, .. } = &mut env.maps[m]
                        && let Some(r) = entries.remove(&key)
                    {
                        env.regions[r as usize].live = false;
                        return Ok(Value::scalar(0));
                    }
                    Ok(Value::scalar((-2i64) as u64)) // -ENOENT
                }
            }
        }
        h::KTIME_GET_NS => Ok(Value::scalar(env.now_ns)),
        h::GET_PRANDOM_U32 => {
            // xorshift64*, deterministic per environment
            let mut x = env.prandom_state;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            env.prandom_state = x;
            Ok(Value::scalar((x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32 as u64))
        }
        h::GET_SMP_PROCESSOR_ID => Ok(Value::scalar(env.cpu as u64)),
        h::XDP_ADJUST_HEAD => {
            let delta = a[1].v as i64 as i32 as i64;
            let len = env.regions[pkt as usize].bytes.len() as i64;
            let new = *data_off as i64 + delta;
            // keep at least an Ethernet header, as the kernel does
            if new < 0 || len - new < 14 {
                return Ok(Value::scalar((-22i64) as u64)); // -EINVAL
            }
            *data_off = new as usize;
            env.pkt_start = new as usize;
            Ok(Value::scalar(0))
        }
        h::RINGBUF_RESERVE => {
            let m = map_of(env, pc, a[0])?;
            let size = a[1].v as usize;
            let Some(ring) = env.ring_mut(m) else {
                return Err(bad("not a ringbuf"));
            };
            match ring.reserve(size) {
                Some((hdr, data)) => {
                    let r = env.alloc(
                        Kind::Ring {
                            map: m,
                            data_off: data,
                            hdr_off: hdr,
                        },
                        vec![0; size],
                        true,
                    );
                    held.push(r);
                    Ok(Value {
                        v: env.regions[r as usize].base,
                        prov: Some(r),
                    })
                }
                None => Ok(Value::scalar(0)),
            }
        }
        h::RINGBUF_SUBMIT | h::RINGBUF_DISCARD => {
            let Some(r) = a[0].prov else {
                return Err(bad("not a ringbuf record"));
            };
            let Some(pos) = held.iter().position(|x| *x == r) else {
                return Err(bad("record not held"));
            };
            if a[0].v != env.regions[r as usize].base {
                return Err(bad("record pointer with offset"));
            }
            held.remove(pos);
            let reg = env.regions[r as usize].clone();
            let Kind::Ring {
                map,
                data_off,
                hdr_off,
            } = reg.kind
            else {
                return Err(bad("not a ringbuf record"));
            };
            let ring = env.ring_mut(map).ok_or_else(|| bad("ring"))?;
            ring.data[data_off..data_off + reg.bytes.len()].copy_from_slice(&reg.bytes);
            ring.commit(hdr_off, id == h::RINGBUF_DISCARD);
            env.regions[r as usize].live = false;
            Ok(Value::scalar(0))
        }
        h::RINGBUF_OUTPUT => {
            let m = map_of(env, pc, a[0])?;
            let size = a[2].v as usize;
            let bytes = read_mem(env, pc, a[1], size)?;
            let ring = env.ring_mut(m).ok_or_else(|| bad("ring"))?;
            match ring.reserve(size) {
                Some((hdr, data)) => {
                    ring.data[data..data + size].copy_from_slice(&bytes);
                    ring.commit(hdr, false);
                    Ok(Value::scalar(0))
                }
                None => Ok(Value::scalar((-11i64) as u64)), // -EAGAIN
            }
        }
        h::SK_LOOKUP_TCP => {
            let size = a[2].v as usize;
            let tuple = read_mem(env, pc, a[1], size)?;
            if size != h::SOCK_TUPLE_IPV4 as usize {
                return Ok(Value::scalar(0));
            }
            let hit = env
                .sockets
                .iter()
                .any(|s| tuple[4..8] == s.daddr && tuple[10..12] == s.dport);
            if !hit {
                return Ok(Value::scalar(0));
            }
            let r = env.alloc(Kind::Socket, vec![0; 8], true);
            held.push(r);
            Ok(Value {
                v: env.regions[r as usize].base,
                prov: Some(r),
            })
        }
        h::SK_RELEASE => {
            let Some(r) = a[0].prov else {
                return Err(bad("not a socket"));
            };
            let Some(pos) = held.iter().position(|x| *x == r) else {
                return Err(bad("socket not held"));
            };
            held.remove(pos);
            env.regions[r as usize].live = false;
            Ok(Value::scalar(0))
        }
        _ => Err(bad("unsupported helper")),
    }
}
