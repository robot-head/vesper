//! A concrete userspace eBPF interpreter for XDP, tc (`sched_cls`),
//! tracepoint and kprobe programs ([`Input`]), used to execute verified
//! programs over fixtures and to check the verifier: every
//! value carries its provenance, and any access outside the region a
//! pointer was derived from, any read of uninitialised stack, any use of a
//! released reference and any reference still held at exit is an
//! [`InterpError`]. An accepted program that faults here is a verifier bug.
//!
//! Contexts are modelled as the kernel lays them out: `struct xdp_md`,
//! `struct __sk_buff` built from [`SkbMeta`] (what `BPF_PROG_TEST_RUN`
//! derives from a packet on `lo`), a tracepoint record (common fields
//! unreadable) and `pt_regs`. Tail calls run installed targets
//! ([`Env::install_tail_call`]) with the kernel's 33-call limit; the
//! generic kfuncs of [`crate::helpers::KFUNCS`] (RCU and irq-flag regions,
//! `bpf_iter_num_*`) run with kernel semantics.
//!
//! Single-CPU model: per-CPU maps hold the value of CPU [`Env::cpu`].
//! No real clock is read; `bpf_ktime_get_ns` returns [`Env::now_ns`].

use crate::helpers as h;
use crate::insn::{
    AluOp, AtomicOp, CallTarget, Endian, Imm64, Insn, JmpOp, Operand, Reg, Size, decode_all,
};
use crate::program::{MapDef, MapKind, Program, ProgramType};
use crate::scalar::sext_const;
use crate::verifier::{
    PERF_MAX_TRACE_SIZE, PT_REGS_SIZE, SK_BUFF_SIZE, SKB_DATA, SKB_DATA_END, SKB_WRITE, XDP_MD_SIZE,
};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

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
    let (a, b) = if wide {
        (a, b)
    } else {
        (a as u32 as u64, b as u32 as u64)
    };
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
    /// The input does not fit the program type (wrong variant, record or
    /// `pt_regs` of the wrong size).
    BadInput(String),
    UninitRegister {
        pc: usize,
        reg: u8,
    },
    InvalidAccess {
        pc: usize,
        detail: String,
    },
    UninitStackRead {
        pc: usize,
        off: i64,
    },
    BadHelperArg {
        pc: usize,
        detail: String,
    },
    UnsupportedInsn {
        pc: usize,
    },
    ReferenceLeak {
        pc: usize,
        count: usize,
    },
    InsnBudget,
    CallDepth {
        pc: usize,
    },
    PcOutOfRange {
        pc: usize,
    },
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
    Ring {
        map: usize,
        data_off: usize,
        hdr_off: usize,
    },
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
        if len > self.data.len() - RINGBUF_HDR_SZ {
            return None;
        }
        // the kernel keeps the producer at most `size - 1` bytes ahead of
        // the consumer (`new_prod_pos - cons_pos > rb->mask` fails)
        let new_prod = self.producer + total as u64;
        if new_prod - self.consumer > self.mask() {
            return None;
        }
        // headers are 8-byte aligned in a power-of-two area, so a header
        // never wraps; the payload may, as in the kernel's double mapping
        let hdr = (self.producer & self.mask()) as usize;
        let hlen = (len as u32) | RINGBUF_BUSY_BIT;
        self.data[hdr..hdr + 4].copy_from_slice(&hlen.to_le_bytes());
        self.data[hdr + 4..hdr + 8].copy_from_slice(&0u32.to_le_bytes());
        self.producer = new_prod;
        Some((hdr, (hdr + RINGBUF_HDR_SZ) & self.mask() as usize))
    }

    /// Copy a payload into the data area starting at `at`, wrapping at the
    /// end as the kernel's second mapping of the data pages does.
    fn write_payload(&mut self, at: usize, bytes: &[u8]) {
        let n = self.data.len();
        for (i, b) in bytes.iter().enumerate() {
            self.data[(at + i) % n] = *b;
        }
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

/// The calling task as the tracing helpers (`bpf_get_current_pid_tgid`,
/// `bpf_get_current_uid_gid`, `bpf_get_current_comm`) see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub pid: u32,
    pub tgid: u32,
    pub uid: u32,
    pub gid: u32,
    /// `task->comm`: NUL-terminated within its 16 bytes.
    pub comm: [u8; 16],
}

impl Task {
    /// A task named `comm` (truncated to 15 bytes).
    pub fn named(comm: &str, pid: u32, tgid: u32) -> Task {
        let mut c = [0u8; 16];
        for (d, s) in c.iter_mut().zip(comm.bytes().take(15)) {
            *d = s;
        }
        Task {
            pid,
            tgid,
            uid: 0,
            gid: 0,
            comm: c,
        }
    }
}

impl Default for Task {
    fn default() -> Self {
        Task::named("vesper", 1000, 1000)
    }
}

