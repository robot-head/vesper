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
