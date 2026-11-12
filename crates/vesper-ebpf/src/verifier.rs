//! The §27.5 pre-flight abstract verifier.
//!
//! It models the parts of Linux `kernel/bpf/verifier.c` that decide whether
//! the programs this crate emits are safe, for privileged (`CAP_BPF`) XDP
//! programs:
//!
//! * a CFG pass: decodable instructions, in-range jumps that stay inside
//!   their subprogram and never land in the second slot of `LD_IMM64`, no
//!   unreachable instructions, no fall-through off a subprogram;
//! * per-register abstract types ([`RegType`]): scalars as tnum + unsigned
//!   and signed intervals, the XDP context, packet pointers whose accessible
//!   range is established only by comparisons against `data_end`, stack
//!   pointers, map pointers and map values (maybe-null until checked),
//!   ringbuf records and socket references;
//! * the 512-byte stack: per-byte initialisation, register spills and
//!   fills, and the worst-case sum of frame sizes over the call graph;
//! * acquired references (`bpf_ringbuf_reserve`, `bpf_sk_lookup_tcp`) that
//!   must be released on every path before the program exits;
//! * path exploration with state pruning against fully explored states
//!   and rejection of loops that revisit an in-progress state unchanged.
//!
//! The direction that matters is soundness: a program this verifier accepts
//! must be one the kernel accepts and one that cannot fault. Where the
//! kernel is more permissive the verifier rejects (for example variable
//! stack offsets, pointer comparisons other than packet/`data_end` and null
//! checks, and every unsupported helper or instruction). Those
//! deliberate differences are listed in `docs/parallel-results/ebpf.md`.

use crate::callgraph::{CallGraph, FnId, FnNode, StackError, check_stack};
use crate::helpers as h;
use crate::insn::{
    AluOp, AtomicOp, CallTarget, DecodeError, Endian, Imm64, Insn, JmpOp, Operand, Reg, Size,
    decode_all,
};
use crate::program::{MapKind, Program, ProgramType};
use crate::scalar::{Scalar, Tnum, sext_const};
use std::collections::HashMap;
use std::fmt;

pub const BPF_MAX_STACK_BYTES: u32 = 512;
/// `MAX_CALL_FRAMES`.
pub const MAX_CALL_FRAMES: usize = 8;
/// `MAX_PACKET_OFF`.
pub const MAX_PACKET_OFF: u64 = 0xffff;
const MAX_VAR_OFF: u64 = 1 << 29;
const STACK_SLOTS: usize = (BPF_MAX_STACK_BYTES / 8) as usize;

/// `sizeof(struct xdp_md)`.
pub const XDP_MD_SIZE: i32 = 24;

/// Tunable limits (the kernel's are fixed; tests lower them).
#[derive(Clone, Debug)]
pub struct VerifierPolicy {
    /// `BPF_COMPLEXITY_LIMIT_INSNS`.
    pub max_insns_processed: u64,
    /// Explored states kept per instruction for pruning.
    pub max_states_per_insn: usize,
}

impl Default for VerifierPolicy {
    fn default() -> Self {
        Self {
            max_insns_processed: 1_000_000,
            max_states_per_insn: 64,
        }
    }
}

/// A rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    /// Slot index of the offending instruction, when there is one.
    pub insn: Option<usize>,
    /// Stable rule identifier, e.g. `"mem.packet_out_of_range"`.
    pub rule: &'static str,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.insn {
            Some(i) => write!(f, "insn {i}: [{}] {}", self.rule, self.message),
            None => write!(f, "[{}] {}", self.rule, self.message),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Contract-shaped finding (wave-five shared vocabulary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifierFinding {
    pub function: String,
    pub inst: Option<u32>,
    pub rule: &'static str,
    pub message: String,
}

/// Summary of an accepted program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyReport {
    pub insns_processed: u64,
    pub states_stored: usize,
    pub paths_completed: u64,
    pub pruned: u64,
    /// Per-subprogram (entry slot, max stack bytes touched).
    pub subprog_stack: Vec<(usize, u32)>,
    /// Worst-case total stack over the call graph (kernel rounding).
    pub worst_stack: u32,
    pub call_graph: CallGraph,
}

fn err(insn: usize, rule: &'static str, message: impl Into<String>) -> VerifyError {
    VerifyError {
        insn: Some(insn),
        rule,
        message: message.into(),
    }
}

/// Abstract value of one register or spilled stack slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegType {
    NotInit,
    Scalar(Scalar),
    /// `PTR_TO_CTX` (`struct xdp_md *`), offset always zero.
    Ctx,
    /// `PTR_TO_PACKET`: `data + var + off`. Pointers that share `id` share
    /// `var`; the accessible range of an id lives in the state.
    Packet { id: u32, off: i32, var: Scalar },
    /// `PTR_TO_PACKET_END`.
    PacketEnd,
    /// `PTR_TO_STACK` into frame `frame` at constant offset `off` (< 0).
    Stack { frame: u8, off: i32 },
    /// `CONST_PTR_TO_MAP`.
    MapPtr { map: u32 },
    /// `PTR_TO_MAP_VALUE`.
    MapValue { map: u32, off: i32, var: Scalar },
    /// `PTR_TO_MAP_VALUE_OR_NULL`.
    MapValueOrNull { map: u32, id: u32 },
    /// `PTR_TO_MEM | MEM_RINGBUF` holding acquired reference `ref_id`.
    Mem { size: u32, off: i32, ref_id: u32 },
    MemOrNull { size: u32, id: u32, ref_id: u32 },
    /// `PTR_TO_SOCKET` holding acquired reference `ref_id`.
    Socket { ref_id: u32 },
    SocketOrNull { id: u32, ref_id: u32 },
}

impl RegType {
    fn is_pointer(&self) -> bool {
        !matches!(self, RegType::NotInit | RegType::Scalar(_))
    }

