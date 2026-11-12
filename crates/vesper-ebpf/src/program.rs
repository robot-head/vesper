//! Programs, map definitions and the assembler used by tests and builders.

use crate::insn::{
    AluOp, AtomicOp, CallTarget, Endian, Imm64, Insn, JmpOp, Operand, RawInsn, Reg, Size, encode,
};
use std::fmt;

/// Kernel map types this crate supports (`enum bpf_map_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MapKind {
    Hash,
    Array,
    PerCpuHash,
    PerCpuArray,
    LruHash,
    RingBuf,
}

impl MapKind {
    pub fn kernel_type(self) -> u32 {
        match self {
            MapKind::Hash => 1,
            MapKind::Array => 2,
            MapKind::PerCpuHash => 5,
            MapKind::PerCpuArray => 6,
            MapKind::LruHash => 9,
            MapKind::RingBuf => 27,
        }
    }

    pub fn from_kernel_type(t: u32) -> Option<MapKind> {
        Some(match t {
            1 => MapKind::Hash,
            2 => MapKind::Array,
            5 => MapKind::PerCpuHash,
            6 => MapKind::PerCpuArray,
            9 => MapKind::LruHash,
            27 => MapKind::RingBuf,
            _ => return None,
        })
    }

    pub fn is_array(self) -> bool {
        matches!(self, MapKind::Array | MapKind::PerCpuArray)
    }
}

/// A map the program references. In a [`Program`], `LD_IMM64` with
/// `BPF_PSEUDO_MAP_FD` carries the index of its map in [`Program::maps`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapDef {
    pub name: String,
    pub kind: MapKind,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
    pub flags: u32,
}

impl MapDef {
    pub fn new(
        name: &str,
        kind: MapKind,
        key_size: u32,
        value_size: u32,
        max_entries: u32,
    ) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            key_size,
            value_size,
            max_entries,
            flags: 0,
        }
    }

    pub fn ringbuf(name: &str, bytes: u32) -> Self {
        Self::new(name, MapKind::RingBuf, 0, 0, bytes)
    }

    /// Kernel `map_alloc_check` rules for the supported kinds.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 15 || !self.name.is_ascii() {
            return Err(format!(
                "map name {:?} must be 1..=15 ASCII bytes",
                self.name
            ));
        }
        match self.kind {
            MapKind::RingBuf => {
                if self.key_size != 0 || self.value_size != 0 {
                    return Err("ringbuf key and value size must be 0".into());
                }
                let n = self.max_entries;
                if n < 4096 || !n.is_power_of_two() {
                    return Err("ringbuf size must be a power of two >= 4096".into());
                }
            }
            k => {
                if self.max_entries == 0 || self.value_size == 0 {
                    return Err("max_entries and value_size must be nonzero".into());
                }
                if k.is_array() && self.key_size != 4 {
                    return Err("array maps have 4-byte keys".into());
                }
                if self.key_size == 0 || self.key_size > 512 {
                    return Err("key size must be 1..=512".into());
                }
                if self.value_size > (1 << 20) {
                    return Err("value size too large".into());
                }
            }
        }
        Ok(())
    }
}

/// Program types this crate verifies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProgramType {
    Xdp,
}

impl ProgramType {
    pub fn kernel_type(self) -> u32 {
        match self {
            ProgramType::Xdp => 6,
        }
    }
}

/// An assembled program with its maps, before map-fd relocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub name: String,
    pub prog_type: ProgramType,
    pub insns: Vec<RawInsn>,
    pub maps: Vec<MapDef>,
    /// Slot indices of `LD_IMM64 BPF_PSEUDO_MAP_FD` instructions, with the
    /// map index each one loads.
    pub map_relocs: Vec<(usize, usize)>,
    /// Entry slots of BPF-to-BPF subprograms (named for ELF symbols).
    pub functions: Vec<(String, usize)>,
    pub license: String,
}

impl Program {
    pub fn bytes(&self) -> Vec<u8> {
        crate::insn::slots_to_bytes(&self.insns)
    }
}

/// A forward-referenceable jump or call target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(usize);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsmError {
    UnboundLabel(usize),
    LabelBoundTwice(usize),
    JumpTooFar { at: usize },
    UnknownMap(String),
    DuplicateMap(String),
    BadMap(String),
}

