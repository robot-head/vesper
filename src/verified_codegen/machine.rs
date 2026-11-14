//! The slot IR (`Lir`), the abstract target instruction subset (`M`) and the
//! instruction-selection table between them.
//!
//! [`select`] is a total function from one `Lir` row to a fixed sequence of
//! `M` instructions. Every row kind has a Lean counterpart in
//! `lean/VerifiedCodegen.lean` (`select`), and [`table_listing`] renders the
//! table in the exact text form the Lean file prints, so the tests can check
//! that the two tables are the same.
use std::fmt::{self, Write as _};

/// A 64-bit word in the current frame.
pub type Slot = u32;
/// A code label (function entries, blocks and abort exits).
pub type Label = u32;

/// Abort exits: every trap jumps to one of these program-wide labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrapKind {
    /// `Fail[ArithOverflow]`.
    Overflow,
    /// `Fail[RuntimeError]` with `RuntimeError::DivisionByZero`.
    DivisionByZero,
    /// A block the checker marked unreachable was reached.
    Unreachable,
}
impl TrapKind {
    pub const ALL: [TrapKind; 3] = [Self::Overflow, Self::DivisionByZero, Self::Unreachable];
    /// The label of the shared abort exit.
    pub fn label(self) -> Label {
        self as Label
    }
    /// The process exit status of this abort.
    pub fn exit_status(self) -> u8 {
        match self {
            Self::Overflow => super::EXIT_OVERFLOW,
            Self::DivisionByZero => super::EXIT_DIVISION_BY_ZERO,
            Self::Unreachable => super::EXIT_UNREACHABLE,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Overflow => "ovf",
            Self::DivisionByZero => "div0",
            Self::Unreachable => "unreachable",
        }
    }
}
/// Labels below this value are the abort exits.
pub const FIRST_FREE_LABEL: Label = 3;

/// The machine representation of a scalar: a zero-extended (unsigned, `bool`)
/// or two's-complement (`i64`) 64-bit word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    U8,
    U32,
    U64,
    I64,
    Bool,
}
impl Kind {
    pub const INTEGERS: [Kind; 4] = [Self::U8, Self::U32, Self::U64, Self::I64];
    pub fn signed(self) -> bool {
        self == Self::I64
    }
    /// The largest value of a narrow unsigned kind, whose results need an
    /// explicit range check (wider kinds use the hardware flags).
    pub fn narrow_mask(self) -> Option<u64> {
        match self {
            Self::U8 => Some(0xff),
            Self::U32 => Some(0xffff_ffff),
            _ => None,
        }
    }
    fn sign(self) -> Sign {
        if self.signed() { Sign::S } else { Sign::U }
    }
    fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::Bool => "bool",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}
impl ArithOp {
    pub const ALL: [ArithOp; 5] = [Self::Add, Self::Sub, Self::Mul, Self::Div, Self::Rem];
    fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::Rem => "rem",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WrapOp {
    Add,
    Sub,
    Mul,
}
impl WrapOp {
    pub const ALL: [WrapOp; 3] = [Self::Add, Self::Sub, Self::Mul];
    fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cond {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
impl Cond {
    pub const ALL: [Cond; 6] = [Self::Eq, Self::Ne, Self::Lt, Self::Le, Self::Gt, Self::Ge];
    fn name(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
        }
    }
}
/// Signedness of a machine operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sign {
    S,
    U,
}
impl Sign {
    fn name(self) -> &'static str {
        match self {
            Self::S => "s",
            Self::U => "u",
        }
    }
}

