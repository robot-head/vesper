//! The port build's side of the boundary (`cfg(vesper_port)`): the names the
//! generated binding (`port/binding.rs`) uses for the Vesper types, and the
//! conversions between the crate's public types and the port's.
//!
//! Most Vesper types have the crate's own layout and field names, so they
//! are the crate's types under the binding's names. Three differ: the port
//! numbers registers with a plain `u8` where the crate has [`Reg`]
//! (`PortInsn`, `PortOperand`), names the reserved field of a
//! `DecodeError` by number (`PortDecodeError`), and holds a MAC address as
//! the low 48 bits of a word (`PortEthWire`).

#![allow(non_camel_case_types)]

pub(crate) use crate::port_binding as ffi;

use crate::insn::{self, AluOp, AtomicOp, CallTarget, Endian, Imm64, JmpOp, RawInsn, Reg, Size};
use crate::wire::{EthIpv4TcpWire, WireError};

pub use crate::insn::{
    AluOp as vesper__ebpf__insn__AluOp, AtomicOp as vesper__ebpf__insn__AtomicOp,
    CallTarget as vesper__ebpf__insn__CallTarget, Endian as vesper__ebpf__insn__Endian,
    Imm64 as vesper__ebpf__insn__Imm64, JmpOp as vesper__ebpf__insn__JmpOp,
    RawInsn as vesper__ebpf__insn__RawInsn, Size as vesper__ebpf__insn__Size,
};
pub use crate::program::{
    MapKind as vesper__ebpf__program__MapKind, ProgramType as vesper__ebpf__program__ProgramType,
};
pub use crate::scalar::{
    Scalar as vesper__ebpf__scalar__Scalar, Tnum as vesper__ebpf__scalar__Tnum,
};
pub use crate::wire::{
    SynDropAlertWire as vesper__ebpf__wire__SynDropAlertWire,
    WireError as vesper__ebpf__wire__WireError,
};
pub use PortDecodeError as vesper__ebpf__insn__DecodeError;
pub use PortEncoded as vesper__ebpf__insn__Encoded;
pub use PortEthWire as vesper__ebpf__wire__EthIpv4TcpWire;
pub use PortInsn as vesper__ebpf__insn__Insn;
pub use PortOperand as vesper__ebpf__insn__Operand;

/// `insn::Operand` with a register number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortOperand {
    Reg(u8),
    Imm(i32),
}

/// `insn::Insn` with register numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortInsn {
    Alu {
        wide: bool,
        op: AluOp,
        dst: u8,
        src: PortOperand,
    },
    Endian {
        kind: Endian,
        bits: u8,
        dst: u8,
    },
    Ja {
        off: i32,
    },
    Jcond {
        wide: bool,
        op: JmpOp,
        dst: u8,
        src: PortOperand,
        off: i16,
    },
    Call(CallTarget),
    Exit,
    LdImm64 {
        dst: u8,
        imm: Imm64,
    },
    Ldx {
        size: Size,
        dst: u8,
        src: u8,
        off: i16,
        sign_extend: bool,
    },
    St {
        size: Size,
        dst: u8,
        off: i16,
        imm: i32,
    },
    Stx {
        size: Size,
        dst: u8,
        src: u8,
        off: i16,
    },
    Atomic {
        size: Size,
        op: AtomicOp,
        dst: u8,
        src: u8,
        off: i16,
    },
    LdLegacy {
        size: Size,
        indirect: bool,
        src: u8,
        imm: i32,
    },
}

/// `insn::DecodeError` with the reserved field by number (0 dst, 1 src,
/// 2 off, 3 imm).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortDecodeError {
    TruncatedProgram { len: usize },
    BadRegister { pc: usize, reg: u8 },
    InvalidOpcode { pc: usize, code: u8 },
    ReservedFieldSet { pc: usize, field: u8 },
    BrokenLdImm64 { pc: usize },
}

/// One or two encoded slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortEncoded {
    pub n: u64,
    pub lo: RawInsn,
    pub hi: RawInsn,
}