/// The `struct __sk_buff` fields of a tc (`BPF_PROG_TYPE_SCHED_CLS`)
/// context that are not derived from the packet itself. `len`, `data` and
/// `data_end` come from the packet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkbMeta {
    pub pkt_type: u32,
    pub mark: u32,
    pub queue_mapping: u32,
    /// `skb->protocol` as the 16-bit network-order value (`0x0800` for
    /// IPv4); the context field holds its two bytes as they are in memory.
    pub protocol: u16,
    pub vlan_tci: u32,
    pub vlan_proto: u32,
    pub priority: u32,
    pub ingress_ifindex: u32,
    pub ifindex: u32,
    pub tc_index: u32,
    pub cb: [u32; 5],
    pub hash: u32,
    pub tc_classid: u32,
}

/// `PACKET_HOST` .. `PACKET_OTHERHOST` (`linux/if_packet.h`).
pub const PACKET_HOST: u32 = 0;
pub const PACKET_BROADCAST: u32 = 1;
pub const PACKET_MULTICAST: u32 = 2;
pub const PACKET_OTHERHOST: u32 = 3;
/// Interface index of `lo`, the device `BPF_PROG_TEST_RUN` uses.
pub const LOOPBACK_IFINDEX: u32 = 1;

impl SkbMeta {
    /// What `BPF_PROG_TEST_RUN` derives for `packet` on the loopback
    /// device (`bpf_prog_test_run_skb`, `eth_type_trans`): the packet type
    /// from the destination MAC against lo's all-zero address (never
    /// [`PACKET_BROADCAST`]: lo has no broadcast address), the
    /// protocol from the EtherType (802.3 length fields become
    /// `ETH_P_802_3` or `ETH_P_802_2`), `ifindex` of lo, everything else
    /// zero. `ctx_in` fields (mark, priority, cb) are set by the caller.
    pub fn test_run(packet: &[u8]) -> SkbMeta {
        let mut m = SkbMeta {
            ifindex: LOOPBACK_IFINDEX,
            ..SkbMeta::default()
        };
        if packet.len() < 14 {
            return m;
        }
        let dst = &packet[..6];
        // lo has no broadcast address (`dev->broadcast` is all zero, which
        // is not a multicast address), so ff:ff:ff:ff:ff:ff is multicast
        // there: checked against Linux 6.18
        m.pkt_type = if dst[0] & 1 != 0 {
            PACKET_MULTICAST
        } else if dst.iter().any(|&b| b != 0) {
            PACKET_OTHERHOST
        } else {
            PACKET_HOST
        };
        let ty = u16::from_be_bytes([packet[12], packet[13]]);
        m.protocol = if ty >= 0x600 {
            ty
        } else if packet.len() >= 16 && packet[14] == 0xff && packet[15] == 0xff {
            0x0001 // ETH_P_802_3
        } else {
            0x0004 // ETH_P_802_2
        };
        m
    }