/// The slot IR: checked Place IR after flattening every value of the scalar
/// subset into 64-bit words. One row is one `Lir` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lir {
    Const {
        dst: Slot,
        word: u64,
    },
    Move {
        dst: Slot,
        src: Slot,
    },
    /// Checked `+ - * / %` (`vesper.arith.checked/1`).
    Arith {
        op: ArithOp,
        kind: Kind,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `wrapping_add`, `wrapping_sub`, `wrapping_mul`.
    Wrapping {
        op: WrapOp,
        kind: Kind,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// Checked value-level negation.
    Neg {
        kind: Kind,
        dst: Slot,
        a: Slot,
    },
    /// Boolean `!`.
    Not {
        dst: Slot,
        a: Slot,
    },
    Compare {
        cond: Cond,
        kind: Kind,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// Outgoing call area word `index` := slot.
    StoreOut {
        index: u32,
        src: Slot,
    },
    LoadOut {
        dst: Slot,
        index: u32,
    },
    /// Incoming area (parameters, results, `inout` write-back).
    LoadIn {
        dst: Slot,
        index: u32,
    },
    StoreIn {
        index: u32,
        src: Slot,
    },
    Call {
        callee: Label,
    },
    Label(Label),
    Jump(Label),
    /// Branch to `target` when the `bool` in `cond` is false.
    BranchZero {
        cond: Slot,
        target: Label,
    },
    /// Function prologue with a frame of `words` 64-bit words.
    Enter {
        words: u32,
    },
    Return,
    Trap(TrapKind),
}

/// Scratch registers of the abstract machine. Values never live in
/// registers across rows: every row loads its operands from frame words and
/// stores its result, which is what makes the table row-local.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reg {
    R0,
    R1,
    R2,
}
impl Reg {
    fn name(self) -> &'static str {
        match self {
            Self::R0 => "r0",
            Self::R1 => "r1",
            Self::R2 => "r2",
        }
    }
}
/// A frame word: a local slot, the outgoing call area, or the incoming area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Loc {
    Slot(Slot),
    Out(u32),
    In(u32),
}
impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Slot(n) => write!(f, "s{n}"),
            Self::Out(n) => write!(f, "o{n}"),
            Self::In(n) => write!(f, "i{n}"),
        }
    }
}

/// The abstract target instruction subset. Each instruction has one fixed
/// x86-64 and one fixed AArch64 encoding (see `x86_64.rs`, `aarch64.rs`),
/// and a semantics in `lean/VerifiedCodegen.lean` (`Mach.step`). Arithmetic
/// works on `R0 := R0 op R1`; `R2` is a scratch register some instructions
/// clobber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M {
    Ld(Reg, Loc),
    St(Loc, Reg),
    Imm(Reg, u64),
    /// `R0 := R0 + R1`, abort `Overflow` on signed overflow (`S`) or carry (`U`).
    AddChk(Sign),
    /// `R0 := R0 - R1`, abort on signed overflow or borrow.
    SubChk(Sign),
    /// `R0 := R0 * R1` (low word), abort when the full product does not fit.
    /// Clobbers `R2`.
    MulChk(Sign),
    /// `R0 := (R0 op R1) mod 2^64`.
    Wrap(WrapOp),
    /// `R0 := R0 & R2`.
    AndR2,
    /// Abort `Overflow` when `R0 > R2` (unsigned).
    TrapGtU,
    /// Abort `DivisionByZero` when `R1 = 0`.
    TrapZero,
    /// Abort `Overflow` when `R0 = i64::MIN` and `R1 = -1`. Clobbers `R2`.
    TrapDivOvf,
    /// `R0 := R0 / R1`, truncating. Clobbers `R2`.
    Div(Sign),
    /// `R0 := R0 rem R1` (sign of the dividend; `x rem -1 = 0`). Clobbers `R2`.
    Rem(Sign),
    /// `R0 := -R0`; aborts `Overflow` for `i64::MIN` (`S`) or any non-zero
    /// unsigned value (`U`).
    NegChk(Sign),
    /// `R0 := R0 xor 1`.
    XorOne,
    /// `R0 := (R0 cond R1) ? 1 : 0`.
    Cmp(Cond, Sign),
    Label(Label),
    Jmp(Label),
    /// Branch when `R0 = 0`.
    Bz(Label),
    Call(Label),
    Enter(u32),
    Leave,
    Trap(TrapKind),
}
impl fmt::Display for M {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ld(r, l) => write!(f, "ld {} {l}", r.name()),
            Self::St(l, r) => write!(f, "st {l} {}", r.name()),
            Self::Imm(r, w) => write!(f, "li {} {w}", r.name()),
            Self::AddChk(s) => write!(f, "addchk {}", s.name()),
            Self::SubChk(s) => write!(f, "subchk {}", s.name()),
            Self::MulChk(s) => write!(f, "mulchk {}", s.name()),
            Self::Wrap(op) => write!(f, "wrap {}", op.name()),
            Self::AndR2 => write!(f, "andr2"),
            Self::TrapGtU => write!(f, "trapgtu"),
            Self::TrapZero => write!(f, "trapzero"),
            Self::TrapDivOvf => write!(f, "trapdivovf"),
            Self::Div(s) => write!(f, "div {}", s.name()),
            Self::Rem(s) => write!(f, "rem {}", s.name()),
            Self::NegChk(s) => write!(f, "negchk {}", s.name()),
            Self::XorOne => write!(f, "xorone"),
            Self::Cmp(c, s) => write!(f, "cmp {} {}", c.name(), s.name()),
            Self::Label(l) => write!(f, "label L{l}"),
            Self::Jmp(l) => write!(f, "jmp L{l}"),
            Self::Bz(l) => write!(f, "bz L{l}"),
            Self::Call(l) => write!(f, "call L{l}"),
            Self::Enter(n) => write!(f, "enter {n}"),
            Self::Leave => write!(f, "leave"),
            Self::Trap(k) => write!(f, "trap {}", k.name()),
        }
    }
}