/// `wire::EthIpv4TcpWire` with each MAC address in the low 48 bits of a
/// word, first byte most significant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortEthWire {
    pub dst_mac: u64,
    pub src_mac: u64,
    pub ether_type: u16,
    pub ver_ihl: u8,
    pub tos: u8,
    pub total_len: u16,
    pub ident: u16,
    pub frag_off: u16,
    pub ttl: u8,
    pub proto: u8,
    pub ip_checksum: u16,
    pub src_ipv4: u32,
    pub dst_ipv4: u32,
    pub src_port: u16,
    pub dst_port: u16,
    pub seq_num: u32,
    pub ack_num: u32,
    pub data_offset_flags: u16,
    pub window_size: u16,
    pub tcp_checksum: u16,
    pub urgent_ptr: u16,
}

// The port checks every register number it decodes, and the crate's
// registers are in range by construction.
fn reg(n: u8) -> Reg {
    Reg::new(n).expect("the port decodes registers 0..=10 only")
}

fn operand(o: PortOperand) -> insn::Operand {
    match o {
        PortOperand::Reg(r) => insn::Operand::Reg(reg(r)),
        PortOperand::Imm(i) => insn::Operand::Imm(i),
    }
}

fn port_operand(o: insn::Operand) -> PortOperand {
    match o {
        insn::Operand::Reg(r) => PortOperand::Reg(r.num()),
        insn::Operand::Imm(i) => PortOperand::Imm(i),
    }
}

impl From<PortInsn> for insn::Insn {
    fn from(i: PortInsn) -> Self {
        use insn::Insn as I;
        match i {
            PortInsn::Alu { wide, op, dst, src } => I::Alu {
                wide,
                op,
                dst: reg(dst),
                src: operand(src),
            },
            PortInsn::Endian { kind, bits, dst } => I::Endian {
                kind,
                bits,
                dst: reg(dst),
            },
            PortInsn::Ja { off } => I::Ja { off },
            PortInsn::Jcond {
                wide,
                op,
                dst,
                src,
                off,
            } => I::Jcond {
                wide,
                op,
                dst: reg(dst),
                src: operand(src),
                off,
            },
            PortInsn::Call(t) => I::Call(t),
            PortInsn::Exit => I::Exit,
            PortInsn::LdImm64 { dst, imm } => I::LdImm64 { dst: reg(dst), imm },
            PortInsn::Ldx {
                size,
                dst,
                src,
                off,
                sign_extend,
            } => I::Ldx {
                size,
                dst: reg(dst),
                src: reg(src),
                off,
                sign_extend,
            },
            PortInsn::St {
                size,
                dst,
                off,
                imm,
            } => I::St {
                size,
                dst: reg(dst),
                off,
                imm,
            },
            PortInsn::Stx {
                size,
                dst,
                src,
                off,
            } => I::Stx {
                size,
                dst: reg(dst),
                src: reg(src),
                off,
            },
            PortInsn::Atomic {
                size,
                op,
                dst,
                src,
                off,
            } => I::Atomic {
                size,
                op,
                dst: reg(dst),
                src: reg(src),
                off,
            },
            PortInsn::LdLegacy {
                size,
                indirect,
                src,
                imm,
            } => I::LdLegacy {
                size,
                indirect,
                src: reg(src),
                imm,
            },
        }
    }
}

impl From<&insn::Insn> for PortInsn {
    fn from(i: &insn::Insn) -> Self {
        use insn::Insn as I;
        match *i {
            I::Alu { wide, op, dst, src } => PortInsn::Alu {
                wide,
                op,
                dst: dst.num(),
                src: port_operand(src),
            },
            I::Endian { kind, bits, dst } => PortInsn::Endian {
                kind,
                bits,
                dst: dst.num(),
            },
            I::Ja { off } => PortInsn::Ja { off },
            I::Jcond {
                wide,
                op,
                dst,
                src,
                off,
            } => PortInsn::Jcond {
                wide,
                op,
                dst: dst.num(),
                src: port_operand(src),
                off,
            },
            I::Call(t) => PortInsn::Call(t),
            I::Exit => PortInsn::Exit,
            I::LdImm64 { dst, imm } => PortInsn::LdImm64 {
                dst: dst.num(),
                imm,
            },
            I::Ldx {
                size,
                dst,
                src,
                off,
                sign_extend,
            } => PortInsn::Ldx {
                size,
                dst: dst.num(),
                src: src.num(),
                off,
                sign_extend,
            },
            I::St {
                size,
                dst,
                off,
                imm,
            } => PortInsn::St {
                size,
                dst: dst.num(),
                off,
                imm,
            },
            I::Stx {
                size,
                dst,
                src,
                off,
            } => PortInsn::Stx {
                size,
                dst: dst.num(),
                src: src.num(),
                off,
            },
            I::Atomic {
                size,
                op,
                dst,
                src,
                off,
            } => PortInsn::Atomic {
                size,
                op,
                dst: dst.num(),
                src: src.num(),
                off,
            },
            I::LdLegacy {
                size,
                indirect,
                src,
                imm,
            } => PortInsn::LdLegacy {
                size,
                indirect,
                src: src.num(),
                imm,
            },
        }
    }
}