    /// The 192-byte `struct __sk_buff` for a packet of `len` bytes, with
    /// the pointer fields (`data`, `data_end`, ...) zero.
    pub fn to_ctx(&self, len: u32) -> Vec<u8> {
        let mut c = vec![0u8; SK_BUFF_SIZE as usize];
        let mut put = |at: usize, v: u32| c[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(0, len);
        put(4, self.pkt_type);
        put(8, self.mark);
        put(12, self.queue_mapping);
        put(16, u16::from_ne_bytes(self.protocol.to_be_bytes()) as u32);
        put(24, self.vlan_tci);
        put(28, self.vlan_proto);
        put(32, self.priority);
        put(36, self.ingress_ifindex);
        put(40, self.ifindex);
        put(44, self.tc_index);
        for (i, v) in self.cb.iter().enumerate() {
            put(48 + 4 * i, *v);
        }
        put(68, self.hash);
        put(72, self.tc_classid);
        c
    }
}

/// The context a run starts from, one variant per [`ProgramType`].
#[derive(Clone, Debug)]
pub enum Input<'a> {
    /// XDP: the packet (`struct xdp_md` is synthesised, with 256 bytes of
    /// headroom for `bpf_xdp_adjust_head`).
    Xdp(&'a [u8]),
    /// tc: the packet from its Ethernet header, and the `__sk_buff`
    /// metadata.
    Skb { packet: &'a [u8], meta: SkbMeta },
    /// Tracepoint: the raw record, common fields (8 bytes) first, at most
    /// `PERF_MAX_TRACE_SIZE` bytes. The kernel lets a program read up to
    /// that size whatever the record's length; in this model the bytes
    /// past the record read as zero.
    Tracepoint(&'a [u8]),
    /// Kprobe: the `struct pt_regs` bytes of the host architecture
    /// (exactly `PT_REGS_SIZE`).
    Kprobe(&'a [u8]),
}

impl Input<'_> {
    pub fn prog_type(&self) -> ProgramType {
        match self {
            Input::Xdp(_) => ProgramType::Xdp,
            Input::Skb { .. } => ProgramType::SchedCls,
            Input::Tracepoint(_) => ProgramType::Tracepoint,
            Input::Kprobe(_) => ProgramType::Kprobe,
        }
    }
}

/// Why a tail-call target cannot be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TailCallError {
    /// The map is not a `BPF_MAP_TYPE_PROG_ARRAY` of this environment.
    NotProgArray {
        map: usize,
    },
    IndexOutOfRange {
        index: u32,
        max_entries: u32,
    },
    /// The kernel only accepts targets of the owner's program type.
    TypeMismatch {
        owner: ProgramType,
        target: ProgramType,
    },
    /// The target references a map this environment does not have (maps
    /// are matched by their full definition, name included).
    MissingMap {
        name: String,
    },
    Decode(String),
}

impl fmt::Display for TailCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for TailCallError {}

/// Decoded instructions of one program, with its map indices translated to
/// the environment's.
#[derive(Debug)]
struct Code {
    prog_type: ProgramType,
    insns: Vec<Option<Insn>>,
    kfuncs: Vec<(usize, String)>,
    maps: Vec<usize>,
}

impl Code {
    fn new(prog: &Program, maps: Vec<usize>) -> Result<Code, String> {
        Ok(Code {
            prog_type: prog.prog_type,
            insns: decode_all(&prog.insns).map_err(|e| e.to_string())?,
            kfuncs: prog
                .kfuncs
                .iter()
                .map(|k| (k.slot, k.name.clone()))
                .collect(),
            maps,
        })
    }
}

/// `MAX_TAIL_CALL_CNT`.
pub const MAX_TAIL_CALL_CNT: u32 = 33;

/// Everything outside the program: maps, the CPU, the virtual clock, the
/// calling task, the sockets `bpf_sk_lookup_tcp` can find and the programs
/// installed in program arrays.
#[derive(Clone, Debug)]
pub struct Env {
    pub maps: Vec<MapState>,
    pub cpu: u32,
    pub now_ns: u64,
    pub prandom_state: u64,
    pub sockets: Vec<ListeningSocket>,
    pub task: Task,
    pub insn_budget: u64,
    defs: Vec<MapDef>,
    prog_type: ProgramType,
    tail: HashMap<(usize, u32), Arc<Code>>,
    regions: Vec<Region>,
    next_base: u64,
    persistent: usize,
    /// Map values deleted during the current run. The kernel frees them
    /// after an RCU grace period, so pointers the program already holds
    /// stay readable until the run ends.
    deferred: Vec<RegionId>,
    /// Offset of `data` inside the packet region (headroom before it).
    pkt_start: usize,
}

/// The name this environment had when it ran XDP programs only.
pub type XdpEnv = Env;

impl Env {
    pub fn new(prog: &Program) -> Self {
        let mut env = Env {
            maps: Vec::new(),
            cpu: 0,
            now_ns: 0,
            prandom_state: 0x5eed,
            sockets: Vec::new(),
            task: Task::default(),
            insn_budget: 1_000_000,
            defs: prog.maps.clone(),
            prog_type: prog.prog_type,
            tail: HashMap::new(),
            regions: Vec::new(),
            next_base: 0x1000_0000,
            persistent: 0,
            deferred: Vec::new(),
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
            // program arrays hold programs, installed with
            // `install_tail_call`, not values
            if def.kind.is_array() && def.kind != MapKind::ProgArray {
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

    /// Install `target` at `index` of program array `map`, as a loader's
    /// `bpf_map_update_elem(prog_array, &index, &prog_fd)` does. The
    /// target shares this environment's maps: each map it references must
    /// be defined identically (name included) in the program the
    /// environment was made for.
    pub fn install_tail_call(
        &mut self,
        map: usize,
        index: u32,
        target: &Program,
    ) -> Result<(), TailCallError> {
        let Some(def) = self.defs.get(map).filter(|d| d.kind == MapKind::ProgArray) else {
            return Err(TailCallError::NotProgArray { map });
        };
        if index >= def.max_entries {
            return Err(TailCallError::IndexOutOfRange {
                index,
                max_entries: def.max_entries,
            });
        }
        if target.prog_type != self.prog_type {
            return Err(TailCallError::TypeMismatch {
                owner: self.prog_type,
                target: target.prog_type,
            });
        }
        let mut maps = Vec::new();
        for d in &target.maps {
            let Some(i) = self.defs.iter().position(|e| e == d) else {
                return Err(TailCallError::MissingMap {
                    name: d.name.clone(),
                });
            };
            maps.push(i);
        }
        let code = Code::new(target, maps).map_err(TailCallError::Decode)?;
        self.tail.insert((map, index), Arc::new(code));
        Ok(())
    }

    /// Empty slot `index` of program array `map` (`bpf_map_delete_elem`).
    pub fn remove_tail_call(&mut self, map: usize, index: u32) -> bool {
        self.tail.remove(&(map, index)).is_some()
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
        // map values allocated by updates stay; everything a run allocated
        // since `persistent` is released (regions before it were released
        // by earlier runs or are map values)
        for r in &mut self.regions[self.persistent..] {
            if !matches!(r.kind, Kind::MapValue) {
                r.live = false;
                r.bytes = Vec::new();
                r.init = Vec::new();
            }
        }
        self.persistent = self.regions.len();
        for r in std::mem::take(&mut self.deferred) {
            let r = &mut self.regions[r as usize];
            r.live = false;
            r.bytes = Vec::new();
            r.init = Vec::new();
        }
    }
}

/// Result of one program run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    pub ret: u64,
    pub insns: u64,
    /// The packet after the run (programs may write it); empty for
    /// tracing programs.
    pub packet: Vec<u8>,
    /// The context bytes after the run: a tc program's writes to `mark`,
    /// `priority`, `cb[]`, ... (pointer fields read as zero).
    pub ctx: Vec<u8>,
    /// Tail calls taken.
    pub tail_calls: u32,
}

struct Frame {
    saved: [Value; 4],
    fp: Value,
    ret_pc: usize,
    /// Set when the frame runs a `bpf_loop` callback.
    lp: Option<LoopFrame>,
}

#[derive(Clone, Copy)]
struct LoopFrame {
    entry: usize,
    i: u64,
    n: u64,
    arg: Value,
}

/// `BPF_MAX_LOOPS`.
const BPF_MAX_LOOPS: u64 = 1 << 23;
const E2BIG: u64 = 7;

const HEADROOM: usize = 256;

/// Execute an XDP program over `packet`.
pub fn run_xdp(prog: &Program, env: &mut Env, packet: &[u8]) -> Result<RunResult, InterpError> {
    run(prog, env, Input::Xdp(packet))
}

/// Execute a tc program over `packet` with `__sk_buff` metadata `meta`.
pub fn run_skb(
    prog: &Program,
    env: &mut Env,
    packet: &[u8],
    meta: SkbMeta,
) -> Result<RunResult, InterpError> {
    run(prog, env, Input::Skb { packet, meta })
}

/// Execute `prog` once from `input`, which must match its program type.
pub fn run(prog: &Program, env: &mut Env, input: Input<'_>) -> Result<RunResult, InterpError> {
    if input.prog_type() != prog.prog_type {
        return Err(InterpError::BadInput(format!(
            "a {:?} program cannot run on {:?} input",
            prog.prog_type,
            input.prog_type()
        )));
    }
    let code = Code::new(prog, (0..prog.maps.len()).collect()).map_err(InterpError::Decode)?;
    let ptype = prog.prog_type;
    env.pkt_start = HEADROOM;
    let (pkt, ctx_bytes) = match input {
        Input::Xdp(p) => {
            let mut buf = vec![0u8; HEADROOM];
            buf.extend_from_slice(p);
            (
                Some(env.alloc(Kind::Packet, buf, true)),
                vec![0; XDP_MD_SIZE as usize],
            )
        }
        Input::Skb { packet, meta } => {
            let len = u32::try_from(packet.len())
                .map_err(|_| InterpError::BadInput("packet too long".into()))?;
            let mut buf = vec![0u8; HEADROOM];
            buf.extend_from_slice(packet);
            (Some(env.alloc(Kind::Packet, buf, true)), meta.to_ctx(len))
        }
        Input::Tracepoint(rec) => {
            if rec.len() > PERF_MAX_TRACE_SIZE as usize {
                return Err(InterpError::BadInput(format!(
                    "tracepoint record of {} bytes exceeds {PERF_MAX_TRACE_SIZE}",
                    rec.len()
                )));
            }
            let mut b = rec.to_vec();
            b.resize(PERF_MAX_TRACE_SIZE as usize, 0);
            (None, b)
        }
        Input::Kprobe(regs) => {
            if PT_REGS_SIZE == 0 || regs.len() != PT_REGS_SIZE as usize {
                return Err(InterpError::BadInput(format!(
                    "pt_regs must be {PT_REGS_SIZE} bytes on this architecture, got {}",
                    regs.len()
                )));
            }
            (None, regs.to_vec())
        }
    };
    let mut data_off = HEADROOM;
    let ctx = env.alloc(Kind::Ctx, ctx_bytes, true);
    let ctx_ptr = Value {
        v: env.regions[ctx as usize].base,
        prov: Some(ctx),
    };
    let new_stack = |env: &mut Env| {
        let r = env.alloc(Kind::Stack, vec![0; 512], false);
        Value {
            v: env.regions[r as usize].base + 512,
            prov: Some(r),
        }
    };
    let mut code = Arc::new(code);
    let mut regs = [Value::scalar(0); 11];
    let mut init = [false; 11];
    regs[1] = ctx_ptr;
    init[1] = true;
    regs[10] = new_stack(env);
    init[10] = true;
    let mut frames: Vec<Frame> = Vec::new();
    let mut held: Vec<RegionId> = Vec::new();
    let mut pc = 0usize;
    let mut count = 0u64;
    let mut tail_calls = 0u32;
    let mut tail_taken = 0u32;
    let result = loop {
        count += 1;
        if count > env.insn_budget {
            break Err(InterpError::InsnBudget);
        }
        let Some(Some(insn)) = code.insns.get(pc).copied() else {
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
                    Operand::Imm(i) => Value::scalar(if wide {
                        i as i64 as u64
                    } else {
                        i as u32 as u64
                    }),
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
                    (AluOp::Add, true, Some(p), None) | (AluOp::Add, true, None, Some(p)) => {
                        Some(p)
                    }
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
                    Operand::Imm(i) => {
                        if wide {
                            i as i64 as u64
                        } else {
                            i as u32 as u64
                        }
                    }
                    Operand::Reg(r) => get!(r).v,
                };
                if cmp_concrete(op, wide, a.v, b) {
                    pc = (pc as i64 + 1 + off as i64) as usize;
                    continue;
                }
            }
            Insn::LdImm64 { dst, imm } => match imm {
                Imm64::Value(v) => set!(dst, Value::scalar(v)),
                // map "pointers" are tagged scalars: the environment's map
                // index
                Imm64::MapFd(i) => match code.maps.get(i as usize) {
                    Some(&m) => set!(dst, Value::scalar(MAP_TAG | m as u64)),
                    None => {
                        break Err(InterpError::BadHelperArg {
                            pc,
                            detail: format!("map index {i} out of range"),
                        });
                    }
                },
                // callback "pointers" are tagged scalars: the entry slot
                Imm64::Pseudo {
                    src: crate::insn::pseudo::FUNC,
                    imm_lo,
                    ..
                } => set!(
                    dst,
                    Value::scalar(FUNC_TAG | (pc as i64 + 1 + imm_lo as i64) as u32 as u64)
                ),
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
                    let ctx_ptr = match r.kind {
                        Kind::Ctx => tri!(ctx_read(env, pc, ptype, pkt, data_off, at, n)),
                        _ => None,
                    };
                    if let Some(v) = ctx_ptr {
                        v
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
                        let prov = if n == 8 {
                            r.spills.get(&at).copied()
                        } else {
                            None
                        };
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
                tri!(store(
                    env,
                    pc,
                    ptype,
                    p,
                    off,
                    size,
                    Value::scalar(imm as i64 as u64)
                ));
            }
            Insn::Stx {
                size,
                dst,
                src,
                off,
            } => {
                let p = get!(dst);
                let v = get!(src);
                tri!(store(env, pc, ptype, p, off, size, v));
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
                    if matches!(r.kind, Kind::Ctx | Kind::Packet) {
                        break Err(InterpError::InvalidAccess {
                            pc,
                            detail: "atomic on the context or packet".into(),
                        });
                    }
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
                    lp: None,
                });
                regs[10] = new_stack(env);
                init[6..=9].fill(false);
                init[0] = false;
                pc = (pc as i64 + 1 + off as i64) as usize;
                continue;
            }
            Insn::Call(CallTarget::Helper(h::LOOP)) => {
                // The kernel inlines bpf_loop (flags 0) and compares the
                // whole 64-bit r1 against BPF_MAX_LOOPS: 1 << 32 gives
                // -E2BIG there, not zero iterations (checked by
                // `accept/bpf_loop_upper_bits` against Linux 6.18).
                let n = get!(Reg::R1).v;
                let cb = get!(Reg::R2);
                let arg = get!(Reg::R3);
                if cb.prov.is_some() || cb.v & !0xffff_ffff != FUNC_TAG {
                    break Err(InterpError::BadHelperArg {
                        pc,
                        detail: "bpf_loop callback is not a function".into(),
                    });
                }
                if n > BPF_MAX_LOOPS {
                    set!(Reg::R0, Value::scalar(E2BIG.wrapping_neg()));
                    init[1..=5].fill(false);
                } else if n == 0 {
                    set!(Reg::R0, Value::scalar(0));
                    init[1..=5].fill(false);
                } else {
                    if frames.len() + 1 >= 8 {
                        break Err(InterpError::CallDepth { pc });
                    }
                    let entry = (cb.v & 0xffff_ffff) as usize;
                    frames.push(Frame {
                        saved: [regs[6], regs[7], regs[8], regs[9]],
                        fp: regs[10],
                        ret_pc: next,
                        lp: Some(LoopFrame {
                            entry,
                            i: 0,
                            n,
                            arg,
                        }),
                    });
                    regs[10] = new_stack(env);
                    init.fill(false);
                    init[10] = true;
                    set!(Reg::R1, Value::scalar(0));
                    set!(Reg::R2, arg);
                    pc = entry;
                    continue;
                }
            }
            Insn::Call(CallTarget::Helper(h::TAIL_CALL)) => {
                // `___bpf_prog_run` JMP_TAIL_CALL: index check, then the
                // count check, then the count is bumped, then an empty slot
                // falls through. On success the target starts over at its
                // first instruction with r1 = ctx and a fresh stack.
                let m = tri!(map_of(env, pc, get!(Reg::R2)));
                let index = get!(Reg::R3).v as u32;
                let Some(def) = env.defs.get(m).filter(|d| d.kind == MapKind::ProgArray) else {
                    break Err(InterpError::BadHelperArg {
                        pc,
                        detail: "bpf_tail_call needs a program array".into(),
                    });
                };
                let max = def.max_entries;
                init[0..=5].fill(false);
                if index >= max || tail_calls >= MAX_TAIL_CALL_CNT {
                    pc = next;
                    continue;
                }
                tail_calls += 1;
                let Some(target) = env.tail.get(&(m, index)).cloned() else {
                    pc = next;
                    continue;
                };
                if !frames.is_empty() {
                    break Err(InterpError::BadHelperArg {
                        pc,
                        detail: "tail call from a subprogram or callback".into(),
                    });
                }
                if !held.is_empty() {
                    break Err(InterpError::ReferenceLeak {
                        pc,
                        count: held.len(),
                    });
                }
                if let Some(p) = regs[10].prov {
                    env.regions[p as usize].live = false;
                }
                tail_taken += 1;
                code = target;
                init.fill(false);
                set!(Reg::R1, ctx_ptr);
                regs[10] = new_stack(env);
                init[10] = true;
                pc = 0;
                continue;
            }
            Insn::Call(CallTarget::Kfunc(_)) => {
                let name = code
                    .kfuncs
                    .iter()
                    .find(|(slot, _)| *slot == pc)
                    .map(|(_, n)| n.as_str());
                let Some(name) = name.filter(|n| crate::helpers::KFUNCS.contains(n)) else {
                    break Err(InterpError::UnsupportedInsn { pc });
                };
                let r1 = regs[1];
                let args = [r1, regs[2], regs[3]];
                let r0 = tri!(call_kfunc(env, pc, name, &args, &init));
                init[0..=5].fill(false);
                if let Some(v) = r0 {
                    set!(Reg::R0, v);
                }
            }
            Insn::Call(CallTarget::Helper(id)) => {
                if !h::allowed(code.prog_type, id) {
                    break Err(InterpError::BadHelperArg {
                        pc,
                        detail: format!("helper {id} is not available to {:?}", code.prog_type),
                    });
                }
                let a: [Value; 5] = [regs[1], regs[2], regs[3], regs[4], regs[5]];
                let r = tri!(call_helper(env, pc, id, &a, pkt, &mut data_off, &mut held));
                set!(Reg::R0, r);
                init[1..=5].fill(false);
            }
            Insn::LdLegacy { .. } => {
                break Err(InterpError::UnsupportedInsn { pc });
            }
            Insn::Exit => {
                let r0 = get!(Reg::R0);
                match frames.pop() {
                    Some(mut f) if f.lp.is_some() => {
                        let mut lp = f.lp.expect("loop frame");
                        if let Some(p) = regs[10].prov {
                            env.regions[p as usize].live = false;
                        }
                        if r0.v != 0 || lp.i + 1 == lp.n {
                            regs[6..=9].copy_from_slice(&f.saved);
                            init.fill(false);
                            init[6..=9].fill(true);
                            init[10] = true;
                            regs[10] = f.fp;
                            let done = if r0.v != 0 { lp.i + 1 } else { lp.n };
                            set!(Reg::R0, Value::scalar(done));
                            pc = f.ret_pc;
                        } else {
                            lp.i += 1;
                            f.lp = Some(lp);
                            frames.push(f);
                            regs[10] = new_stack(env);
                            init.fill(false);
                            init[10] = true;
                            set!(Reg::R1, Value::scalar(lp.i));
                            set!(Reg::R2, lp.arg);
                            pc = lp.entry;
                        }
                        continue;
                    }
                    Some(f) => {
                        if let Some(p) = regs[10].prov {
                            env.regions[p as usize].live = false;
                        }
                        regs[6..=9].copy_from_slice(&f.saved);
                        init[6..=9].fill(true);
                        regs[10] = f.fp;
                        init[1..=5].fill(false);
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
                        let packet = match pkt {
                            Some(p) => env.regions[p as usize].bytes[data_off..].to_vec(),
                            None => Vec::new(),
                        };
                        break Ok(RunResult {
                            ret: r0.v,
                            insns: count,
                            packet,
                            ctx: env.regions[ctx as usize].bytes.clone(),
                            tail_calls: tail_taken,
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

/// The value `bpf_local_irq_save` stores in its flag word here (x86
/// `EFLAGS` with `IF` set); the program cannot read it.
const IRQ_FLAGS: u64 = 0x246;
const EINVAL: u64 = 22;

/// The modelled kfuncs. Preemption, RCU read-side and irq-disabled
/// regions have no observable effect on one simulated CPU; `bpf_iter_num`
/// keeps `struct bpf_iter_num_kern { int cur; int end; }` in its stack
/// slot exactly as `kernel/bpf/bpf_iter.c` does.
fn call_kfunc(
    env: &mut Env,
    pc: usize,
    name: &str,
    a: &[Value; 3],
    init: &[bool; 11],
) -> Result<Option<Value>, InterpError> {
    let arg = |i: usize| -> Result<Value, InterpError> {
        if init[i + 1] {
            Ok(a[i])
        } else {
            Err(InterpError::UninitRegister {
                pc,
                reg: i as u8 + 1,
            })
        }
    };
    let slot = |env: &Env| -> Result<(RegionId, usize), InterpError> {
        let p = arg(0)?;
        let (rid, at) = resolve(env, pc, p, 0, 8)?;
        if !matches!(env.regions[rid as usize].kind, Kind::Stack) {
            return Err(InterpError::BadHelperArg {
                pc,
                detail: format!("{name}: arg #0 is not on the stack"),
            });
        }
        Ok((rid, at))
    };
    let write = |env: &mut Env, rid: RegionId, at: usize, v: u64| {
        let r = &mut env.regions[rid as usize];
        r.bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
        r.init[at..at + 8].fill(true);
        r.spills.remove(&at);
    };
    let iter = |env: &Env, rid: RegionId, at: usize| {
        let b = &env.regions[rid as usize].bytes[at..at + 8];
        (
            i32::from_le_bytes(b[..4].try_into().expect("4 bytes")),
            i32::from_le_bytes(b[4..].try_into().expect("4 bytes")),
        )
    };
    let pack = |cur: i32, end: i32| (cur as u32 as u64) | ((end as u32 as u64) << 32);
    Ok(match name {
        "bpf_local_irq_save" => {
            let (rid, at) = slot(env)?;
            write(env, rid, at, IRQ_FLAGS);
            None
        }
        "bpf_local_irq_restore" => {
            slot(env)?;
            None
        }
        "bpf_iter_num_new" => {
            let (rid, at) = slot(env)?;
            let start = arg(1)?.v as i32;
            let end = arg(2)?.v as i32;
            let (state, ret) = if start > end {
                (pack(0, 0), EINVAL.wrapping_neg())
            } else if end as i64 - start as i64 > BPF_MAX_LOOPS as i64 {
                (pack(0, 0), E2BIG.wrapping_neg())
            } else {
                (pack(start.wrapping_sub(1), end), 0)
            };
            write(env, rid, at, state);
            Some(Value::scalar(ret as u32 as u64))
        }
        "bpf_iter_num_next" => {
            let (rid, at) = slot(env)?;
            let (cur, end) = iter(env, rid, at);
            if cur as i64 + 1 >= end as i64 {
                write(env, rid, at, pack(0, 0));
                Some(Value::scalar(0))
            } else {
                write(env, rid, at, pack(cur + 1, end));
                let r = &env.regions[rid as usize];
                Some(Value {
                    v: r.base + at as u64,
                    prov: Some(rid),
                })
            }
        }
        "bpf_iter_num_destroy" => {
            let (rid, at) = slot(env)?;
            write(env, rid, at, pack(0, 0));
            None
        }
        _ => None,
    })
}

/// A context load: `Some` for the pointer fields, `None` for a plain field
/// read from the context bytes; offsets the program type does not expose
/// are faults.
fn ctx_read(
    env: &Env,
    pc: usize,
    t: ProgramType,
    pkt: Option<RegionId>,
    data_off: usize,
    at: usize,
    n: usize,
) -> Result<Option<Value>, InterpError> {
    let bad = || InterpError::InvalidAccess {
        pc,
        detail: format!("{t:?} ctx field {at} size {n}"),
    };
    let packet_ptr = |end: bool| {
        let p = pkt.expect("packet program");
        let pr = &env.regions[p as usize];
        Value {
            v: pr.base
                + if end {
                    pr.bytes.len() as u64
                } else {
                    data_off as u64
                },
            prov: Some(p),
        }
    };
    match t {
        // xdp_md: data, data_end, data_meta, ifindex, rxq, egress
        ProgramType::Xdp => match at {
            0 => Ok(Some(packet_ptr(false))),
            4 => Ok(Some(packet_ptr(true))),
            12 => Ok(Some(Value::scalar(1))),
            16 => Ok(Some(Value::scalar(0))),
            _ => Err(bad()),
        },
        ProgramType::SchedCls => match at as i32 {
            SKB_DATA if n == 4 => Ok(Some(packet_ptr(false))),
            SKB_DATA_END if n == 4 => Ok(Some(packet_ptr(true))),
            o if o < SKB_DATA => Ok(None),
            _ => Err(bad()),
        },
        // the first 8 bytes are the common fields, not readable
        ProgramType::Tracepoint if at < 8 => Err(bad()),
        ProgramType::Tracepoint | ProgramType::Kprobe => Ok(None),
    }
}

const BPF_NOEXIST: u64 = 1;
const BPF_EXIST: u64 = 2;

const MAP_TAG: u64 = 0xbbf0_0000_0000_0000;
const FUNC_TAG: u64 = 0xbbf1_0000_0000_0000;

fn resolve(
    env: &Env,
    pc: usize,
    p: Value,
    off: i16,
    n: usize,
) -> Result<(RegionId, usize), InterpError> {
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
            detail: format!(
                "access at {at} size {n} outside region of {} bytes",
                r.bytes.len()
            ),
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

#[allow(clippy::too_many_arguments)]
fn store(
    env: &mut Env,
    pc: usize,
    t: ProgramType,
    p: Value,
    off: i16,
    size: Size,
    v: Value,
) -> Result<(), InterpError> {
    let n = size.bytes() as usize;
    let (rid, at) = resolve(env, pc, p, off, n)?;
    let r = &mut env.regions[rid as usize];
    if matches!(r.kind, Kind::Ctx) {
        // tc programs may write a few u32 `__sk_buff` fields (scalars)
        let writable = t == ProgramType::SchedCls
            && n == 4
            && SKB_WRITE.contains(&(at as i32))
            && v.prov.is_none();
        if !writable {
            return Err(InterpError::InvalidAccess {
                pc,
                detail: format!("{t:?} ctx write at {at} size {n}"),
            });
        }
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

fn read_mem(env: &Env, pc: usize, p: Value, n: usize) -> Result<Vec<u8>, InterpError> {
    let (rid, at) = resolve(env, pc, p, 0, n)?;
    let r = &env.regions[rid as usize];
    if r.init[at..at + n].iter().any(|b| !b) {
        return Err(InterpError::UninitStackRead { pc, off: at as i64 });
    }
    Ok(r.bytes[at..at + n].to_vec())
}

fn map_of(env: &Env, pc: usize, v: Value) -> Result<usize, InterpError> {
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
    env: &mut Env,
    pc: usize,
    id: i32,
    a: &[Value; 5],
    pkt: Option<RegionId>,
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
                    // flags: BPF_ANY 0, BPF_NOEXIST 1, BPF_EXIST 2; anything
                    // else (including BPF_F_LOCK without a spin lock) is -EINVAL
                    let flags = a[3].v;
                    if flags > BPF_EXIST {
                        return Ok(Value::scalar((-22i64) as u64)); // -EINVAL
                    }
                    if def.kind.is_array() {
                        if !exists {
                            return Ok(Value::scalar((-7i64) as u64)); // -E2BIG
                        }
                        if flags == BPF_NOEXIST {
                            return Ok(Value::scalar((-17i64) as u64)); // -EEXIST
                        }
                    } else if flags == BPF_NOEXIST && exists {
                        return Ok(Value::scalar((-17i64) as u64)); // -EEXIST
                    } else if flags == BPF_EXIST && !exists {
                        return Ok(Value::scalar((-2i64) as u64)); // -ENOENT
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
                        // reclaimed when the run ends (RCU), not now
                        env.deferred.push(r);
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
            Ok(Value::scalar(
                (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32 as u64,
            ))
        }
        h::GET_SMP_PROCESSOR_ID => Ok(Value::scalar(env.cpu as u64)),
        h::XDP_ADJUST_HEAD => {
            let delta = a[1].v as i64 as i32 as i64;
            let pkt = pkt.ok_or_else(|| bad("no packet"))?;
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
            ring.write_payload(data_off, &reg.bytes);
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
                    ring.write_payload(data, &bytes);
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
        h::GET_CURRENT_PID_TGID => Ok(Value::scalar(
            (env.task.tgid as u64) << 32 | env.task.pid as u64,
        )),
        h::GET_CURRENT_UID_GID => Ok(Value::scalar(
            (env.task.gid as u64) << 32 | env.task.uid as u64,
        )),
        h::GET_CURRENT_COMM => {
            // `strscpy_pad(buf, task->comm, size)`: at most size - 1 bytes
            // of the name, then NUL padding; the helper returns 0
            let size = a[1].v as usize;
            if size == 0 {
                return Err(bad("bpf_get_current_comm with size 0"));
            }
            let (rid, at) = resolve(env, pc, a[0], 0, size)?;
            let name_len = env.task.comm.iter().position(|&b| b == 0).unwrap_or(16);
            let keep = name_len.min(size - 1);
            let mut out = vec![0u8; size];
            out[..keep].copy_from_slice(&env.task.comm[..keep]);
            let r = &mut env.regions[rid as usize];
            // stack, map value or a live ring-buffer record (`resolve`
            // rejects released and dead memory)
            if !matches!(r.kind, Kind::Stack | Kind::MapValue | Kind::Ring { .. }) {
                return Err(bad("bpf_get_current_comm buffer"));
            }
            r.bytes[at..at + size].copy_from_slice(&out);
            r.init[at..at + size].fill(true);
            r.spills.retain(|&s, _| s + 8 <= at || s >= at + size);
            Ok(Value::scalar(0))
        }
        _ => Err(bad("unsupported helper")),
    }
}