use M::*;
use Reg::*;

fn binary(a: Slot, b: Slot, body: &[M], dst: Slot) -> Vec<M> {
    let mut out = vec![Ld(R0, Loc::Slot(a)), Ld(R1, Loc::Slot(b))];
    out.extend_from_slice(body);
    out.push(St(Loc::Slot(dst), R0));
    out
}

/// The instruction-selection table: one fixed `M` sequence per `Lir` row.
/// Total: every row of the slot IR has exactly one entry.
pub fn select(row: &Lir) -> Vec<M> {
    match *row {
        Lir::Const { dst, word } => vec![Imm(R0, word), St(Loc::Slot(dst), R0)],
        Lir::Move { dst, src } => vec![Ld(R0, Loc::Slot(src)), St(Loc::Slot(dst), R0)],
        Lir::Arith {
            op,
            kind,
            dst,
            a,
            b,
        } => {
            let s = kind.sign();
            let range = |m: M| match kind.narrow_mask() {
                Some(mask) => vec![m, Imm(R2, mask), TrapGtU],
                None => vec![m],
            };
            let body = match op {
                ArithOp::Add => range(AddChk(s)),
                // A narrow unsigned difference is never above its minuend.
                ArithOp::Sub => vec![SubChk(s)],
                ArithOp::Mul => range(MulChk(s)),
                ArithOp::Div if kind.signed() => vec![TrapZero, TrapDivOvf, Div(s)],
                ArithOp::Div => vec![TrapZero, Div(s)],
                ArithOp::Rem => vec![TrapZero, Rem(s)],
            };
            binary(a, b, &body, dst)
        }
        Lir::Wrapping {
            op,
            kind,
            dst,
            a,
            b,
        } => {
            let mut body = vec![Wrap(op)];
            if let Some(mask) = kind.narrow_mask() {
                body.extend([Imm(R2, mask), AndR2]);
            }
            binary(a, b, &body, dst)
        }
        Lir::Neg { kind, dst, a } => vec![
            Ld(R0, Loc::Slot(a)),
            NegChk(kind.sign()),
            St(Loc::Slot(dst), R0),
        ],
        Lir::Not { dst, a } => vec![Ld(R0, Loc::Slot(a)), XorOne, St(Loc::Slot(dst), R0)],
        Lir::Compare {
            cond,
            kind,
            dst,
            a,
            b,
        } => binary(a, b, &[Cmp(cond, kind.sign())], dst),
        Lir::StoreOut { index, src } => vec![Ld(R0, Loc::Slot(src)), St(Loc::Out(index), R0)],
        Lir::LoadOut { dst, index } => vec![Ld(R0, Loc::Out(index)), St(Loc::Slot(dst), R0)],
        Lir::LoadIn { dst, index } => vec![Ld(R0, Loc::In(index)), St(Loc::Slot(dst), R0)],
        Lir::StoreIn { index, src } => vec![Ld(R0, Loc::Slot(src)), St(Loc::In(index), R0)],
        Lir::Call { callee } => vec![Call(callee)],
        Lir::Label(l) => vec![Label(l)],
        Lir::Jump(l) => vec![Jmp(l)],
        Lir::BranchZero { cond, target } => vec![Ld(R0, Loc::Slot(cond)), Bz(target)],
        Lir::Enter { words } => vec![Enter(words)],
        Lir::Return => vec![Leave],
        Lir::Trap(k) => vec![Trap(k)],
    }
}