impl PortDecodeError {
    /// The port verifier's decode error words: variant (as declared), the
    /// position or length, and the register, code or field number.
    fn from_words(variant: u64, a: usize, b: u8) -> Self {
        match variant {
            0 => PortDecodeError::TruncatedProgram { len: a },
            1 => PortDecodeError::BadRegister { pc: a, reg: b },
            2 => PortDecodeError::InvalidOpcode { pc: a, code: b },
            3 => PortDecodeError::ReservedFieldSet { pc: a, field: b },
            _ => PortDecodeError::BrokenLdImm64 { pc: a },
        }
    }
}

impl From<PortDecodeError> for insn::DecodeError {
    fn from(e: PortDecodeError) -> Self {
        use insn::DecodeError as D;
        match e {
            PortDecodeError::TruncatedProgram { len } => D::TruncatedProgram { len },
            PortDecodeError::BadRegister { pc, reg } => D::BadRegister { pc, reg },
            PortDecodeError::InvalidOpcode { pc, code } => D::InvalidOpcode { pc, code },
            PortDecodeError::ReservedFieldSet { pc, field } => D::ReservedFieldSet {
                pc,
                field: ["dst", "src", "off", "imm"][usize::from(field & 3)],
            },
            PortDecodeError::BrokenLdImm64 { pc } => D::BrokenLdImm64 { pc },
        }
    }
}

// insn.rs

pub(crate) fn decode(slots: &[RawInsn], pc: usize) -> Result<insn::Insn, insn::DecodeError> {
    let at = |i: usize| slots.get(i).copied().unwrap_or_default();
    let hi = pc.checked_add(1).map_or(RawInsn::default(), at);
    ffi::insn_decode(pc, slots.len(), at(pc), hi)
        .map(insn::Insn::from)
        .map_err(insn::DecodeError::from)
}

pub(crate) fn decode_all(slots: &[RawInsn]) -> Result<Vec<Option<insn::Insn>>, insn::DecodeError> {
    let decoded = ffi::insn_decode_all(slots).map_err(insn::DecodeError::from)?;
    let mut out = Vec::with_capacity(slots.len());
    for i in decoded {
        let wide = matches!(i, PortInsn::LdImm64 { .. });
        out.push(Some(insn::Insn::from(i)));
        if wide {
            out.push(None);
        }
    }
    Ok(out)
}

pub(crate) fn encode(i: &insn::Insn) -> Vec<RawInsn> {
    let e = ffi::insn_encode(PortInsn::from(i));
    if e.n == 2 {
        vec![e.lo, e.hi]
    } else {
        vec![e.lo]
    }
}

pub(crate) fn slots_from_bytes(bytes: &[u8]) -> Result<Vec<RawInsn>, insn::DecodeError> {
    ffi::insn_slots_from_bytes(bytes).map_err(insn::DecodeError::from)
}

// wire.rs

fn mac_word(m: [u8; 6]) -> u64 {
    m.iter().fold(0, |w, &b| (w << 8) | u64::from(b))
}

fn mac_bytes(w: u64) -> [u8; 6] {
    let b = w.to_be_bytes();
    [b[2], b[3], b[4], b[5], b[6], b[7]]
}

