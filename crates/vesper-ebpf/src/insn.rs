//! eBPF instruction encoding and decoding (Linux `include/uapi/linux/bpf.h`,
//! RFC 9669 instruction set).
//!
//! A program is a sequence of 8-byte slots. `LD_IMM64` is the only
//! instruction that occupies two slots. [`RawInsn`] is the wire form of one
//! slot; [`Insn`] is the typed form of one instruction. Decoding never
//! panics: malformed slots are [`DecodeError`]s.

use std::fmt;

/// Instruction classes (low three bits of the opcode).
pub mod class {
    pub const LD: u8 = 0x00;
    pub const LDX: u8 = 0x01;
    pub const ST: u8 = 0x02;
    pub const STX: u8 = 0x03;
    pub const ALU: u8 = 0x04;
    pub const JMP: u8 = 0x05;
    pub const JMP32: u8 = 0x06;
    pub const ALU64: u8 = 0x07;
}

/// `LD_IMM64` source-register pseudo values.
pub mod pseudo {
    pub const MAP_FD: u8 = 1;
    pub const MAP_VALUE: u8 = 2;
    pub const BTF_ID: u8 = 3;
    pub const FUNC: u8 = 4;
    pub const MAP_IDX: u8 = 5;
    pub const MAP_IDX_VALUE: u8 = 6;
    /// `CALL` with `src = 1`: BPF-to-BPF call, `imm` is a slot offset.
    pub const CALL: u8 = 1;
    /// `CALL` with `src = 2`: kernel function (kfunc) by BTF id.
    pub const KFUNC_CALL: u8 = 2;
}

/// One 8-byte instruction slot exactly as the kernel receives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct RawInsn {
    pub code: u8,
    /// Low nibble.
    pub dst: u8,
    /// High nibble.
    pub src: u8,
    pub off: i16,
    pub imm: i32,
}

impl RawInsn {
    pub const SIZE: usize = 8;

    pub fn new(code: u8, dst: u8, src: u8, off: i16, imm: i32) -> Self {
        Self {
            code,
            dst,
            src,
            off,
            imm,
        }
    }

    /// Little-endian wire bytes (the only byte order this crate targets).
    pub fn to_bytes(self) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0] = self.code;
        b[1] = (self.dst & 0x0f) | ((self.src & 0x0f) << 4);
        b[2..4].copy_from_slice(&self.off.to_le_bytes());
        b[4..8].copy_from_slice(&self.imm.to_le_bytes());
        b
    }

    pub fn from_bytes(b: [u8; 8]) -> Self {
        Self {
            code: b[0],
            dst: b[1] & 0x0f,
            src: b[1] >> 4,
            off: i16::from_le_bytes([b[2], b[3]]),
            imm: i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        }
    }

    pub fn class(self) -> u8 {
        self.code & 0x07
    }
}

/// Serialise slots to bytes.
pub fn slots_to_bytes(slots: &[RawInsn]) -> Vec<u8> {
    let mut out = Vec::with_capacity(slots.len() * 8);
    for s in slots {
        out.extend_from_slice(&s.to_bytes());
    }
    out
}

/// Parse bytes into slots. The length must be a multiple of eight.
pub fn slots_from_bytes(bytes: &[u8]) -> Result<Vec<RawInsn>, DecodeError> {
    if !bytes.len().is_multiple_of(8) {
        return Err(DecodeError::TruncatedProgram { len: bytes.len() });
    }
    let (chunks, _) = bytes.as_chunks::<8>();
    Ok(chunks.iter().map(|c| RawInsn::from_bytes(*c)).collect())
}

/// A register number known to be in `0..=10`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reg(u8);

impl Reg {
    pub const R0: Reg = Reg(0);
    pub const R1: Reg = Reg(1);
    pub const R2: Reg = Reg(2);
    pub const R3: Reg = Reg(3);
    pub const R4: Reg = Reg(4);
    pub const R5: Reg = Reg(5);
    pub const R6: Reg = Reg(6);
    pub const R7: Reg = Reg(7);
    pub const R8: Reg = Reg(8);
    pub const R9: Reg = Reg(9);
    /// Read-only frame pointer.
    pub const R10: Reg = Reg(10);