/// Every row kind of the table, instantiated at fixed slots (`dst = 0`,
/// `a = 1`, `b = 2`), in a canonical order.
pub fn table_rows() -> Vec<(String, Lir)> {
    let (dst, a, b) = (0, 1, 2);
    let mut rows = vec![
        ("const".to_owned(), Lir::Const { dst, word: 7 }),
        ("move".to_owned(), Lir::Move { dst, src: a }),
    ];
    for op in ArithOp::ALL {
        for kind in Kind::INTEGERS {
            rows.push((
                format!("arith {} {}", op.name(), kind.name()),
                Lir::Arith {
                    op,
                    kind,
                    dst,
                    a,
                    b,
                },
            ));
        }
    }
    for op in WrapOp::ALL {
        for kind in Kind::INTEGERS {
            rows.push((
                format!("wrapping {} {}", op.name(), kind.name()),
                Lir::Wrapping {
                    op,
                    kind,
                    dst,
                    a,
                    b,
                },
            ));
        }
    }
    for kind in Kind::INTEGERS {
        rows.push((format!("neg {}", kind.name()), Lir::Neg { kind, dst, a }));
    }
    rows.push(("not".to_owned(), Lir::Not { dst, a }));
    for cond in Cond::ALL {
        for kind in [Kind::U8, Kind::U32, Kind::U64, Kind::I64, Kind::Bool] {
            rows.push((
                format!("compare {} {}", cond.name(), kind.name()),
                Lir::Compare {
                    cond,
                    kind,
                    dst,
                    a,
                    b,
                },
            ));
        }
    }
    rows.extend([
        ("storeout".to_owned(), Lir::StoreOut { index: 3, src: a }),
        ("loadout".to_owned(), Lir::LoadOut { dst, index: 3 }),
        ("loadin".to_owned(), Lir::LoadIn { dst, index: 3 }),
        ("storein".to_owned(), Lir::StoreIn { index: 3, src: a }),
        (
            "branchzero".to_owned(),
            Lir::BranchZero { cond: a, target: 9 },
        ),
        ("jump".to_owned(), Lir::Jump(9)),
        ("label".to_owned(), Lir::Label(9)),
        ("call".to_owned(), Lir::Call { callee: 9 }),
        ("enter".to_owned(), Lir::Enter { words: 4 }),
        ("return".to_owned(), Lir::Return),
        ("trap ovf".to_owned(), Lir::Trap(TrapKind::Overflow)),
        ("trap div0".to_owned(), Lir::Trap(TrapKind::DivisionByZero)),
        (
            "trap unreachable".to_owned(),
            Lir::Trap(TrapKind::Unreachable),
        ),
    ]);
    rows
}

/// The table in the line format `lean/VerifiedCodegen.lean` prints:
/// `<row>: <m>; <m>; ...`, one row per line.
pub fn table_listing() -> String {
    let mut out = String::new();
    for (name, row) in table_rows() {
        let ms = select(&row)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        let _ = writeln!(out, "{name}: {ms}");
    }
    out
}