pub(crate) fn eth_encode(w: &EthIpv4TcpWire) -> Vec<u8> {
    ffi::wire_eth_encode(PortEthWire {
        dst_mac: mac_word(w.dst_mac),
        src_mac: mac_word(w.src_mac),
        ether_type: w.ether_type,
        ver_ihl: w.ver_ihl,
        tos: w.tos,
        total_len: w.total_len,
        ident: w.ident,
        frag_off: w.frag_off,
        ttl: w.ttl,
        proto: w.proto,
        ip_checksum: w.ip_checksum,
        src_ipv4: w.src_ipv4,
        dst_ipv4: w.dst_ipv4,
        src_port: w.src_port,
        dst_port: w.dst_port,
        seq_num: w.seq_num,
        ack_num: w.ack_num,
        data_offset_flags: w.data_offset_flags,
        window_size: w.window_size,
        tcp_checksum: w.tcp_checksum,
        urgent_ptr: w.urgent_ptr,
    })
}

pub(crate) fn eth_decode(b: &[u8]) -> Result<EthIpv4TcpWire, WireError> {
    let w = ffi::wire_eth_decode(b)?;
    Ok(EthIpv4TcpWire {
        dst_mac: mac_bytes(w.dst_mac),
        src_mac: mac_bytes(w.src_mac),
        ether_type: w.ether_type,
        ver_ihl: w.ver_ihl,
        tos: w.tos,
        total_len: w.total_len,
        ident: w.ident,
        frag_off: w.frag_off,
        ttl: w.ttl,
        proto: w.proto,
        ip_checksum: w.ip_checksum,
        src_ipv4: w.src_ipv4,
        dst_ipv4: w.dst_ipv4,
        src_port: w.src_port,
        dst_port: w.dst_port,
        seq_num: w.seq_num,
        ack_num: w.ack_num,
        data_offset_flags: w.data_offset_flags,
        window_size: w.window_size,
        tcp_checksum: w.tcp_checksum,
        urgent_ptr: w.urgent_ptr,
    })
}

// program.rs

/// `MapDef::validate`'s message for the port's rule number.
pub(crate) fn map_validate(def: &crate::program::MapDef) -> Result<(), String> {
    let rule = ffi::program_map_validate(
        &def.name,
        def.kind,
        def.key_size,
        def.value_size,
        def.max_entries,
    );
    Err(match rule {
        0 => return Ok(()),
        1 => format!("map name {:?} must be 1..=15 ASCII bytes", def.name),
        2 => "ringbuf key and value size must be 0".into(),
        3 => "ringbuf size must be a power of two >= 4096".into(),
        4 => "max_entries and value_size must be nonzero".into(),
        5 => "array maps have 4-byte keys".into(),
        6 => "program arrays have 4-byte values".into(),
        7 => "key size must be 1..=512".into(),
        _ => "value size too large".into(),
    })
}

// callgraph.rs

pub(crate) fn check_stack(
    g: &crate::callgraph::CallGraph,
    root: crate::callgraph::FnId,
    limit: u32,
    max_frames: usize,
) -> Result<u32, crate::callgraph::StackError> {
    use crate::callgraph::{FnId, StackError};
    let ids: Vec<u64> = g.functions.iter().map(|n| u64::from(n.id.0)).collect();
    let frames: Vec<u32> = g.functions.iter().map(|n| n.frame_bytes).collect();
    let calls: Vec<u64> = g
        .calls
        .iter()
        .flat_map(|(a, b)| [u64::from(a.0), u64::from(b.0)])
        .collect();
    let indirect: Vec<u64> = g.indirect.iter().map(|f| u64::from(f.0)).collect();
    let out = ffi::callgraph_check_stack(
        &ids,
        &frames,
        &calls,
        &indirect,
        u64::from(root.0),
        limit,
        max_frames as u64,
    );
    // Every id and byte count the port returns came from a u32.
    let id = |w: u64| FnId(w as u32);
    let path = || out[2..].iter().map(|&w| id(w)).collect::<Vec<_>>();
    match out[0] {
        0 => Ok(out[1] as u32),
        1 => Err(StackError::Exceeded {
            bytes: out[1] as u32,
            path: path(),
        }),
        2 => Err(StackError::Recursion { path: path() }),
        3 => Err(StackError::TooDeep { path: path() }),
        4 => Err(StackError::UnknownFunction(id(out[1]))),
        _ => Err(StackError::IndirectCall(id(out[1]))),
    }
}

