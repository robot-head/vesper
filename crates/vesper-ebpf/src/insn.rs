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
    /// `BPF_CLASS(code)`.
    pub const MASK: u8 = 0x07;
}

/// Operation codes (`BPF_OP`, high four bits) of the ALU classes, and of
/// the `BPF_ATOMIC` `imm` field.
pub mod op {
    pub const ADD: u8 = 0x00;
    pub const SUB: u8 = 0x10;
    pub const MUL: u8 = 0x20;
    pub const DIV: u8 = 0x30;
    pub const OR: u8 = 0x40;
    pub const AND: u8 = 0x50;
    pub const LSH: u8 = 0x60;
    pub const RSH: u8 = 0x70;
    pub const NEG: u8 = 0x80;
    pub const MOD: u8 = 0x90;
    pub const XOR: u8 = 0xa0;
    pub const MOV: u8 = 0xb0;
    pub const ARSH: u8 = 0xc0;
    pub const END: u8 = 0xd0;
    /// `BPF_OP(code)`.
    pub const MASK: u8 = 0xf0;
}

/// Operation codes (`BPF_OP`) of the jump classes.
pub mod jmp {
    pub const JA: u8 = 0x00;
    pub const JEQ: u8 = 0x10;
    pub const JGT: u8 = 0x20;
    pub const JGE: u8 = 0x30;
    pub const JSET: u8 = 0x40;
    pub const JNE: u8 = 0x50;
    pub const JSGT: u8 = 0x60;
    pub const JSGE: u8 = 0x70;
    pub const CALL: u8 = 0x80;
    pub const EXIT: u8 = 0x90;
    pub const JLT: u8 = 0xa0;
    pub const JLE: u8 = 0xb0;
    pub const JSLT: u8 = 0xc0;
    pub const JSLE: u8 = 0xd0;
}

/// The source bit (`BPF_SRC`): an immediate (`BPF_K`) or a register
/// (`BPF_X`). On `BPF_END` the same bit selects `BPF_TO_BE`.
pub mod source {
    pub const K: u8 = 0x00;
    pub const X: u8 = 0x08;
    pub const TO_LE: u8 = 0x00;
    pub const TO_BE: u8 = 0x08;
}

/// Access sizes (`BPF_SIZE`) of the load and store classes.
pub mod size {
    pub const W: u8 = 0x00;
    pub const H: u8 = 0x08;
    pub const B: u8 = 0x10;
    pub const DW: u8 = 0x18;
    /// `BPF_SIZE(code)`.
    pub const MASK: u8 = 0x18;
}

/// Modes (`BPF_MODE`) of the load and store classes.
pub mod mode {
    pub const IMM: u8 = 0x00;
    pub const ABS: u8 = 0x20;
    pub const IND: u8 = 0x40;
    pub const MEM: u8 = 0x60;
    pub const MEMSX: u8 = 0x80;
    pub const ATOMIC: u8 = 0xc0;
    /// `BPF_MODE(code)`.
    pub const MASK: u8 = 0xe0;
}

/// `BPF_ATOMIC` `imm` values: an ALU [`op`] code, optionally with
/// `BPF_FETCH`, or one of the exchanges.
pub mod atomic {
    pub const FETCH: i32 = 0x01;
    pub const XCHG: i32 = 0xe0 | FETCH;
    pub const CMPXCHG: i32 = 0xf0 | FETCH;
}

/// Width of a register field (`dst` is the low nibble of byte 1, `src` the
/// high one).
const REG_NIBBLE: u8 = 0x0f;

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
        b[1] = (self.dst & REG_NIBBLE) | ((self.src & REG_NIBBLE) << 4);
        b[2..4].copy_from_slice(&self.off.to_le_bytes());
        b[4..8].copy_from_slice(&self.imm.to_le_bytes());
        b
    }

    pub fn from_bytes(b: [u8; 8]) -> Self {
        Self {
            code: b[0],
            dst: b[1] & REG_NIBBLE,
            src: b[1] >> 4,
            off: i16::from_le_bytes([b[2], b[3]]),
            imm: i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        }
    }

    pub fn class(self) -> u8 {
        self.code & class::MASK
    }
}

/// Serialise slots to bytes.
pub fn slots_to_bytes(slots: &[RawInsn]) -> Vec<u8> {
    crate::port::ffi::insn_slots_to_bytes(slots)
}

/// Parse bytes into slots. The length must be a multiple of eight.
pub fn slots_from_bytes(bytes: &[u8]) -> Result<Vec<RawInsn>, DecodeError> {
    crate::port::slots_from_bytes(bytes)
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



/// Decode the instruction starting at slot `pc`.
pub fn decode(slots: &[RawInsn], pc: usize) -> Result<Insn, DecodeError> {
    crate::port::decode(slots, pc)
}

/// Decode a whole program. Entry `i` is `Some` when an instruction starts at
/// slot `i` and `None` for the second slot of an `LD_IMM64`.
pub fn decode_all(slots: &[RawInsn]) -> Result<Vec<Option<Insn>>, DecodeError> {
    crate::port::decode_all(slots)
}

/// Encode one typed instruction into one or two slots.
pub fn encode(insn: &Insn) -> Vec<RawInsn> {
    crate::port::encode(insn)
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