impl fmt::Display for AsmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsmError::UnboundLabel(l) => write!(f, "label {l} never bound"),
            AsmError::LabelBoundTwice(l) => write!(f, "label {l} bound twice"),
            AsmError::JumpTooFar { at } => write!(f, "jump at slot {at} out of 16-bit range"),
            AsmError::UnknownMap(m) => write!(f, "unknown map {m}"),
            AsmError::DuplicateMap(m) => write!(f, "duplicate map {m}"),
            AsmError::BadMap(m) => write!(f, "invalid map: {m}"),
        }
    }
}

impl std::error::Error for AsmError {}

#[derive(Clone, Copy, Debug)]
enum Fixup {
    Jump(Label),
    Call(Label),
}

/// Two-pass assembler with labels, map references and local calls.
#[derive(Clone, Debug, Default)]
pub struct Asm {
    slots: Vec<RawInsn>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Fixup)>,
    maps: Vec<MapDef>,
    map_relocs: Vec<(usize, usize)>,
    functions: Vec<(String, Label)>,
}

impl Asm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current slot index.
    pub fn pc(&self) -> usize {
        self.slots.len()
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    pub fn bind(&mut self, l: Label) -> Result<(), AsmError> {
        if self.labels[l.0].is_some() {
            return Err(AsmError::LabelBoundTwice(l.0));
        }
        self.labels[l.0] = Some(self.slots.len());
        Ok(())
    }

    /// Bind a fresh label here.
    pub fn here(&mut self) -> Label {
        let l = self.label();
        self.labels[l.0] = Some(self.slots.len());
        l
    }

    /// Declare a map; returns its index.
    pub fn map(&mut self, def: MapDef) -> Result<usize, AsmError> {
        def.validate().map_err(AsmError::BadMap)?;
        if self.maps.iter().any(|m| m.name == def.name) {
            return Err(AsmError::DuplicateMap(def.name));
        }
        self.maps.push(def);
        Ok(self.maps.len() - 1)
    }

    pub fn map_index(&self, name: &str) -> Result<usize, AsmError> {
        self.maps
            .iter()
            .position(|m| m.name == name)
            .ok_or_else(|| AsmError::UnknownMap(name.to_owned()))
    }

    pub(crate) fn map_def(&self, index: usize) -> Option<&MapDef> {
        self.maps.get(index)
    }

    /// Emit an arbitrary typed instruction (jump offsets taken literally).
    pub fn insn(&mut self, i: Insn) -> &mut Self {
        self.slots.extend(encode(&i));
        self
    }

    /// Emit a raw slot (used by tests to build malformed programs).
    pub fn raw(&mut self, r: RawInsn) -> &mut Self {
        self.slots.push(r);
        self
    }

    pub fn alu64(&mut self, op: AluOp, dst: Reg, src: impl Into<Operand>) -> &mut Self {
        self.insn(Insn::Alu {
            wide: true,
            op,
            dst,
            src: src.into(),
        })
    }

    pub fn alu32(&mut self, op: AluOp, dst: Reg, src: impl Into<Operand>) -> &mut Self {
        self.insn(Insn::Alu {
            wide: false,
            op,
            dst,
            src: src.into(),
        })
    }

    pub fn mov64(&mut self, dst: Reg, src: impl Into<Operand>) -> &mut Self {
        self.alu64(AluOp::Mov, dst, src)
    }

    pub fn add64(&mut self, dst: Reg, src: impl Into<Operand>) -> &mut Self {
        self.alu64(AluOp::Add, dst, src)
    }

    pub fn endian(&mut self, kind: Endian, bits: u8, dst: Reg) -> &mut Self {
        self.insn(Insn::Endian { kind, bits, dst })
    }

    pub fn ld_imm64(&mut self, dst: Reg, v: u64) -> &mut Self {
        self.insn(Insn::LdImm64 {
            dst,
            imm: Imm64::Value(v),
        })
    }

    /// `dst = &map` (relocated to a map fd at load time).
    pub fn ld_map(&mut self, dst: Reg, map: usize) -> &mut Self {
        let at = self.slots.len();
        self.map_relocs.push((at, map));
        self.insn(Insn::LdImm64 {
            dst,
            imm: Imm64::MapFd(map as i32),
        })
    }

    pub fn ldx(&mut self, size: Size, dst: Reg, src: Reg, off: i16) -> &mut Self {
        self.insn(Insn::Ldx {
            size,
            dst,
            src,
            off,
            sign_extend: false,
        })
    }

    pub fn stx(&mut self, size: Size, dst: Reg, off: i16, src: Reg) -> &mut Self {
        self.insn(Insn::Stx {
            size,
            dst,
            src,
            off,
        })
    }

    pub fn st(&mut self, size: Size, dst: Reg, off: i16, imm: i32) -> &mut Self {
        self.insn(Insn::St {
            size,
            dst,
            off,
            imm,
        })
    }

    pub fn atomic(&mut self, size: Size, op: AtomicOp, dst: Reg, off: i16, src: Reg) -> &mut Self {
        self.insn(Insn::Atomic {
            size,
            op,
            dst,
            src,
            off,
        })
    }

    pub fn call(&mut self, helper: i32) -> &mut Self {
        self.insn(Insn::Call(CallTarget::Helper(helper)))
    }

    /// BPF-to-BPF call to `target`, which also becomes a named function.
    pub fn call_local(&mut self, target: Label) -> &mut Self {
        let at = self.slots.len();
        self.fixups.push((at, Fixup::Call(target)));
        self.insn(Insn::Call(CallTarget::Local(0)))
    }

    /// Name a subprogram entry for ELF symbols.
    pub fn function(&mut self, name: &str, entry: Label) {
        self.functions.push((name.to_owned(), entry));
    }

    pub fn exit(&mut self) -> &mut Self {
        self.insn(Insn::Exit)
    }

    /// Unconditional jump with a literal slot offset (tests build
    /// malformed control flow with it).
    pub fn ja_raw(&mut self, off: i32) -> &mut Self {
        self.insn(Insn::Ja { off })
    }

    pub fn ja(&mut self, target: Label) -> &mut Self {
        let at = self.slots.len();
        self.fixups.push((at, Fixup::Jump(target)));
        self.insn(Insn::Ja { off: 0 })
    }

    pub fn jmp(
        &mut self,
        op: JmpOp,
        dst: Reg,
        src: impl Into<Operand>,
        target: Label,
    ) -> &mut Self {
        let at = self.slots.len();
        self.fixups.push((at, Fixup::Jump(target)));
        self.insn(Insn::Jcond {
            wide: true,
            op,
            dst,
            src: src.into(),
            off: 0,
        })
    }

    pub fn jmp32(
        &mut self,
        op: JmpOp,
        dst: Reg,
        src: impl Into<Operand>,
        target: Label,
    ) -> &mut Self {
        let at = self.slots.len();
        self.fixups.push((at, Fixup::Jump(target)));
        self.insn(Insn::Jcond {
            wide: false,
            op,
            dst,
            src: src.into(),
            off: 0,
        })
    }

    /// Resolve labels and produce a program.
    pub fn finish(mut self, name: &str, prog_type: ProgramType) -> Result<Program, AsmError> {
        let resolve =
            |labels: &[Option<usize>], l: Label| labels[l.0].ok_or(AsmError::UnboundLabel(l.0));
        for &(at, fix) in &self.fixups {
            let (l, is_call) = match fix {
                Fixup::Jump(l) => (l, false),
                Fixup::Call(l) => (l, true),
            };
            let target = resolve(&self.labels, l)? as i64;
            let rel = target - (at as i64 + 1);
            let slot = &mut self.slots[at];
            if is_call {
                slot.imm = i32::try_from(rel).map_err(|_| AsmError::JumpTooFar { at })?;
            } else if slot.code == crate::insn::class::JMP32 {
                slot.imm = i32::try_from(rel).map_err(|_| AsmError::JumpTooFar { at })?;
            } else {
                slot.off = i16::try_from(rel).map_err(|_| AsmError::JumpTooFar { at })?;
            }
        }
        let mut functions = Vec::new();
        for (n, l) in &self.functions {
            functions.push((n.clone(), resolve(&self.labels, *l)?));
        }
        Ok(Program {
            name: name.to_owned(),
            prog_type,
            insns: self.slots,
            maps: self.maps,
            map_relocs: self.map_relocs,
            functions,
            license: "Dual MIT/GPL".to_owned(),
        })
    }
}