/// Every rule the verifier reports, so the port's rule text maps back to
/// the crate's `&'static str`.
const RULES: &[&str] = &[
    "alu.div_by_zero",
    "alu.invalid_shift",
    "alu.packet_var_too_large",
    "alu.pointer_arith",
    "alu.pointer_offset",
    "alu.stack_var_offset",
    "atomic.invalid_target",
    "atomic.misaligned",
    "atomic.pointer_operand",
    "call.arg_type",
    "call.depth",
    "call.graph",
    "call.helper_not_allowed",
    "call.kfunc_unknown",
    "call.kfunc_unsupported",
    "call.recursion",
    "call.tail_call_ref",
    "call.tail_call_subprogs",
    "call.unknown_helper",
    "cfg.cross_subprog_jump",
    "cfg.empty",
    "cfg.fallthrough",
    "cfg.jump_into_ld_imm64",
    "cfg.jump_out_of_range",
    "cfg.too_large",
    "cfg.unreachable",
    "complexity.limit",
    "decode.invalid",
    "exit.callback_ret",
    "exit.r0_not_scalar",
    "exit.stack_ptr_escape",
    "insn.unsupported",
    "internal.no_successor",
    "jmp.pointer_compare",
    "kfunc.arg_type",
    "kfunc.irq_order",
    "kfunc.license",
    "kfunc.preempt_region",
    "kfunc.rcu_nested",
    "kfunc.region",
    "kfunc.slot_in_use",
    "kfunc.unbalanced",
    "ld_imm64.bad_func",
    "ld_imm64.bad_map",
    "loop.unbounded",
    "loop.widened_precise",
    "map.invalid",
    "mem.ctx_invalid_access",
    "mem.ctx_st_imm",
    "mem.ctx_write",
    "mem.invalid_pointer",
    "mem.kfunc_object",
    "mem.map_value_out_of_bounds",
    "mem.null_arith",
    "mem.null_deref",
    "mem.packet_out_of_range",
    "mem.pointer_leak",
    "mem.pointer_partial_fill",
    "mem.pointer_partial_spill",
    "mem.rdonly_out_of_bounds",
    "mem.rdonly_write",
    "mem.ringbuf_out_of_bounds",
    "mem.stack_misaligned",
    "mem.stack_out_of_bounds",
    "mem.uninit_stack_read",
    "mem.unsupported_target",
    "prog.unsupported_type",
    "ref.leak",
    "ref.release_invalid",
    "ref.release_offset",
    "reg.uninit_read",
    "reg.write_fp",
    "stack.limit",
];

/// The kfuncs the verifier models, by the code the port matches on.
const VERIFIER_KFUNCS: &[&str] = &[
    "bpf_preempt_disable",
    "bpf_preempt_enable",
    "bpf_rcu_read_lock",
    "bpf_rcu_read_unlock",
    "bpf_local_irq_save",
    "bpf_local_irq_restore",
    "bpf_iter_num_new",
    "bpf_iter_num_next",
    "bpf_iter_num_destroy",
];

fn rule(text: &[u8]) -> &'static str {
    RULES
        .iter()
        .copied()
        .find(|r| r.as_bytes() == text)
        .unwrap_or_else(|| {
            panic!(
                "verifier rule {:?} is not in RULES",
                String::from_utf8_lossy(text)
            )
        })
}