    pub fn new(n: u8) -> Option<Reg> {
        (n <= 10).then_some(Reg(n))
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn num(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// Second operand of ALU and conditional-jump instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operand {
    Reg(Reg),
    Imm(i32),
}

impl From<Reg> for Operand {
    fn from(r: Reg) -> Self {
        Operand::Reg(r)
    }
}

impl From<i32> for Operand {
    fn from(i: i32) -> Self {
        Operand::Imm(i)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AluOp {
    Add,
    Sub,
    Mul,
    Div,
    /// Signed division (`off = 1`, ISA v4).
    SDiv,
    Or,
    And,
    Lsh,
    Rsh,
    Neg,
    Mod,
    /// Signed modulo (`off = 1`, ISA v4).
    SMod,
    Xor,
    Mov,
    /// Sign-extending move from the low 8, 16 or 32 bits (ISA v4).
    MovSx(u8),
    Arsh,
}

impl AluOp {
    fn code(self) -> (u8, i16) {
        match self {
            AluOp::Add => (0x00, 0),
            AluOp::Sub => (0x10, 0),
            AluOp::Mul => (0x20, 0),
            AluOp::Div => (0x30, 0),
            AluOp::SDiv => (0x30, 1),
            AluOp::Or => (0x40, 0),
            AluOp::And => (0x50, 0),
            AluOp::Lsh => (0x60, 0),
            AluOp::Rsh => (0x70, 0),
            AluOp::Neg => (0x80, 0),
            AluOp::Mod => (0x90, 0),
            AluOp::SMod => (0x90, 1),
            AluOp::Xor => (0xa0, 0),
            AluOp::Mov => (0xb0, 0),
            AluOp::MovSx(bits) => (0xb0, bits as i16),
            AluOp::Arsh => (0xc0, 0),
        }
    }
}

/// Byte-order conversion (`BPF_END`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Endian {
    /// `BPF_ALU | BPF_END | BPF_TO_LE`: truncate to the width (LE target).
    ToLe,
    /// `BPF_ALU | BPF_END | BPF_TO_BE`: byte swap the low width bits.
    ToBe,
    /// `BPF_ALU64 | BPF_END`: unconditional byte swap (ISA v4).
    Swap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JmpOp {
    Eq,
    Gt,
    Ge,
    Set,
    Ne,
    SGt,
    SGe,
    Lt,
    Le,
    SLt,
    SLe,
}

impl JmpOp {
    fn code(self) -> u8 {
        match self {
            JmpOp::Eq => 0x10,
            JmpOp::Gt => 0x20,
            JmpOp::Ge => 0x30,
            JmpOp::Set => 0x40,
            JmpOp::Ne => 0x50,
            JmpOp::SGt => 0x60,
            JmpOp::SGe => 0x70,
            JmpOp::Lt => 0xa0,
            JmpOp::Le => 0xb0,
            JmpOp::SLt => 0xc0,
            JmpOp::SLe => 0xd0,
        }
    }

    fn from_code(op: u8) -> Option<JmpOp> {
        Some(match op {
            0x10 => JmpOp::Eq,
            0x20 => JmpOp::Gt,
            0x30 => JmpOp::Ge,
            0x40 => JmpOp::Set,
            0x50 => JmpOp::Ne,
            0x60 => JmpOp::SGt,
            0x70 => JmpOp::SGe,
            0xa0 => JmpOp::Lt,
            0xb0 => JmpOp::Le,
            0xc0 => JmpOp::SLt,
            0xd0 => JmpOp::SLe,
            _ => return None,
        })
    }

    /// The condition that holds on the fall-through edge.
    pub fn negate(self) -> JmpOp {
        match self {
            JmpOp::Eq => JmpOp::Ne,
            JmpOp::Ne => JmpOp::Eq,
            JmpOp::Gt => JmpOp::Le,
            JmpOp::Le => JmpOp::Gt,
            JmpOp::Ge => JmpOp::Lt,
            JmpOp::Lt => JmpOp::Ge,
            JmpOp::SGt => JmpOp::SLe,
            JmpOp::SLe => JmpOp::SGt,
            JmpOp::SGe => JmpOp::SLt,
            JmpOp::SLt => JmpOp::SGe,
            // `!(a & b != 0)` is not a JmpOp; callers special-case Set.
            JmpOp::Set => JmpOp::Set,
        }
    }

    /// The same relation with operands swapped (`a op b` == `b op' a`).
    pub fn swap(self) -> JmpOp {
        match self {
            JmpOp::Eq => JmpOp::Eq,
            JmpOp::Ne => JmpOp::Ne,
            JmpOp::Set => JmpOp::Set,
            JmpOp::Gt => JmpOp::Lt,
            JmpOp::Lt => JmpOp::Gt,
            JmpOp::Ge => JmpOp::Le,
            JmpOp::Le => JmpOp::Ge,
            JmpOp::SGt => JmpOp::SLt,
            JmpOp::SLt => JmpOp::SGt,
            JmpOp::SGe => JmpOp::SLe,
            JmpOp::SLe => JmpOp::SGe,
        }
    }
}

/// Memory access width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Size {
    B,
    H,
    W,
    DW,
}

impl Size {
    pub fn bytes(self) -> u32 {
        match self {
            Size::B => 1,
            Size::H => 2,
            Size::W => 4,
            Size::DW => 8,
        }
    }

    fn code(self) -> u8 {
        match self {
            Size::W => 0x00,
            Size::H => 0x08,
            Size::B => 0x10,
            Size::DW => 0x18,
        }
    }

    fn from_code(c: u8) -> Size {
        match c & 0x18 {
            0x00 => Size::W,
            0x08 => Size::H,
            0x10 => Size::B,
            _ => Size::DW,
        }
    }

    pub fn from_bytes(n: u32) -> Option<Size> {
        Some(match n {
            1 => Size::B,
            2 => Size::H,
            4 => Size::W,
            8 => Size::DW,
            _ => return None,
        })
    }
}

/// `BPF_ATOMIC` operations (the `imm` field).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AtomicOp {
    Add { fetch: bool },
    Or { fetch: bool },
    And { fetch: bool },
    Xor { fetch: bool },
    Xchg,
    CmpXchg,
}

impl AtomicOp {
    fn imm(self) -> i32 {
        let f = |b: bool| if b { 0x01 } else { 0 };
        match self {
            AtomicOp::Add { fetch } => f(fetch),
            AtomicOp::Or { fetch } => 0x40 | f(fetch),
            AtomicOp::And { fetch } => 0x50 | f(fetch),
            AtomicOp::Xor { fetch } => 0xa0 | f(fetch),
            AtomicOp::Xchg => 0xe1,
            AtomicOp::CmpXchg => 0xf1,
        }
    }

    fn from_imm(imm: i32) -> Option<AtomicOp> {
        Some(match imm {
            0x00 => AtomicOp::Add { fetch: false },
            0x01 => AtomicOp::Add { fetch: true },
            0x40 => AtomicOp::Or { fetch: false },
            0x41 => AtomicOp::Or { fetch: true },
            0x50 => AtomicOp::And { fetch: false },
            0x51 => AtomicOp::And { fetch: true },
            0xa0 => AtomicOp::Xor { fetch: false },
            0xa1 => AtomicOp::Xor { fetch: true },
            0xe1 => AtomicOp::Xchg,
            0xf1 => AtomicOp::CmpXchg,
            _ => return None,
        })
    }

    pub fn fetches(self) -> bool {
        match self {
            AtomicOp::Add { fetch }
            | AtomicOp::Or { fetch }
            | AtomicOp::And { fetch }
            | AtomicOp::Xor { fetch } => fetch,
            AtomicOp::Xchg | AtomicOp::CmpXchg => true,
        }
    }
}

/// What a 64-bit immediate load means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Imm64 {
    /// A plain 64-bit constant.
    Value(u64),
    /// `BPF_PSEUDO_MAP_FD`: a map. Before relocation, the immediate is an
    /// index into the program's map table; the loader rewrites it to a fd.
    MapFd(i32),
    /// `BPF_PSEUDO_MAP_VALUE` and the other pseudo forms are decoded but
    /// rejected by the verifier.
    Pseudo { src: u8, imm_lo: i32, imm_hi: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallTarget {
    /// Kernel helper by id (`src = 0`).
    Helper(i32),
    /// BPF-to-BPF call; target slot = pc + 1 + offset.
    Local(i32),
    /// Kernel function by BTF id (`src = 2`).
    Kfunc(i32),
}

/// One typed instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Insn {
    Alu {
        wide: bool,
        op: AluOp,
        dst: Reg,
        src: Operand,
    },
    Endian {
        kind: Endian,
        bits: u8,
        dst: Reg,
    },
    /// Unconditional jump; `off` in slots relative to the next slot.
    Ja {
        off: i32,
    },
    Jcond {
        wide: bool,
        op: JmpOp,
        dst: Reg,
        src: Operand,
        off: i16,
    },
    Call(CallTarget),
    Exit,
    LdImm64 {
        dst: Reg,
        imm: Imm64,
    },
    Ldx {
        size: Size,
        dst: Reg,
        src: Reg,
        off: i16,
        /// `BPF_MEMSX` sign-extending load (ISA v4).
        sign_extend: bool,
    },
    St {
        size: Size,
        dst: Reg,
        off: i16,
        imm: i32,
    },
    Stx {
        size: Size,
        dst: Reg,
        src: Reg,
        off: i16,
    },
    Atomic {
        size: Size,
        op: AtomicOp,
        dst: Reg,
        src: Reg,
        off: i16,
    },
    /// Legacy `BPF_LD | BPF_ABS` / `BPF_IND` packet loads.
    LdLegacy {
        size: Size,
        indirect: bool,
        src: Reg,
        imm: i32,
    },
}

impl Insn {
    /// Number of slots this instruction occupies.
    pub fn slots(&self) -> usize {
        match self {
            Insn::LdImm64 { .. } => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    TruncatedProgram {
        len: usize,
    },
    BadRegister {
        pc: usize,
        reg: u8,
    },
    InvalidOpcode {
        pc: usize,
        code: u8,
    },
    ReservedFieldSet {
        pc: usize,
        field: &'static str,
    },
    /// `LD_IMM64` without its second slot, or a malformed second slot.
    BrokenLdImm64 {
        pc: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TruncatedProgram { len } => {
                write!(f, "program length {len} is not a multiple of 8")
            }
            DecodeError::BadRegister { pc, reg } => write!(f, "insn {pc}: invalid register {reg}"),
            DecodeError::InvalidOpcode { pc, code } => {
                write!(f, "insn {pc}: invalid opcode {code:#04x}")
            }
            DecodeError::ReservedFieldSet { pc, field } => {
                write!(f, "insn {pc}: reserved field {field} must be zero")
            }
            DecodeError::BrokenLdImm64 { pc } => write!(f, "insn {pc}: incomplete ld_imm64"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn reg(pc: usize, n: u8) -> Result<Reg, DecodeError> {
    Reg::new(n).ok_or(DecodeError::BadRegister { pc, reg: n })
}

fn zero<T: PartialEq + Default>(pc: usize, v: T, field: &'static str) -> Result<(), DecodeError> {
    if v == T::default() {
        Ok(())
    } else {
        Err(DecodeError::ReservedFieldSet { pc, field })
    }
}

/// Decode the instruction starting at slot `pc`.
pub fn decode(slots: &[RawInsn], pc: usize) -> Result<Insn, DecodeError> {
    let Some(&r) = slots.get(pc) else {
        return Err(DecodeError::TruncatedProgram {
            len: slots.len() * 8,
        });
    };
    let bad = DecodeError::InvalidOpcode { pc, code: r.code };
    match r.class() {
        class::ALU | class::ALU64 => {
            let wide = r.class() == class::ALU64;
            let x = r.code & 0x08 != 0;
            let dst = reg(pc, r.dst)?;
            let opc = r.code & 0xf0;
            if opc == 0xd0 {
                let kind = match (wide, x) {
                    (false, false) => Endian::ToLe,
                    (false, true) => Endian::ToBe,
                    (true, false) => Endian::Swap,
                    (true, true) => return Err(bad),
                };
                zero(pc, r.src, "src")?;
                zero(pc, r.off, "off")?;
                let bits = match r.imm {
                    16 => 16,
                    32 => 32,
                    64 => 64,
                    _ => return Err(bad),
                };
                return Ok(Insn::Endian { kind, bits, dst });
            }
            let op = match (opc, r.off) {
                (0x00, 0) => AluOp::Add,
                (0x10, 0) => AluOp::Sub,
                (0x20, 0) => AluOp::Mul,
                (0x30, 0) => AluOp::Div,
                (0x30, 1) => AluOp::SDiv,
                (0x40, 0) => AluOp::Or,
                (0x50, 0) => AluOp::And,
                (0x60, 0) => AluOp::Lsh,
                (0x70, 0) => AluOp::Rsh,
                (0x80, 0) => AluOp::Neg,
                (0x90, 0) => AluOp::Mod,
                (0x90, 1) => AluOp::SMod,
                (0xa0, 0) => AluOp::Xor,
                (0xb0, 0) => AluOp::Mov,
                (0xb0, 8) => AluOp::MovSx(8),
                (0xb0, 16) => AluOp::MovSx(16),
                (0xb0, 32) if wide => AluOp::MovSx(32),
                (0xc0, 0) => AluOp::Arsh,
                (0x00..=0xc0, _) => return Err(DecodeError::ReservedFieldSet { pc, field: "off" }),
                _ => return Err(bad),
            };
            let src = if op == AluOp::Neg {
                if x {
                    return Err(bad);
                }
                zero(pc, r.src, "src")?;
                zero(pc, r.imm, "imm")?;
                Operand::Imm(0)
            } else if x {
                zero(pc, r.imm, "imm")?;
                Operand::Reg(reg(pc, r.src)?)
            } else {
                zero(pc, r.src, "src")?;
                Operand::Imm(r.imm)
            };
            if matches!(op, AluOp::MovSx(_)) && !x {
                return Err(bad);
            }
            Ok(Insn::Alu { wide, op, dst, src })
        }
        class::JMP | class::JMP32 => {
            let wide = r.class() == class::JMP;
            let x = r.code & 0x08 != 0;
            match r.code & 0xf0 {
                0x00 => {
                    if x {
                        return Err(bad);
                    }
                    zero(pc, r.dst, "dst")?;
                    zero(pc, r.src, "src")?;
                    if wide {
                        zero(pc, r.imm, "imm")?;
                        Ok(Insn::Ja { off: r.off as i32 })
                    } else {
                        // gotol: 32-bit offset in imm.
                        zero(pc, r.off, "off")?;
                        Ok(Insn::Ja { off: r.imm })
                    }
                }
                0x80 if wide => {
                    if x {
                        return Err(bad);
                    }
                    zero(pc, r.dst, "dst")?;
                    zero(pc, r.off, "off")?;
                    let t = match r.src {
                        0 => CallTarget::Helper(r.imm),
                        pseudo::CALL => CallTarget::Local(r.imm),
                        pseudo::KFUNC_CALL => CallTarget::Kfunc(r.imm),
                        _ => return Err(bad),
                    };
                    Ok(Insn::Call(t))
                }
                0x90 if wide => {
                    if x {
                        return Err(bad);
                    }
                    zero(pc, r.dst, "dst")?;
                    zero(pc, r.src, "src")?;
                    zero(pc, r.off, "off")?;
                    zero(pc, r.imm, "imm")?;
                    Ok(Insn::Exit)
                }
                opc => {
                    let op = JmpOp::from_code(opc).ok_or(bad)?;
                    let dst = reg(pc, r.dst)?;
                    let src = if x {
                        zero(pc, r.imm, "imm")?;
                        Operand::Reg(reg(pc, r.src)?)
                    } else {
                        zero(pc, r.src, "src")?;
                        Operand::Imm(r.imm)
                    };
                    Ok(Insn::Jcond {
                        wide,
                        op,
                        dst,
                        src,
                        off: r.off,
                    })
                }
            }
        }
        class::LD => {
            let size = Size::from_code(r.code);
            match r.code & 0xe0 {
                0x00 if size == Size::DW => {
                    let dst = reg(pc, r.dst)?;
                    zero(pc, r.off, "off")?;
                    let Some(&hi) = slots.get(pc + 1) else {
                        return Err(DecodeError::BrokenLdImm64 { pc });
                    };
                    if hi.code != 0 || hi.dst != 0 || hi.src != 0 || hi.off != 0 {
                        return Err(DecodeError::BrokenLdImm64 { pc });
                    }
                    let imm = match r.src {
                        0 => Imm64::Value((r.imm as u32 as u64) | ((hi.imm as u32 as u64) << 32)),
                        pseudo::MAP_FD => {
                            if hi.imm != 0 {
                                return Err(DecodeError::BrokenLdImm64 { pc });
                            }
                            Imm64::MapFd(r.imm)
                        }
                        pseudo::MAP_VALUE
                        | pseudo::BTF_ID
                        | pseudo::FUNC
                        | pseudo::MAP_IDX
                        | pseudo::MAP_IDX_VALUE => Imm64::Pseudo {
                            src: r.src,
                            imm_lo: r.imm,
                            imm_hi: hi.imm,
                        },
                        _ => return Err(bad),
                    };
                    Ok(Insn::LdImm64 { dst, imm })
                }
                m @ (0x20 | 0x40) if size != Size::DW => {
                    zero(pc, r.dst, "dst")?;
                    zero(pc, r.off, "off")?;
                    let indirect = m == 0x40;
                    let src = if indirect {
                        reg(pc, r.src)?
                    } else {
                        zero(pc, r.src, "src")?;
                        Reg::R0
                    };
                    Ok(Insn::LdLegacy {
                        size,
                        indirect,
                        src,
                        imm: r.imm,
                    })
                }
                _ => Err(bad),
            }
        }
        class::LDX => {
            let sign_extend = match r.code & 0xe0 {
                0x60 => false,
                0x80 if Size::from_code(r.code) != Size::DW => true,
                _ => return Err(bad),
            };
            zero(pc, r.imm, "imm")?;
            Ok(Insn::Ldx {
                size: Size::from_code(r.code),
                dst: reg(pc, r.dst)?,
                src: reg(pc, r.src)?,
                off: r.off,
                sign_extend,
            })
        }
        class::ST => {
            if r.code & 0xe0 != 0x60 {
                return Err(bad);
            }
            zero(pc, r.src, "src")?;
            Ok(Insn::St {
                size: Size::from_code(r.code),
                dst: reg(pc, r.dst)?,
                off: r.off,
                imm: r.imm,
            })
        }
        class::STX => {
            let size = Size::from_code(r.code);
            match r.code & 0xe0 {
                0x60 => {
                    zero(pc, r.imm, "imm")?;
                    Ok(Insn::Stx {
                        size,
                        dst: reg(pc, r.dst)?,
                        src: reg(pc, r.src)?,
                        off: r.off,
                    })
                }
                0xc0 if matches!(size, Size::W | Size::DW) => {
                    let op = AtomicOp::from_imm(r.imm).ok_or(bad)?;
                    Ok(Insn::Atomic {
                        size,
                        op,
                        dst: reg(pc, r.dst)?,
                        src: reg(pc, r.src)?,
                        off: r.off,
                    })
                }
                _ => Err(bad),
            }
        }
        _ => Err(bad),
    }
}

/// Decode a whole program. Entry `i` is `Some` when an instruction starts at
/// slot `i` and `None` for the second slot of an `LD_IMM64`.
pub fn decode_all(slots: &[RawInsn]) -> Result<Vec<Option<Insn>>, DecodeError> {
    let mut out = Vec::with_capacity(slots.len());
    let mut pc = 0;
    while pc < slots.len() {
        let insn = decode(slots, pc)?;
        let n = insn.slots();
        out.push(Some(insn));
        if n == 2 {
            out.push(None);
        }
        pc += n;
    }
    Ok(out)
}

/// Encode one typed instruction into one or two slots.
pub fn encode(insn: &Insn) -> Vec<RawInsn> {
    let r = |code, dst: Reg, src: u8, off, imm| RawInsn::new(code, dst.num(), src, off, imm);
    match *insn {
        Insn::Alu { wide, op, dst, src } => {
            let cls = if wide { class::ALU64 } else { class::ALU };
            let (opc, off) = op.code();
            match src {
                Operand::Imm(i) => vec![r(cls | opc, dst, 0, off, i)],
                Operand::Reg(s) => vec![r(cls | opc | 0x08, dst, s.num(), off, 0)],
            }
        }
        Insn::Endian { kind, bits, dst } => {
            let code = match kind {
                Endian::ToLe => class::ALU | 0xd0,
                Endian::ToBe => class::ALU | 0xd8,
                Endian::Swap => class::ALU64 | 0xd0,
            };
            vec![r(code, dst, 0, 0, bits as i32)]
        }
        Insn::Ja { off } => match i16::try_from(off) {
            Ok(o) => vec![RawInsn::new(class::JMP, 0, 0, o, 0)],
            Err(_) => vec![RawInsn::new(class::JMP32, 0, 0, 0, off)],
        },
        Insn::Jcond {
            wide,
            op,
            dst,
            src,
            off,
        } => {
            let cls = if wide { class::JMP } else { class::JMP32 };
            match src {
                Operand::Imm(i) => vec![r(cls | op.code(), dst, 0, off, i)],
                Operand::Reg(s) => vec![r(cls | op.code() | 0x08, dst, s.num(), off, 0)],
            }
        }
        Insn::Call(t) => {
            let (src, imm) = match t {
                CallTarget::Helper(i) => (0, i),
                CallTarget::Local(i) => (pseudo::CALL, i),
                CallTarget::Kfunc(i) => (pseudo::KFUNC_CALL, i),
            };
            vec![RawInsn::new(class::JMP | 0x80, 0, src, 0, imm)]
        }
        Insn::Exit => vec![RawInsn::new(class::JMP | 0x90, 0, 0, 0, 0)],
        Insn::LdImm64 { dst, imm } => {
            let (src, lo, hi) = match imm {
                Imm64::Value(v) => (0, v as u32 as i32, (v >> 32) as u32 as i32),
                Imm64::MapFd(fd) => (pseudo::MAP_FD, fd, 0),
                Imm64::Pseudo {
                    src,
                    imm_lo,
                    imm_hi,
                } => (src, imm_lo, imm_hi),
            };
            vec![
                r(class::LD | 0x18, dst, src, 0, lo),
                RawInsn::new(0, 0, 0, 0, hi),
            ]
        }
        Insn::Ldx {
            size,
            dst,
            src,
            off,
            sign_extend,
        } => {
            let mode = if sign_extend { 0x80 } else { 0x60 };
            vec![r(class::LDX | mode | size.code(), dst, src.num(), off, 0)]
        }
        Insn::St {
            size,
            dst,
            off,
            imm,
        } => vec![r(class::ST | 0x60 | size.code(), dst, 0, off, imm)],
        Insn::Stx {
            size,
            dst,
            src,
            off,
        } => vec![r(class::STX | 0x60 | size.code(), dst, src.num(), off, 0)],
        Insn::Atomic {
            size,
            op,
            dst,
            src,
            off,
        } => vec![r(
            class::STX | 0xc0 | size.code(),
            dst,
            src.num(),
            off,
            op.imm(),
        )],
        Insn::LdLegacy {
            size,
            indirect,
            src,
            imm,
        } => {
            let mode = if indirect { 0x40 } else { 0x20 };
            let s = if indirect { src.num() } else { 0 };
            vec![RawInsn::new(class::LD | mode | size.code(), 0, s, 0, imm)]
        }
    }
}

/// One line per instruction (`slot: typed form`), for diagnostics.
pub fn disassemble(slots: &[RawInsn]) -> String {
    let mut out = String::new();
    let mut pc = 0;
    while pc < slots.len() {
        match decode(slots, pc) {
            Ok(i) => {
                out.push_str(&format!("{pc:4}: {i:?}\n"));
                pc += i.slots();
            }
            Err(e) => {
                out.push_str(&format!("{pc:4}: <{e}>\n"));
                pc += 1;
            }
        }
    }
    out
}