    fn name(&self) -> &'static str {
        match self {
            RegType::NotInit => "not initialized",
            RegType::Scalar(_) => "scalar",
            RegType::Ctx => "ctx",
            RegType::Packet { .. } => "pkt",
            RegType::PacketEnd => "pkt_end",
            RegType::Stack { .. } => "fp",
            RegType::MapPtr { .. } => "map_ptr",
            RegType::MapValue { .. } => "map_value",
            RegType::MapValueOrNull { .. } => "map_value_or_null",
            RegType::Mem { .. } => "ringbuf_mem",
            RegType::MemOrNull { .. } => "ringbuf_mem_or_null",
            RegType::Socket { .. } => "sock",
            RegType::SocketOrNull { .. } => "sock_or_null",
        }
    }

    fn ref_id(&self) -> Option<u32> {
        match *self {
            RegType::Mem { ref_id, .. }
            | RegType::MemOrNull { ref_id, .. }
            | RegType::Socket { ref_id }
            | RegType::SocketOrNull { ref_id, .. } => Some(ref_id),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Slot {
    /// Bit `i` set: byte `i` of the slot is initialised (unknown value).
    Bytes(u8),
    /// An 8-byte aligned register spill.
    Spill(RegType),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Frame {
    regs: [RegType; 11],
    stack: Vec<Slot>,
    subprog: usize,
    return_pc: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RefKind {
    Ringbuf,
    Socket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RefEntry {
    id: u32,
    kind: RefKind,
    acquired_at: usize,
    frame: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    frames: Vec<Frame>,
    refs: Vec<RefEntry>,
    /// Accessible bytes per packet-pointer id.
    pkt_ranges: Vec<(u32, u32)>,
    next_id: u32,
}

impl State {
    fn cur(&self) -> &Frame {
        self.frames.last().expect("at least one frame")
    }

    fn cur_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("at least one frame")
    }

    fn reg(&self, r: Reg) -> RegType {
        self.cur().regs[r.index()]
    }

    fn set(&mut self, r: Reg, v: RegType) {
        self.cur_mut().regs[r.index()] = v;
    }

    fn fresh_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn range(&self, id: u32) -> u32 {
        self.pkt_ranges
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, r)| *r)
            .unwrap_or(0)
    }

    fn set_range(&mut self, id: u32, r: u32) {
        if let Some(e) = self.pkt_ranges.iter_mut().find(|(i, _)| *i == id) {
            e.1 = e.1.max(r);
        } else {
            self.pkt_ranges.push((id, r));
            self.pkt_ranges.sort_unstable();
        }
    }

    /// Apply `f` to every register and spilled register in every frame.
    fn for_each_value(&mut self, mut f: impl FnMut(&mut RegType)) {
        for fr in &mut self.frames {
            for r in fr.regs.iter_mut() {
                f(r);
            }
            for s in fr.stack.iter_mut() {
                if let Slot::Spill(r) = s {
                    f(r);
                    if matches!(r, RegType::NotInit) {
                        // an invalidated spill reads back as unknown bytes
                        *s = Slot::Bytes(0xff);
                    }
                }
            }
        }
    }
}

fn new_frame(subprog: usize, return_pc: Option<usize>, frame_no: u8) -> Frame {
    let mut regs = [RegType::NotInit; 11];
    regs[10] = RegType::Stack {
        frame: frame_no,
        off: 0,
    };
    Frame {
        regs,
        stack: vec![Slot::Bytes(0); STACK_SLOTS],
        subprog,
        return_pc,
    }
}

/// Bijection between ids of an explored state and the current state.
#[derive(Default)]
struct IdMap {
    fwd: HashMap<u32, u32>,
    rev: HashMap<u32, u32>,
}

impl IdMap {
    fn check(&mut self, old: u32, cur: u32) -> bool {
        if old == 0 || cur == 0 {
            return old == cur;
        }
        match (self.fwd.get(&old), self.rev.get(&cur)) {
            (Some(&c), _) => c == cur,
            (None, Some(_)) => false,
            (None, None) => {
                self.fwd.insert(old, cur);
                self.rev.insert(cur, old);
                true
            }
        }
    }
}

fn reg_includes(
    old: &RegType,
    cur: &RegType,
    so: &State,
    sc: &State,
    ids: &mut IdMap,
) -> bool {
    use RegType::*;
    match (*old, *cur) {
        (NotInit, _) => true,
        (Scalar(o), Scalar(c)) => o.includes(&c),
        (Ctx, Ctx) | (PacketEnd, PacketEnd) => true,
        (
            Packet {
                id: io,
                off: oo,
                var: vo,
            },
            Packet {
                id: ic,
                off: oc,
                var: vc,
            },
        ) => {
            oo == oc
                && vo.includes(&vc)
                && vc.includes(&vo)
                && ids.check(io, ic)
                && so.range(io) <= sc.range(ic)
        }
        (Stack { frame: fo, off: oo }, Stack { frame: fc, off: oc }) => fo == fc && oo == oc,
        (MapPtr { map: a }, MapPtr { map: b }) => a == b,
        (
            MapValue {
                map: mo,
                off: oo,
                var: vo,
            },
            MapValue {
                map: mc,
                off: oc,
                var: vc,
            },
        ) => mo == mc && oo == oc && vo.includes(&vc),
        (MapValueOrNull { map: mo, id: io }, MapValueOrNull { map: mc, id: ic }) => {
            mo == mc && ids.check(io, ic)
        }
        (
            Mem {
                size: zo,
                off: oo,
                ref_id: ro,
            },
            Mem {
                size: zc,
                off: oc,
                ref_id: rc,
            },
        ) => zo == zc && oo == oc && ids.check(ro, rc),
        (
            MemOrNull {
                size: zo,
                id: io,
                ref_id: ro,
            },
            MemOrNull {
                size: zc,
                id: ic,
                ref_id: rc,
            },
        ) => zo == zc && ids.check(io, ic) && ids.check(ro, rc),
        (Socket { ref_id: ro }, Socket { ref_id: rc }) => ids.check(ro, rc),
        (SocketOrNull { id: io, ref_id: ro }, SocketOrNull { id: ic, ref_id: rc }) => {
            ids.check(io, ic) && ids.check(ro, rc)
        }
        _ => false,
    }
}

/// `cur` ⊆ `old`: every behaviour of `cur` was covered by exploring `old`.
fn state_includes(old: &State, cur: &State) -> bool {
    if old.frames.len() != cur.frames.len() || old.refs.len() != cur.refs.len() {
        return false;
    }
    let mut ids = IdMap::default();
    for (fo, fc) in old.frames.iter().zip(&cur.frames) {
        if fo.subprog != fc.subprog || fo.return_pc != fc.return_pc {
            return false;
        }
        for (ro, rc) in fo.regs.iter().zip(&fc.regs) {
            if !reg_includes(ro, rc, old, cur, &mut ids) {
                return false;
            }
        }
        for (so, sc) in fo.stack.iter().zip(&fc.stack) {
            let ok = match (so, sc) {
                (Slot::Bytes(0), _) => true,
                (Slot::Bytes(mo), Slot::Bytes(mc)) => mo & !mc == 0,
                (Slot::Bytes(_), Slot::Spill(RegType::Scalar(_))) => true,
                (Slot::Spill(ro), Slot::Spill(rc)) => reg_includes(ro, rc, old, cur, &mut ids),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
    }
    // Every live reference of the explored state must correspond to a live
    // reference of the current state (same kind).
    for ro in &old.refs {
        let Some(&c) = ids.fwd.get(&ro.id) else {
            return false;
        };
        match cur.refs.iter().find(|r| r.id == c) {
            Some(rc) if rc.kind == ro.kind => {}
            _ => return false,
        }
    }
    true
}

#[derive(Clone, Copy, Debug)]
struct Subprog {
    start: usize,
    end: usize,
}

struct Record {
    state: State,
    branches: u32,
    parent: Option<usize>,
}

struct Path {
    pc: usize,
    state: State,
    parent: Option<usize>,
}

enum Step {
    Next(usize),
    Fork { taken: (usize, State), fall: usize },
    Jump(usize),
    Done,
}

/// Run the verifier on `prog`.
pub fn verify(prog: &Program, policy: &VerifierPolicy) -> Result<VerifyReport, VerifyError> {
    Verifier::new(prog, policy)?.run()
}

/// Run the verifier and return contract-shaped findings (empty = accepted).
pub fn findings(prog: &Program, policy: &VerifierPolicy) -> Vec<VerifierFinding> {
    match verify(prog, policy) {
        Ok(_) => Vec::new(),
        Err(e) => vec![VerifierFinding {
            function: prog.name.clone(),
            inst: e.insn.map(|i| i as u32),
            rule: e.rule,
            message: e.message,
        }],
    }
}

struct Verifier<'a> {
    prog: &'a Program,
    policy: &'a VerifierPolicy,
    insns: Vec<Option<Insn>>,
    subprogs: Vec<Subprog>,
    prune_point: Vec<bool>,
    explored: HashMap<usize, Vec<usize>>,
    records: Vec<Record>,
    stack_depth: Vec<u32>,
    processed: u64,
    paths: u64,
    pruned: u64,
}

impl<'a> Verifier<'a> {
    fn new(prog: &'a Program, policy: &'a VerifierPolicy) -> Result<Self, VerifyError> {
        if prog.prog_type != ProgramType::Xdp {
            return Err(VerifyError {
                insn: None,
                rule: "prog.unsupported_type",
                message: "only XDP programs are supported".into(),
            });
        }
        for m in &prog.maps {
            m.validate().map_err(|e| VerifyError {
                insn: None,
                rule: "map.invalid",
                message: format!("map {}: {e}", m.name),
            })?;
        }
        let insns = decode_all(&prog.insns).map_err(|e| {
            let pc = match e {
                DecodeError::BadRegister { pc, .. }
                | DecodeError::InvalidOpcode { pc, .. }
                | DecodeError::ReservedFieldSet { pc, .. }
                | DecodeError::BrokenLdImm64 { pc } => Some(pc),
                DecodeError::TruncatedProgram { .. } => None,
            };
            VerifyError {
                insn: pc,
                rule: "decode.invalid",
                message: e.to_string(),
            }
        })?;
        if insns.is_empty() {
            return Err(VerifyError {
                insn: None,
                rule: "cfg.empty",
                message: "empty program".into(),
            });
        }
        if insns.len() as u64 > 1_000_000 {
            return Err(VerifyError {
                insn: None,
                rule: "cfg.too_large",
                message: "program exceeds 1M instructions".into(),
            });
        }
        let mut v = Verifier {
            prog,
            policy,
            prune_point: vec![false; insns.len()],
            insns,
            subprogs: Vec::new(),
            explored: HashMap::new(),
            records: Vec::new(),
            stack_depth: Vec::new(),
            processed: 0,
            paths: 0,
            pruned: 0,
        };
        v.check_cfg()?;
        Ok(v)
    }

    fn target(&self, pc: usize, off: i64) -> Result<usize, VerifyError> {
        let t = pc as i64 + 1 + off;
        if t < 0 || t >= self.insns.len() as i64 {
            return Err(err(pc, "cfg.jump_out_of_range", format!("jump to {t} out of range")));
        }
        let t = t as usize;
        if self.insns[t].is_none() {
            return Err(err(
                pc,
                "cfg.jump_into_ld_imm64",
                format!("jump to {t}, the middle of ld_imm64"),
            ));
        }
        Ok(t)
    }

    fn subprog_of(&self, pc: usize) -> usize {
        self.subprogs
            .iter()
            .rposition(|s| s.start <= pc)
            .unwrap_or(0)
    }

    fn check_cfg(&mut self) -> Result<(), VerifyError> {
        let n = self.insns.len();
        let mut starts = vec![0usize];
        for pc in 0..n {
            if let Some(Insn::Call(CallTarget::Local(off))) = self.insns[pc] {
                let t = self.target(pc, off as i64)?;
                starts.push(t);
            }
        }
        starts.sort_unstable();
        starts.dedup();
        self.subprogs = starts
            .iter()
            .enumerate()
            .map(|(i, &s)| Subprog {
                start: s,
                end: starts.get(i + 1).copied().unwrap_or(n),
            })
            .collect();
        self.stack_depth = vec![0; self.subprogs.len()];
        for (si, sp) in self.subprogs.clone().iter().enumerate() {
            // last instruction must not fall through
            let mut last = sp.end - 1;
            if self.insns[last].is_none() {
                last -= 1;
            }
            match self.insns[last] {
                Some(Insn::Exit) | Some(Insn::Ja { .. }) => {}
                _ => {
                    return Err(err(
                        last,
                        "cfg.fallthrough",
                        format!("last insn of subprog {si} is not exit or jmp"),
                    ));
                }
            }
            for pc in sp.start..sp.end {
                let succ: Vec<i64> = match self.insns[pc] {
                    Some(Insn::Ja { off }) => vec![off as i64],
                    Some(Insn::Jcond { off, .. }) => vec![off as i64],
                    _ => vec![],
                };
                for off in succ {
                    let t = self.target(pc, off)?;
                    if t < sp.start || t >= sp.end {
                        return Err(err(
                            pc,
                            "cfg.cross_subprog_jump",
                            format!("jump from subprog {si} to {t} leaves it"),
                        ));
                    }
                    self.prune_point[t] = true;
                    if matches!(self.insns[pc], Some(Insn::Jcond { .. })) && pc + 1 < n {
                        self.prune_point[pc + 1] = true;
                    }
                }
                if let Some(Insn::Call(_)) = self.insns[pc]
                    && pc + 1 < n
                {
                    self.prune_point[pc + 1] = true;
                }
            }
            self.prune_point[sp.start] = true;
        }
        // reachability from every subprogram entry
        let mut seen = vec![false; n];
        let mut stack: Vec<usize> = self.subprogs.iter().map(|s| s.start).collect();
        while let Some(pc) = stack.pop() {
            if seen[pc] {
                continue;
            }
            seen[pc] = true;
            let insn = self.insns[pc].expect("instruction start");
            let next = pc + insn.slots();
            match insn {
                Insn::Exit => {}
                Insn::Ja { off } => stack.push(self.target(pc, off as i64)?),
                Insn::Jcond { off, .. } => {
                    stack.push(self.target(pc, off as i64)?);
                    if next < n {
                        stack.push(next);
                    }
                }
                _ => {
                    if next < n {
                        stack.push(next);
                    }
                }
            }
        }
        for pc in 0..n {
            if self.insns[pc].is_some() && !seen[pc] {
                return Err(err(pc, "cfg.unreachable", "unreachable insn"));
            }
        }
        Ok(())
    }

    fn initial_state(&self) -> State {
        let mut f = new_frame(0, None, 0);
        f.regs[1] = RegType::Ctx;
        State {
            frames: vec![f],
            refs: Vec::new(),
            pkt_ranges: Vec::new(),
            next_id: 0,
        }
    }

    fn is_ancestor(&self, rec: usize, mut cur: Option<usize>) -> bool {
        while let Some(c) = cur {
            if c == rec {
                return true;
            }
            cur = self.records[c].parent;
        }
        false
    }

    fn finish_path(&mut self, mut cur: Option<usize>) {
        self.paths += 1;
        while let Some(c) = cur {
            let r = &mut self.records[c];
            r.branches -= 1;
            if r.branches > 0 {
                break;
            }
            cur = r.parent;
        }
    }

    /// Returns `true` when the path is pruned.
    fn checkpoint(&mut self, path: &mut Path) -> Result<bool, VerifyError> {
        let pc = path.pc;
        if let Some(list) = self.explored.get(&pc) {
            for &rid in list {
                let rec = &self.records[rid];
                if rec.branches == 0 {
                    if state_includes(&rec.state, &path.state) {
                        return Ok(true);
                    }
                } else if self.is_ancestor(rid, path.parent)
                    && state_includes(&rec.state, &path.state)
                    && state_includes(&path.state, &rec.state)
                {
                    return Err(err(
                        pc,
                        "loop.unbounded",
                        "infinite loop detected: back-edge reaches an identical state",
                    ));
                }
            }
        }
        let list_len = self.explored.get(&pc).map_or(0, Vec::len);
        if list_len < self.policy.max_states_per_insn {
            self.records.push(Record {
                state: path.state.clone(),
                branches: 1,
                parent: path.parent,
            });
            let id = self.records.len() - 1;
            self.explored.entry(pc).or_default().push(id);
            path.parent = Some(id);
        }
        Ok(false)
    }

    fn run(mut self) -> Result<VerifyReport, VerifyError> {
        let mut work = vec![Path {
            pc: 0,
            state: self.initial_state(),
            parent: None,
        }];
        while let Some(mut path) = work.pop() {
            loop {
                if self.prune_point[path.pc] && self.checkpoint(&mut path)? {
                    self.pruned += 1;
                    self.finish_path(path.parent);
                    break;
                }
                self.processed += 1;
                if self.processed > self.policy.max_insns_processed {
                    return Err(err(
                        path.pc,
                        "complexity.limit",
                        format!(
                            "processed more than {} insns (unbounded loop or path explosion)",
                            self.policy.max_insns_processed
                        ),
                    ));
                }
                match self.step(path.pc, &mut path.state)? {
                    Step::Next(n) | Step::Jump(n) => path.pc = n,
                    Step::Fork { taken, fall } => {
                        if let Some(p) = path.parent {
                            self.records[p].branches += 1;
                        }
                        work.push(Path {
                            pc: taken.0,
                            state: taken.1,
                            parent: path.parent,
                        });
                        path.pc = fall;
                    }
                    Step::Done => {
                        self.finish_path(path.parent);
                        break;
                    }
                }
            }
        }
        // static stack check over the call graph
        let mut graph = CallGraph::default();
        for (i, sp) in self.subprogs.iter().enumerate() {
            let name = self
                .prog
                .functions
                .iter()
                .find(|(_, e)| *e == sp.start)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| {
                    if i == 0 {
                        self.prog.name.clone()
                    } else {
                        format!("subprog_{}", sp.start)
                    }
                });
            graph.functions.push(FnNode {
                id: FnId(i as u32),
                name,
                frame_bytes: round_stack(self.stack_depth[i]),
            });
        }
        for pc in 0..self.insns.len() {
            if let Some(Insn::Call(CallTarget::Local(off))) = self.insns[pc] {
                let t = (pc as i64 + 1 + off as i64) as usize;
                let from = self.subprog_of(pc);
                let to = self.subprog_of(t);
                graph.calls.push((FnId(from as u32), FnId(to as u32)));
            }
        }
        let worst = check_stack(&graph, FnId(0), BPF_MAX_STACK_BYTES, MAX_CALL_FRAMES).map_err(
            |e| match e {
                StackError::Exceeded { bytes, path } => VerifyError {
                    insn: None,
                    rule: "stack.limit",
                    message: format!(
                        "combined stack {bytes} bytes over call path {path:?} exceeds {BPF_MAX_STACK_BYTES}"
                    ),
                },
                StackError::Recursion { path } => VerifyError {
                    insn: None,
                    rule: "call.recursion",
                    message: format!("recursive call path {path:?}"),
                },
                StackError::TooDeep { path } => VerifyError {
                    insn: None,
                    rule: "call.depth",
                    message: format!("call path {path:?} exceeds {MAX_CALL_FRAMES} frames"),
                },
                StackError::UnknownFunction(f) | StackError::IndirectCall(f) => VerifyError {
                    insn: None,
                    rule: "call.graph",
                    message: format!("bad call graph node {f:?}"),
                },
            },
        )?;
        Ok(VerifyReport {
            insns_processed: self.processed,
            states_stored: self.records.len(),
            paths_completed: self.paths,
            pruned: self.pruned,
            subprog_stack: self
                .subprogs
                .iter()
                .zip(&self.stack_depth)
                .map(|(s, d)| (s.start, *d))
                .collect(),
            worst_stack: worst,
            call_graph: graph,
        })
    }

    fn read(&self, pc: usize, st: &State, r: Reg) -> Result<RegType, VerifyError> {
        let v = st.reg(r);
        if matches!(v, RegType::NotInit) {
            return Err(err(pc, "reg.uninit_read", format!("{r} !read_ok")));
        }
        Ok(v)
    }

    fn write_check(&self, pc: usize, r: Reg) -> Result<(), VerifyError> {
        if r == Reg::R10 {
            return Err(err(pc, "reg.write_fp", "frame pointer is read only"));
        }
        Ok(())
    }

    fn step(&mut self, pc: usize, st: &mut State) -> Result<Step, VerifyError> {
        let insn = self.insns[pc].expect("instruction start");
        let next = pc + insn.slots();
        match insn {
            Insn::Alu { wide, op, dst, src } => {
                self.alu(pc, st, wide, op, dst, src)?;
                Ok(Step::Next(next))
            }
            Insn::Endian { kind, bits, dst } => {
                self.write_check(pc, dst)?;
                let RegType::Scalar(s) = self.read(pc, st, dst)? else {
                    return Err(err(pc, "alu.pointer_arith", format!("byte swap on pointer {dst}")));
                };
                let r = match s.as_const() {
                    Some(c) => Scalar::konst(crate::interp::endian(kind, bits, c)),
                    None => match kind {
                        Endian::ToLe => Scalar::truncate(s, bits as u32),
                        Endian::ToBe | Endian::Swap => Scalar::unknown_bits(bits as u32),
                    },
                };
                st.set(dst, RegType::Scalar(r));
                Ok(Step::Next(next))
            }
            Insn::Ja { off } => Ok(Step::Jump(self.target(pc, off as i64)?)),
            Insn::Jcond {
                wide,
                op,
                dst,
                src,
                off,
            } => {
                let target = self.target(pc, off as i64)?;
                self.cond(pc, st, wide, op, dst, src, target, next)
            }
            Insn::Call(CallTarget::Helper(id)) => {
                self.helper(pc, st, id)?;
                Ok(Step::Next(next))
            }
            Insn::Call(CallTarget::Local(off)) => {
                let t = self.target(pc, off as i64)?;
                if st.frames.len() >= MAX_CALL_FRAMES {
                    return Err(err(pc, "call.depth", "the call stack of 8 frames is too deep"));
                }
                let sp = self.subprog_of(t);
                if st.frames.iter().any(|f| f.subprog == sp) {
                    return Err(err(pc, "call.recursion", "recursive call"));
                }
                let frame_no = st.frames.len() as u8;
                let mut f = new_frame(sp, Some(next), frame_no);
                for i in 1..=5 {
                    f.regs[i] = st.cur().regs[i];
                }
                st.frames.push(f);
                Ok(Step::Jump(t))
            }
            Insn::Call(CallTarget::Kfunc(id)) => Err(err(
                pc,
                "call.kfunc_unsupported",
                format!("kfunc call {id} is not supported"),
            )),
            Insn::Exit => self.exit(pc, st),
            Insn::LdImm64 { dst, imm } => {
                self.write_check(pc, dst)?;
                let v = match imm {
                    Imm64::Value(v) => RegType::Scalar(Scalar::konst(v)),
                    Imm64::MapFd(i) => {
                        if i < 0 || i as usize >= self.prog.maps.len() {
                            return Err(err(pc, "ld_imm64.bad_map", format!("no map {i}")));
                        }
                        RegType::MapPtr { map: i as u32 }
                    }
                    Imm64::Pseudo { src, .. } => {
                        return Err(err(
                            pc,
                            "insn.unsupported",
                            format!("ld_imm64 pseudo source {src} is not supported"),
                        ));
                    }
                };
                st.set(dst, v);
                Ok(Step::Next(next))
            }
            Insn::Ldx {
                size,
                dst,
                src,
                off,
                sign_extend,
            } => {
                self.write_check(pc, dst)?;
                let p = self.read(pc, st, src)?;
                let mut v = self.load(pc, st, p, off, size)?;
                if sign_extend {
                    match v {
                        RegType::Scalar(s) => {
                            v = RegType::Scalar(Scalar::sext(s, size.bytes() * 8));
                        }
                        _ => {
                            return Err(err(
                                pc,
                                "mem.pointer_partial_fill",
                                "sign-extending load of a spilled pointer",
                            ));
                        }
                    }
                }
                st.set(dst, v);
                Ok(Step::Next(next))
            }
            Insn::St {
                size,
                dst,
                off,
                imm,
            } => {
                let p = self.read(pc, st, dst)?;
                let v = RegType::Scalar(Scalar::konst(imm as i64 as u64));
                self.store(pc, st, p, off, size, v)?;
                Ok(Step::Next(next))
            }
            Insn::Stx {
                size,
                dst,
                src,
                off,
            } => {
                let p = self.read(pc, st, dst)?;
                let v = self.read(pc, st, src)?;
                self.store(pc, st, p, off, size, v)?;
                Ok(Step::Next(next))
            }
            Insn::Atomic {
                size,
                op,
                dst,
                src,
                off,
            } => {
                self.atomic(pc, st, size, op, dst, src, off)?;
                Ok(Step::Next(next))
            }
            Insn::LdLegacy { .. } => Err(err(
                pc,
                "insn.unsupported",
                "BPF_LD_ABS/BPF_LD_IND are not allowed in XDP programs",
            )),
        }
    }

    fn operand(&self, pc: usize, st: &State, wide: bool, src: Operand) -> Result<RegType, VerifyError> {
        Ok(match src {
            Operand::Imm(i) => RegType::Scalar(Scalar::konst(if wide {
                i as i64 as u64
            } else {
                i as u32 as u64
            })),
            Operand::Reg(r) => self.read(pc, st, r)?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn alu(
        &mut self,
        pc: usize,
        st: &mut State,
        wide: bool,
        op: AluOp,
        dst: Reg,
        src: Operand,
    ) -> Result<(), VerifyError> {
        self.write_check(pc, dst)?;
        let bits = if wide { 64 } else { 32 };
        let finish = |s: Scalar| {
            if wide {
                s
            } else {
                Scalar::zext32(s)
            }
        };
        match op {
            AluOp::Mov => {
                let v = self.operand(pc, st, wide, src)?;
                let v = match v {
                    RegType::Scalar(s) => RegType::Scalar(finish(if wide {
                        s
                    } else {
                        Scalar::truncate(s, 32)
                    })),
                    p if wide => p,
                    p => {
                        return Err(err(
                            pc,
                            "alu.pointer_arith",
                            format!("32-bit move of pointer ({})", p.name()),
                        ));
                    }
                };
                st.set(dst, v);
                return Ok(());
            }
            AluOp::MovSx(from) => {
                let RegType::Scalar(s) = self.operand(pc, st, wide, src)? else {
                    return Err(err(pc, "alu.pointer_arith", "sign-extending move of pointer"));
                };
                let s = if wide {
                    Scalar::sext(s, from as u32)
                } else {
                    Scalar::zext32(Scalar::sext(Scalar::truncate(s, 32), from as u32))
                };
                st.set(dst, RegType::Scalar(finish(s)));
                return Ok(());
            }
            AluOp::Neg => {
                let RegType::Scalar(s) = self.read(pc, st, dst)? else {
                    return Err(err(pc, "alu.pointer_arith", "negation of pointer"));
                };
                let r = match s.as_const() {
                    Some(c) => Scalar::konst(c.wrapping_neg()),
                    None => Scalar::UNKNOWN,
                };
                st.set(dst, RegType::Scalar(finish(r)));
                return Ok(());
            }
            _ => {}
        }
        let d = self.read(pc, st, dst)?;
        let s = self.operand(pc, st, wide, src)?;
        if let Operand::Imm(i) = src {
            if matches!(op, AluOp::Div | AluOp::Mod | AluOp::SDiv | AluOp::SMod) && i == 0 {
                return Err(err(pc, "alu.div_by_zero", "division by zero immediate"));
            }
            if matches!(op, AluOp::Lsh | AluOp::Rsh | AluOp::Arsh) && (i < 0 || i >= bits) {
                return Err(err(pc, "alu.invalid_shift", format!("invalid shift {i}")));
            }
        }
        match (d, s) {
            (RegType::Scalar(a), RegType::Scalar(b)) => {
                let (a, b) = if wide {
                    (a, b)
                } else {
                    (Scalar::truncate(a, 32), Scalar::truncate(b, 32))
                };
                let r = scalar_alu(op, wide, a, b);
                st.set(dst, RegType::Scalar(finish(r)));
                Ok(())
            }
            (p, RegType::Scalar(b)) if p.is_pointer() => {
                if !wide {
                    return Err(err(pc, "alu.pointer_arith", "32-bit pointer arithmetic"));
                }
                let neg = match op {
                    AluOp::Add => false,
                    AluOp::Sub => true,
                    _ => {
                        return Err(err(
                            pc,
                            "alu.pointer_arith",
                            format!("{op:?} on pointer ({}) prohibited", p.name()),
                        ));
                    }
                };
                let r = self.ptr_add(pc, st, p, b, neg)?;
                st.set(dst, r);
                Ok(())
            }
            (RegType::Scalar(a), p) if p.is_pointer() && op == AluOp::Add && wide => {
                let r = self.ptr_add(pc, st, p, a, false)?;
                st.set(dst, r);
                Ok(())
            }
            (RegType::Packet { .. }, RegType::Packet { .. }) if op == AluOp::Sub && wide => {
                st.set(dst, RegType::Scalar(Scalar::UNKNOWN));
                Ok(())
            }
            (d, s) => Err(err(
                pc,
                "alu.pointer_arith",
                format!("{op:?} between {} and {} prohibited", d.name(), s.name()),
            )),
        }
    }

    fn ptr_add(
        &mut self,
        pc: usize,
        st: &mut State,
        p: RegType,
        b: Scalar,
        neg: bool,
    ) -> Result<RegType, VerifyError> {
        let konst = b.as_const().map(|c| c as i64);
        let add_off = |off: i32, c: i64| -> Option<i32> {
            let c = if neg { c.checked_neg()? } else { c };
            let r = (off as i64).checked_add(c)?;
            (r.unsigned_abs() < MAX_VAR_OFF).then_some(r as i32)
        };
        let bad_off = || err(pc, "alu.pointer_offset", "pointer offset out of range");
        match p {
            RegType::Stack { frame, off } => {
                let Some(c) = konst else {
                    return Err(err(
                        pc,
                        "alu.stack_var_offset",
                        "variable offset stack pointers are not supported",
                    ));
                };
                Ok(RegType::Stack {
                    frame,
                    off: add_off(off, c).ok_or_else(bad_off)?,
                })
            }
            RegType::Mem { size, off, ref_id } => {
                let Some(c) = konst else {
                    return Err(err(
                        pc,
                        "alu.pointer_arith",
                        "variable offset into ringbuf memory is not supported",
                    ));
                };
                Ok(RegType::Mem {
                    size,
                    off: add_off(off, c).ok_or_else(bad_off)?,
                    ref_id,
                })
            }
            RegType::Packet { id, off, var } => match konst {
                Some(c) => Ok(RegType::Packet {
                    id,
                    off: add_off(off, c).ok_or_else(bad_off)?,
                    var,
                }),
                None => {
                    if neg {
                        return Err(err(
                            pc,
                            "alu.pointer_arith",
                            "subtracting a variable from a packet pointer",
                        ));
                    }
                    if b.umax > MAX_PACKET_OFF || b.smin < 0 {
                        return Err(err(
                            pc,
                            "alu.packet_var_too_large",
                            format!("variable packet offset up to {} exceeds 0xffff", b.umax),
                        ));
                    }
                    let nvar = Scalar::add(var, b);
                    if nvar.umax > MAX_PACKET_OFF {
                        return Err(err(
                            pc,
                            "alu.packet_var_too_large",
                            "accumulated variable packet offset exceeds 0xffff",
                        ));
                    }
                    Ok(RegType::Packet {
                        id: st.fresh_id(),
                        off,
                        var: nvar,
                    })
                }
            },
            RegType::MapValue { map, off, var } => match konst {
                Some(c) => Ok(RegType::MapValue {
                    map,
                    off: add_off(off, c).ok_or_else(bad_off)?,
                    var,
                }),
                None => {
                    if neg || b.umax >= MAX_VAR_OFF || b.smin < 0 {
                        return Err(err(
                            pc,
                            "alu.pointer_arith",
                            "unbounded or negative variable map value offset",
                        ));
                    }
                    Ok(RegType::MapValue {
                        map,
                        off,
                        var: Scalar::add(var, b),
                    })
                }
            },
            RegType::MapValueOrNull { .. } | RegType::MemOrNull { .. } | RegType::SocketOrNull { .. } => {
                Err(err(
                    pc,
                    "mem.null_arith",
                    format!("arithmetic on {} before null check", p.name()),
                ))
            }
            other => Err(err(
                pc,
                "alu.pointer_arith",
                format!("arithmetic on {} prohibited", other.name()),
            )),
        }
    }

    /// Validate an access of `size` bytes at `p + off`; returns the kind of
    /// target for loads and stores.
    fn access(
        &mut self,
        pc: usize,
        st: &State,
        p: RegType,
        off: i16,
        size: Size,
        write: bool,
    ) -> Result<Target, VerifyError> {
        let n = size.bytes() as i64;
        match p {
            RegType::Ctx => {
                if write {
                    return Err(err(pc, "mem.ctx_write", "cannot write into xdp_md"));
                }
                let o = off as i32;
                if !(0..XDP_MD_SIZE).contains(&o) || o as i64 % n != 0 {
                    return Err(err(pc, "mem.ctx_invalid_access", format!("invalid xdp_md access off={o} size={n}")));
                }
                let field = o & !3;
                match field {
                    0 | 4 if size == Size::W => Ok(Target::CtxPtr(field)),
                    // `__is_valid_xdp_access`: every field is a full u32
                    12 | 16 if size == Size::W => Ok(Target::Scalar(size)),
                    _ => Err(err(
                        pc,
                        "mem.ctx_invalid_access",
                        format!("invalid xdp_md access off={o} size={n}"),
                    )),
                }
            }
            RegType::Packet { id, off: po, var } => {
                let total = po as i64 + off as i64;
                let range = st.range(id) as i64;
                if var.smin < 0 || total < 0 || total + n > range {
                    return Err(err(
                        pc,
                        "mem.packet_out_of_range",
                        format!(
                            "invalid access to packet, off={total} size={n}, R range={range} (check against data_end first)"
                        ),
                    ));
                }
                Ok(Target::Scalar(size))
            }
            RegType::Stack { frame, off: so } => {
                let total = so as i64 + off as i64;
                if total < -(BPF_MAX_STACK_BYTES as i64) || total + n > 0 {
                    return Err(err(
                        pc,
                        "mem.stack_out_of_bounds",
                        format!("invalid stack off={total} size={n}"),
                    ));
                }
                if total % n != 0 {
                    return Err(err(
                        pc,
                        "mem.stack_misaligned",
                        format!("misaligned stack access off={total} size={n}"),
                    ));
                }
                let sp = st.frames[frame as usize].subprog;
                let depth = (-total) as u32;
                self.stack_depth[sp] = self.stack_depth[sp].max(depth);
                Ok(Target::Stack {
                    frame,
                    at: (total + BPF_MAX_STACK_BYTES as i64) as usize,
                })
            }
            RegType::MapValue { map, off: mo, var } => {
                let def = &self.prog.maps[map as usize];
                let lo = mo as i64 + off as i64;
                let hi = lo + var.umax as i64 + n;
                if var.smin < 0 || lo < 0 || hi > def.value_size as i64 {
                    return Err(err(
                        pc,
                        "mem.map_value_out_of_bounds",
                        format!(
                            "invalid access to map value, value_size={} off={lo} size={n}",
                            def.value_size
                        ),
                    ));
                }
                Ok(Target::Scalar(size))
            }
            RegType::Mem { size: msz, off: mo, .. } => {
                let lo = mo as i64 + off as i64;
                if lo < 0 || lo + n > msz as i64 {
                    return Err(err(
                        pc,
                        "mem.ringbuf_out_of_bounds",
                        format!("invalid access to ringbuf record of {msz} bytes, off={lo} size={n}"),
                    ));
                }
                Ok(Target::Scalar(size))
            }
            RegType::MapValueOrNull { .. } | RegType::MemOrNull { .. } | RegType::SocketOrNull { .. } => {
                Err(err(
                    pc,
                    "mem.null_deref",
                    format!("dereference of {} before null check", p.name()),
                ))
            }
            RegType::Socket { .. } => Err(err(
                pc,
                "mem.unsupported_target",
                "access to bpf_sock fields is not supported",
            )),
            RegType::NotInit => Err(err(pc, "reg.uninit_read", "uninitialized base")),
            other => Err(err(
                pc,
                "mem.invalid_pointer",
                format!("invalid mem access through {}", other.name()),
            )),
        }
    }

    fn load(
        &mut self,
        pc: usize,
        st: &State,
        p: RegType,
        off: i16,
        size: Size,
    ) -> Result<RegType, VerifyError> {
        match self.access(pc, st, p, off, size, false)? {
            Target::CtxPtr(0) => Ok(RegType::Packet {
                id: 0,
                off: 0,
                var: Scalar::konst(0),
            }),
            Target::CtxPtr(_) => Ok(RegType::PacketEnd),
            Target::Scalar(sz) => Ok(RegType::Scalar(Scalar::unknown_bits(sz.bytes() * 8))),
            Target::Stack { frame, at } => {
                let slot = st.frames[frame as usize].stack[at / 8];
                let n = size.bytes() as usize;
                let first = at % 8;
                match slot {
                    Slot::Spill(r) if n == 8 => Ok(r),
                    Slot::Spill(RegType::Scalar(_)) => {
                        Ok(RegType::Scalar(Scalar::unknown_bits(n as u32 * 8)))
                    }
                    Slot::Spill(_) => Err(err(
                        pc,
                        "mem.pointer_partial_fill",
                        "partial read of a spilled pointer",
                    )),
                    Slot::Bytes(mask) => {
                        let need = byte_mask(first, n);
                        if mask & need != need {
                            return Err(err(
                                pc,
                                "mem.uninit_stack_read",
                                format!(
                                    "invalid read from stack off={} size={n}",
                                    at as i64 - BPF_MAX_STACK_BYTES as i64
                                ),
                            ));
                        }
                        Ok(RegType::Scalar(Scalar::unknown_bits(n as u32 * 8)))
                    }
                }
            }
        }
    }

    fn store(
        &mut self,
        pc: usize,
        st: &mut State,
        p: RegType,
        off: i16,
        size: Size,
        v: RegType,
    ) -> Result<(), VerifyError> {
        match self.access(pc, st, p, off, size, true)? {
            Target::Stack { frame, at } => {
                let n = size.bytes() as usize;
                let slot = &mut st.frames[frame as usize].stack[at / 8];
                if n == 8 {
                    *slot = Slot::Spill(v);
                } else {
                    if v.is_pointer() {
                        return Err(err(
                            pc,
                            "mem.pointer_partial_spill",
                            "partial spill of a pointer",
                        ));
                    }
                    let mask = match *slot {
                        Slot::Spill(_) => 0xff,
                        Slot::Bytes(m) => m,
                    };
                    *slot = Slot::Bytes(mask | byte_mask(at % 8, n));
                }
                Ok(())
            }
            Target::Scalar(_) => {
                if v.is_pointer() {
                    return Err(err(
                        pc,
                        "mem.pointer_leak",
                        format!("storing {} into memory other than the stack", v.name()),
                    ));
                }
                Ok(())
            }
            Target::CtxPtr(_) => unreachable!("ctx writes rejected in access"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn atomic(
        &mut self,
        pc: usize,
        st: &mut State,
        size: Size,
        op: AtomicOp,
        dst: Reg,
        src: Reg,
        off: i16,
    ) -> Result<(), VerifyError> {
        let p = self.read(pc, st, dst)?;
        let v = self.read(pc, st, src)?;
        if v.is_pointer() {
            return Err(err(pc, "atomic.pointer_operand", "atomic operand must be a scalar"));
        }
        if op == AtomicOp::CmpXchg {
            let r0 = self.read(pc, st, Reg::R0)?;
            if r0.is_pointer() {
                return Err(err(pc, "atomic.pointer_operand", "cmpxchg R0 must be a scalar"));
            }
        }
        match p {
            RegType::Stack { .. } | RegType::MapValue { .. } => {}
            other => {
                return Err(err(
                    pc,
                    "atomic.invalid_target",
                    format!("BPF_ATOMIC into {} is not allowed", other.name()),
                ));
            }
        }
        // an atomic both reads and writes its target
        self.load(pc, st, p, off, size)?;
        self.store(pc, st, p, off, size, RegType::Scalar(Scalar::UNKNOWN))?;
        if let Target::Stack { frame, at } = self.access(pc, st, p, off, size, true)? {
            // the spill (if any) is now an unknown value
            let slot = &mut st.frames[frame as usize].stack[at / 8];
            if let Slot::Spill(_) = slot {
                *slot = Slot::Bytes(0xff);
            }
        }
        let fetched = RegType::Scalar(if size == Size::W {
            Scalar::unknown_bits(32)
        } else {
            Scalar::UNKNOWN
        });
        if op == AtomicOp::CmpXchg {
            st.set(Reg::R0, fetched);
        } else if op.fetches() {
            self.write_check(pc, src)?;
            st.set(src, fetched);
        }
        Ok(())
    }

    /// Bytes `[p, p + n)` must be readable (helper memory arguments).
    fn check_mem_read(&mut self, pc: usize, st: &State, p: RegType, n: u32, what: &str) -> Result<(), VerifyError> {
        if n == 0 {
            return Err(err(pc, "call.arg_type", format!("{what}: zero-sized memory")));
        }
        match p {
            RegType::Stack { frame, off } => {
                let lo = off as i64;
                let hi = lo + n as i64;
                if lo < -(BPF_MAX_STACK_BYTES as i64) || hi > 0 {
                    return Err(err(
                        pc,
                        "mem.stack_out_of_bounds",
                        format!("{what}: invalid indirect stack access off={lo} size={n}"),
                    ));
                }
                let sp = st.frames[frame as usize].subprog;
                self.stack_depth[sp] = self.stack_depth[sp].max((-lo) as u32);
                let fr = &st.frames[frame as usize];
                for b in lo..hi {
                    let at = (b + BPF_MAX_STACK_BYTES as i64) as usize;
                    let ok = match fr.stack[at / 8] {
                        Slot::Spill(RegType::Scalar(_)) => true,
                        Slot::Spill(_) => false,
                        Slot::Bytes(m) => m & (1 << (at % 8)) != 0,
                    };
                    if !ok {
                        return Err(err(
                            pc,
                            "mem.uninit_stack_read",
                            format!("{what}: invalid indirect read from stack off={b}"),
                        ));
                    }
                }
                Ok(())
            }
            RegType::Packet { .. } | RegType::MapValue { .. } | RegType::Mem { .. } => {
                // check the first and last byte with the ordinary rules
                let n = n as i64;
                let last = i16::try_from(n - 1).map_err(|_| err(pc, "call.arg_type", "size too large"))?;
                self.access(pc, st, p, 0, Size::B, false)?;
                self.access(pc, st, p, last, Size::B, false)?;
                Ok(())
            }
            other => Err(err(
                pc,
                "call.arg_type",
                format!("{what}: expected pointer to memory, got {}", other.name()),
            )),
        }
    }

    fn arg(&self, pc: usize, st: &State, i: usize) -> Result<RegType, VerifyError> {
        let v = st.cur().regs[i];
        if matches!(v, RegType::NotInit) {
            return Err(err(pc, "reg.uninit_read", format!("helper argument r{i} !read_ok")));
        }
        Ok(v)
    }

    fn map_arg(&self, pc: usize, st: &State, i: usize) -> Result<u32, VerifyError> {
        match self.arg(pc, st, i)? {
            RegType::MapPtr { map } => Ok(map),
            other => Err(err(
                pc,
                "call.arg_type",
                format!("r{i}: expected map pointer, got {}", other.name()),
            )),
        }
    }

    fn scalar_arg(&self, pc: usize, st: &State, i: usize) -> Result<Scalar, VerifyError> {
        match self.arg(pc, st, i)? {
            RegType::Scalar(s) => Ok(s),
            other => Err(err(
                pc,
                "call.arg_type",
                format!("r{i}: expected scalar, got {}", other.name()),
            )),
        }
    }

    fn const_arg(&self, pc: usize, st: &State, i: usize) -> Result<u64, VerifyError> {
        self.scalar_arg(pc, st, i)?.as_const().ok_or_else(|| {
            err(pc, "call.arg_type", format!("r{i}: expected a known constant"))
        })
    }

    fn release(&self, pc: usize, st: &mut State, ref_id: u32) -> Result<(), VerifyError> {
        let Some(pos) = st.refs.iter().position(|r| r.id == ref_id) else {
            return Err(err(pc, "ref.release_invalid", "release of a reference that is not held"));
        };
        st.refs.remove(pos);
        st.for_each_value(|r| {
            if r.ref_id() == Some(ref_id) {
                *r = RegType::NotInit;
            }
        });
        Ok(())
    }

    fn helper(&mut self, pc: usize, st: &mut State, id: i32) -> Result<(), VerifyError> {
        let ret = match id {
            h::MAP_LOOKUP_ELEM | h::MAP_UPDATE_ELEM | h::MAP_DELETE_ELEM => {
                let map = self.map_arg(pc, st, 1)?;
                let def = self.prog.maps[map as usize].clone();
                if def.kind == MapKind::RingBuf {
                    return Err(err(
                        pc,
                        "call.arg_type",
                        format!("helper {id} cannot be used with ringbuf map {}", def.name),
                    ));
                }
                let key = self.arg(pc, st, 2)?;
                self.check_mem_read(pc, st, key, def.key_size, "map key")?;
                match id {
                    h::MAP_LOOKUP_ELEM => RegType::MapValueOrNull {
                        map,
                        id: st.fresh_id(),
                    },
                    h::MAP_UPDATE_ELEM => {
                        let val = self.arg(pc, st, 3)?;
                        self.check_mem_read(pc, st, val, def.value_size, "map value")?;
                        self.scalar_arg(pc, st, 4)?;
                        RegType::Scalar(Scalar::UNKNOWN)
                    }
                    _ => {
                        if def.kind.is_array() {
                            return Err(err(
                                pc,
                                "call.arg_type",
                                "bpf_map_delete_elem on an array map",
                            ));
                        }
                        RegType::Scalar(Scalar::UNKNOWN)
                    }
                }
            }
            h::KTIME_GET_NS => RegType::Scalar(Scalar::UNKNOWN),
            h::GET_PRANDOM_U32 | h::GET_SMP_PROCESSOR_ID => {
                RegType::Scalar(Scalar::unknown_bits(32))
            }
            h::XDP_ADJUST_HEAD => {
                if !matches!(self.arg(pc, st, 1)?, RegType::Ctx) {
                    return Err(err(pc, "call.arg_type", "r1 must be the xdp_md context"));
                }
                self.scalar_arg(pc, st, 2)?;
                // every packet pointer is invalidated (`clear_all_pkt_pointers`)
                st.for_each_value(|r| {
                    if matches!(r, RegType::Packet { .. } | RegType::PacketEnd) {
                        *r = RegType::NotInit;
                    }
                });
                st.pkt_ranges.clear();
                RegType::Scalar(Scalar::UNKNOWN)
            }
            h::RINGBUF_RESERVE => {
                let map = self.map_arg(pc, st, 1)?;
                let def = &self.prog.maps[map as usize];
                if def.kind != MapKind::RingBuf {
                    return Err(err(pc, "call.arg_type", "bpf_ringbuf_reserve needs a ringbuf map"));
                }
                let size = self.const_arg(pc, st, 2)?;
                if size == 0 || size > def.max_entries as u64 / 2 {
                    return Err(err(pc, "call.arg_type", format!("invalid ringbuf record size {size}")));
                }
                if self.const_arg(pc, st, 3)? != 0 {
                    return Err(err(pc, "call.arg_type", "bpf_ringbuf_reserve flags must be 0"));
                }
                let ref_id = st.fresh_id();
                let frame = (st.frames.len() - 1) as u8;
                st.refs.push(RefEntry {
                    id: ref_id,
                    kind: RefKind::Ringbuf,
                    acquired_at: pc,
                    frame,
                });
                RegType::MemOrNull {
                    size: size as u32,
                    id: st.fresh_id(),
                    ref_id,
                }
            }
            h::RINGBUF_SUBMIT | h::RINGBUF_DISCARD => {
                let p = self.arg(pc, st, 1)?;
                let RegType::Mem { off, ref_id, .. } = p else {
                    return Err(err(
                        pc,
                        "ref.release_invalid",
                        format!("r1 must be a reserved ringbuf record, got {}", p.name()),
                    ));
                };
                if off != 0 {
                    return Err(err(
                        pc,
                        "ref.release_offset",
                        "r1 must have zero offset when passed to release func",
                    ));
                }
                self.scalar_arg(pc, st, 2)?;
                self.release(pc, st, ref_id)?;
                RegType::NotInit
            }
            h::RINGBUF_OUTPUT => {
                let map = self.map_arg(pc, st, 1)?;
                if self.prog.maps[map as usize].kind != MapKind::RingBuf {
                    return Err(err(pc, "call.arg_type", "bpf_ringbuf_output needs a ringbuf map"));
                }
                let size = self.const_arg(pc, st, 3)?;
                let data = self.arg(pc, st, 2)?;
                let size32 = u32::try_from(size)
                    .map_err(|_| err(pc, "call.arg_type", "ringbuf output size too large"))?;
                self.check_mem_read(pc, st, data, size32, "ringbuf output data")?;
                self.scalar_arg(pc, st, 4)?;
                RegType::Scalar(Scalar::UNKNOWN)
            }
            h::SK_LOOKUP_TCP => {
                if !matches!(self.arg(pc, st, 1)?, RegType::Ctx) {
                    return Err(err(pc, "call.arg_type", "r1 must be the xdp_md context"));
                }
                let size = self.const_arg(pc, st, 3)?;
                if size != h::SOCK_TUPLE_IPV4 as u64 && size != h::SOCK_TUPLE_IPV6 as u64 {
                    return Err(err(pc, "call.arg_type", format!("invalid tuple size {size}")));
                }
                let tuple = self.arg(pc, st, 2)?;
                self.check_mem_read(pc, st, tuple, size as u32, "socket tuple")?;
                self.scalar_arg(pc, st, 4)?;
                if self.const_arg(pc, st, 5)? != 0 {
                    return Err(err(pc, "call.arg_type", "bpf_sk_lookup_tcp flags must be 0"));
                }
                let ref_id = st.fresh_id();
                let frame = (st.frames.len() - 1) as u8;
                st.refs.push(RefEntry {
                    id: ref_id,
                    kind: RefKind::Socket,
                    acquired_at: pc,
                    frame,
                });
                RegType::SocketOrNull {
                    id: st.fresh_id(),
                    ref_id,
                }
            }
            h::SK_RELEASE => {
                let p = self.arg(pc, st, 1)?;
                let RegType::Socket { ref_id } = p else {
                    return Err(err(
                        pc,
                        "ref.release_invalid",
                        format!("r1 must be an acquired socket, got {}", p.name()),
                    ));
                };
                self.release(pc, st, ref_id)?;
                RegType::Scalar(Scalar::UNKNOWN)
            }
            other => {
                return Err(err(
                    pc,
                    "call.unknown_helper",
                    format!("helper {other} is not supported"),
                ));
            }
        };
        for i in 1..=5 {
            st.cur_mut().regs[i] = RegType::NotInit;
        }
        let ret = if matches!(ret, RegType::NotInit) {
            // void helpers still clobber r0
            RegType::Scalar(Scalar::UNKNOWN)
        } else {
            ret
        };
        st.set(Reg::R0, ret);
        Ok(())
    }

    fn exit(&mut self, pc: usize, st: &mut State) -> Result<Step, VerifyError> {
        let r0 = self.read(pc, st, Reg::R0)?;
        let depth = st.frames.len() - 1;
        if depth == 0 {
            if !matches!(r0, RegType::Scalar(_)) {
                return Err(err(
                    pc,
                    "exit.r0_not_scalar",
                    format!("R0 at program exit is {}, not a scalar", r0.name()),
                ));
            }
            if let Some(r) = st.refs.first() {
                return Err(err(
                    pc,
                    "ref.leak",
                    format!(
                        "Unreleased reference id={} ({:?}) acquired at insn {}",
                        r.id, r.kind, r.acquired_at
                    ),
                ));
            }
            return Ok(Step::Done);
        }
        if let Some(r) = st.refs.iter().find(|r| r.frame as usize == depth) {
            return Err(err(
                pc,
                "ref.leak",
                format!(
                    "reference id={} acquired at insn {} is still held when its subprogram returns",
                    r.id, r.acquired_at
                ),
            ));
        }
        // nothing may keep pointing into the dying frame
        let dying = depth as u8;
        let points_into = |r: &RegType| matches!(r, RegType::Stack { frame, .. } if *frame == dying);
        if points_into(&r0) {
            return Err(err(pc, "exit.stack_ptr_escape", "returning a pointer to the callee stack"));
        }
        for f in &st.frames[..depth] {
            let bad = f.regs.iter().any(points_into)
                || f.stack
                    .iter()
                    .any(|s| matches!(s, Slot::Spill(r) if points_into(r)));
            if bad {
                return Err(err(
                    pc,
                    "exit.stack_ptr_escape",
                    "a caller holds a pointer to the callee stack",
                ));
            }
        }
        let callee = st.frames.pop().expect("callee frame");
        let ret_pc = callee.return_pc.expect("callee has a return pc");
        let caller = st.cur_mut();
        caller.regs[0] = r0;
        for i in 1..=5 {
            caller.regs[i] = RegType::NotInit;
        }
        Ok(Step::Jump(ret_pc))
    }

    #[allow(clippy::too_many_arguments)]
    fn cond(
        &mut self,
        pc: usize,
        st: &mut State,
        wide: bool,
        op: JmpOp,
        dst: Reg,
        src: Operand,
        target: usize,
        fall: usize,
    ) -> Result<Step, VerifyError> {
        let a = self.read(pc, st, dst)?;
        let b = self.operand(pc, st, wide, src)?;
        let src_reg = match src {
            Operand::Reg(r) => Some(r),
            Operand::Imm(_) => None,
        };
        let (taken, fallen): (Option<State>, Option<State>) = match (a, b) {
            (RegType::Scalar(x), RegType::Scalar(y)) => {
                let mut t = None;
                let mut f = None;
                if let Some((x1, y1)) = refine_jmp(op, wide, x, y, true) {
                    let mut s = st.clone();
                    s.set(dst, RegType::Scalar(x1));
                    if let Some(r) = src_reg {
                        s.set(r, RegType::Scalar(y1));
                    }
                    t = Some(s);
                }
                if let Some((x1, y1)) = refine_jmp(op, wide, x, y, false) {
                    let mut s = st.clone();
                    s.set(dst, RegType::Scalar(x1));
                    if let Some(r) = src_reg {
                        s.set(r, RegType::Scalar(y1));
                    }
                    f = Some(s);
                }
                (t, f)
            }
            (p, RegType::Scalar(z)) | (RegType::Scalar(z), p)
                if is_or_null(&p) && z.as_const() == Some(0) && matches!(op, JmpOp::Eq | JmpOp::Ne) =>
            {
                if !wide {
                    return Err(err(pc, "jmp.pointer_compare", "32-bit null check of a pointer"));
                }
                let mut null = st.clone();
                let mut nonnull = st.clone();
                mark_null(pc, self, &mut null, &p, true)?;
                mark_null(pc, self, &mut nonnull, &p, false)?;
                if op == JmpOp::Eq {
                    (Some(null), Some(nonnull))
                } else {
                    (Some(nonnull), Some(null))
                }
            }
            (RegType::Packet { id, off, .. }, RegType::PacketEnd)
            | (RegType::PacketEnd, RegType::Packet { id, off, .. }) => {
                if !wide {
                    return Err(err(pc, "jmp.pointer_compare", "32-bit packet pointer comparison"));
                }
                let op = if matches!(a, RegType::PacketEnd) {
                    op.swap()
                } else {
                    op
                };
                // `op` now reads `pkt op pkt_end`
                let mut t = st.clone();
                let mut f = st.clone();
                let good = (0..=MAX_PACKET_OFF as i32).contains(&off);
                match op {
                    JmpOp::Gt | JmpOp::Ge => {
                        if good {
                            f.set_range(id, off as u32);
                        }
                    }
                    JmpOp::Lt | JmpOp::Le => {
                        if good {
                            t.set_range(id, off as u32);
                        }
                    }
                    JmpOp::Eq | JmpOp::Ne => {}
                    _ => {
                        return Err(err(
                            pc,
                            "jmp.pointer_compare",
                            "signed or bit-test comparison of packet pointers",
                        ));
                    }
                }
                (Some(t), Some(f))
            }
            (x, y) => {
                return Err(err(
                    pc,
                    "jmp.pointer_compare",
                    format!("comparison between {} and {} prohibited", x.name(), y.name()),
                ));
            }
        };
        match (taken, fallen) {
            (Some(t), Some(f)) => {
                *st = f;
                Ok(Step::Fork {
                    taken: (target, t),
                    fall,
                })
            }
            (Some(t), None) => {
                *st = t;
                Ok(Step::Jump(target))
            }
            (None, Some(f)) => {
                *st = f;
                Ok(Step::Next(fall))
            }
            (None, None) => Err(err(pc, "internal.no_successor", "both branches infeasible")),
        }
    }
}

enum Target {
    CtxPtr(i32),
    Scalar(Size),
    Stack { frame: u8, at: usize },
}

fn byte_mask(first: usize, n: usize) -> u8 {
    let m: u16 = ((1u16 << n) - 1) << first;
    m as u8
}

fn round_stack(d: u32) -> u32 {
    d.max(1).div_ceil(32) * 32
}

fn is_or_null(p: &RegType) -> bool {
    matches!(
        p,
        RegType::MapValueOrNull { .. } | RegType::MemOrNull { .. } | RegType::SocketOrNull { .. }
    )
}

fn null_id(p: &RegType) -> u32 {
    match *p {
        RegType::MapValueOrNull { id, .. }
        | RegType::MemOrNull { id, .. }
        | RegType::SocketOrNull { id, .. } => id,
        _ => 0,
    }
}

/// Resolve every copy of a maybe-null pointer (same id) on one branch.
fn mark_null(
    pc: usize,
    v: &Verifier<'_>,
    st: &mut State,
    p: &RegType,
    is_null: bool,
) -> Result<(), VerifyError> {
    let id = null_id(p);
    let _ = (pc, v);
    st.for_each_value(|r| {
        if null_id(r) == id && is_or_null(r) {
            *r = if is_null {
                RegType::Scalar(Scalar::konst(0))
            } else {
                match *r {
                    RegType::MapValueOrNull { map, .. } => RegType::MapValue {
                        map,
                        off: 0,
                        var: Scalar::konst(0),
                    },
                    RegType::MemOrNull { size, ref_id, .. } => RegType::Mem {
                        size,
                        off: 0,
                        ref_id,
                    },
                    RegType::SocketOrNull { ref_id, .. } => RegType::Socket { ref_id },
                    other => other,
                }
            };
        }
    });
    if is_null && let Some(rid) = p.ref_id() {
        // a failed acquire holds nothing (`release_reference_state`)
        st.refs.retain(|r| r.id != rid);
    }
    Ok(())
}

/// The abstract result of a scalar ALU op on already-truncated operands.
pub fn scalar_alu(op: AluOp, wide: bool, a: Scalar, b: Scalar) -> Scalar {
    let bits = if wide { 64 } else { 32 };
    let shift = |b: Scalar| b.as_const().map(|c| (c & (bits - 1)) as u32);
    match op {
        AluOp::Add => Scalar::add(a, b),
        AluOp::Sub => Scalar::sub(a, b),
        AluOp::Mul => Scalar::mul(a, b),
        AluOp::Div => Scalar::udiv(a, b),
        AluOp::Mod => Scalar::umod(a, b),
        AluOp::Or => Scalar::or(a, b),
        AluOp::And => Scalar::and(a, b),
        AluOp::Xor => Scalar::xor(a, b),
        AluOp::Lsh => match shift(b) {
            Some(s) => Scalar::lsh(a, s),
            None => Scalar::UNKNOWN,
        },
        AluOp::Rsh => match shift(b) {
            Some(s) => Scalar::rsh(a, s),
            None => Scalar::urange(0, a.umax),
        },
        AluOp::Arsh => match (shift(b), wide) {
            (Some(s), true) => Scalar::arsh(a, s),
            (Some(s), false) => match a.as_const() {
                Some(c) => Scalar::konst(((c as u32 as i32) >> s) as u32 as u64),
                None => Scalar::unknown_bits(32),
            },
            (None, _) => Scalar::UNKNOWN,
        },
        AluOp::SDiv | AluOp::SMod => match (a.as_const(), b.as_const()) {
            (Some(x), Some(y)) => Scalar::konst(crate::interp::alu_concrete(op, wide, x, y)),
            _ => Scalar::UNKNOWN,
        },
        AluOp::Mov | AluOp::MovSx(_) | AluOp::Neg => unreachable!("handled by caller"),
    }
}

fn exclude(s: Scalar, c: u64) -> Option<Scalar> {
    let mut s = s;
    if s.umin == c {
        s.umin = c.checked_add(1)?;
    }
    if s.umax == c {
        s.umax = c.checked_sub(1)?;
    }
    let ci = c as i64;
    if s.smin == ci {
        s.smin = ci.checked_add(1)?;
    }
    if s.smax == ci {
        s.smax = ci.checked_sub(1)?;
    }
    s.normalize()
}

/// Refine `(a, b)` assuming `a op b` holds (`holds = true`) or does not.
/// `None` means that outcome is impossible.
pub fn refine_jmp(op: JmpOp, wide: bool, a: Scalar, b: Scalar, holds: bool) -> Option<(Scalar, Scalar)> {
    if !wide {
        // 32-bit comparisons: exact for constants; otherwise refine only
        // when both values provably fit so 32- and 64-bit order agree.
        if let (Some(x), Some(y)) = (a.as_const(), b.as_const()) {
            let r = crate::interp::cmp_concrete(op, false, x, y);
            return (r == holds).then_some((a, b));
        }
        let fits = match op {
            JmpOp::SGt | JmpOp::SGe | JmpOp::SLt | JmpOp::SLe => {
                a.umax <= i32::MAX as u64 && b.umax <= i32::MAX as u64
            }
            _ => a.umax <= u32::MAX as u64 && b.umax <= u32::MAX as u64,
        };
        if !fits {
            return Some((a, b));
        }
    }
    if !holds {
        if op == JmpOp::Set {
            // a & b == 0
            return match b.as_const() {
                Some(c) => {
                    let na = Scalar::meet(
                        a,
                        Scalar {
                            var: Tnum { value: 0, mask: !c },
                            ..Scalar::UNKNOWN
                        },
                    )?;
                    Some((na, b))
                }
                None => match a.as_const() {
                    Some(c) => {
                        let nb = Scalar::meet(
                            b,
                            Scalar {
                                var: Tnum { value: 0, mask: !c },
                                ..Scalar::UNKNOWN
                            },
                        )?;
                        Some((a, nb))
                    }
                    None => Some((a, b)),
                },
            };
        }
        return refine_jmp(op.negate(), true, a, b, true);
    }
    let mut a = a;
    let mut b = b;
    match op {
        JmpOp::Eq => {
            let m = Scalar::meet(a, b)?;
            return Some((m, m));
        }
        JmpOp::Ne => {
            if let (Some(x), Some(y)) = (a.as_const(), b.as_const()) {
                return (x != y).then_some((a, b));
            }
            if let Some(c) = b.as_const() {
                a = exclude(a, c)?;
            } else if let Some(c) = a.as_const() {
                b = exclude(b, c)?;
            }
            return Some((a, b));
        }
        JmpOp::Set => {
            if Tnum::and(a.var, b.var) == Tnum::konst(0) {
                return None;
            }
            return Some((a, b));
        }
        JmpOp::Gt => {
            a.umin = a.umin.max(b.umin.checked_add(1)?);
            b.umax = b.umax.min(a.umax.checked_sub(1)?);
        }
        JmpOp::Ge => {
            a.umin = a.umin.max(b.umin);
            b.umax = b.umax.min(a.umax);
        }
        JmpOp::Lt => {
            let (nb, na) = refine_jmp(JmpOp::Gt, true, b, a, true)?;
            return Some((na, nb));
        }
        JmpOp::Le => {
            let (nb, na) = refine_jmp(JmpOp::Ge, true, b, a, true)?;
            return Some((na, nb));
        }
        JmpOp::SGt => {
            a.smin = a.smin.max(b.smin.checked_add(1)?);
            b.smax = b.smax.min(a.smax.checked_sub(1)?);
        }
        JmpOp::SGe => {
            a.smin = a.smin.max(b.smin);
            b.smax = b.smax.min(a.smax);
        }
        JmpOp::SLt => {
            let (nb, na) = refine_jmp(JmpOp::SGt, true, b, a, true)?;
            return Some((na, nb));
        }
        JmpOp::SLe => {
            let (nb, na) = refine_jmp(JmpOp::SGe, true, b, a, true)?;
            return Some((na, nb));
        }
    }
    Some((a.normalize()?, b.normalize()?))
}

/// Used by tests: the abstract value of a constant.
pub fn sext_imm(imm: i32, bits: u32) -> u64 {
    sext_const(imm as u32 as u64, bits)
}