/// `verifier::verify`: the program-type and map checks of `Verifier::new`
/// here, the rest in the port. The program crosses as its slots plus words
/// for the maps (kernel type, key, value, max entries, name range) and
/// kfunc references (slot, code, name range) over one text pool.
pub(crate) fn verify(
    prog: &crate::program::Program,
    policy: &crate::verifier::VerifierPolicy,
) -> Result<crate::verifier::VerifyReport, crate::verifier::VerifyError> {
    use crate::callgraph::{CallGraph, FnId, FnNode};
    use crate::verifier::{PT_REGS_SIZE, VerifyError, VerifyReport};

    if prog.prog_type == crate::program::ProgramType::Kprobe && PT_REGS_SIZE == 0 {
        return Err(VerifyError {
            insn: None,
            rule: "prog.unsupported_type",
            message: "kprobe programs: the pt_regs layout of this architecture is unknown".into(),
        });
    }
    for m in &prog.maps {
        m.validate().map_err(|e| VerifyError {
            insn: None,
            rule: "map.invalid",
            message: format!("map {}: {e}", m.name),
        })?;
    }
    let mut text = Vec::new();
    let mut maps = Vec::with_capacity(prog.maps.len() * 6);
    for m in &prog.maps {
        maps.extend([
            u64::from(m.kind.kernel_type()),
            u64::from(m.key_size),
            u64::from(m.value_size),
            u64::from(m.max_entries),
            text.len() as u64,
            m.name.len() as u64,
        ]);
        text.extend_from_slice(m.name.as_bytes());
    }
    let mut kfuncs = Vec::with_capacity(prog.kfuncs.len() * 4);
    for k in &prog.kfuncs {
        let code = VERIFIER_KFUNCS
            .iter()
            .position(|n| *n == k.name)
            .unwrap_or(VERIFIER_KFUNCS.len());
        kfuncs.extend([
            k.slot as u64,
            code as u64,
            text.len() as u64,
            k.name.len() as u64,
        ]);
        text.extend_from_slice(k.name.as_bytes());
    }
    let out = ffi::verifier_verify(
        &prog.insns,
        prog.prog_type,
        i64::from(PT_REGS_SIZE),
        crate::helpers::gpl_compatible(&prog.license),
        &maps,
        &kfuncs,
        &text,
        policy.max_insns_processed,
        policy.max_states_per_insn as u64,
        policy.widen_loops,
    );
    let out = out.as_ref();
    let w = out.words().to_vec();
    match w[0] {
        1 => Err(VerifyError {
            insn: (w[1] != u64::MAX).then_some(w[1] as usize),
            rule: rule(&out.rule().to_vec()),
            message: String::from_utf8(out.msg().to_vec()).expect("ASCII message"),
        }),
        2 => {
            let e = PortDecodeError::from_words(w[1], w[2] as usize, w[3] as u8);
            let pc = match e {
                PortDecodeError::TruncatedProgram { .. } => None,
                PortDecodeError::BadRegister { pc, .. }
                | PortDecodeError::InvalidOpcode { pc, .. }
                | PortDecodeError::ReservedFieldSet { pc, .. }
                | PortDecodeError::BrokenLdImm64 { pc } => Some(pc),
            };
            Err(VerifyError {
                insn: pc,
                rule: "decode.invalid",
                message: insn::DecodeError::from(e).to_string(),
            })
        }
        _ => {
            let nsub = w[7] as usize;
            let subprog_stack: Vec<(usize, u32)> = (0..nsub)
                .map(|i| (w[8 + 2 * i] as usize, w[9 + 2 * i] as u32))
                .collect();
            let mut graph = CallGraph::default();
            for (i, &(start, depth)) in subprog_stack.iter().enumerate() {
                let name = prog
                    .functions
                    .iter()
                    .find(|(_, e)| *e == start)
                    .map(|(n, _)| n.clone())
                    .unwrap_or_else(|| {
                        if i == 0 {
                            prog.name.clone()
                        } else {
                            format!("subprog_{start}")
                        }
                    });
                graph.functions.push(FnNode {
                    id: FnId(i as u32),
                    name,
                    frame_bytes: depth.max(1).div_ceil(32) * 32,
                });
            }
            for &[from, to] in w[8 + 2 * nsub..].as_chunks::<2>().0 {
                graph.calls.push((FnId(from as u32), FnId(to as u32)));
            }
            Ok(VerifyReport {
                insns_processed: w[1],
                states_stored: w[2] as usize,
                paths_completed: w[3],
                pruned: w[4],
                subprog_stack,
                worst_stack: w[6] as u32,
                call_graph: graph,
                widenings: w[5],
            })
        }
    }
}
