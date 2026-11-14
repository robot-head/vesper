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
    /// `BPF_MAP_TYPE_PROG_ARRAY`: the target table of `bpf_tail_call`.
    ProgArray,
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
            MapKind::ProgArray => 3,
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
            3 => MapKind::ProgArray,
            _ => return None,
        })
    }

    pub fn is_array(self) -> bool {
        matches!(
            self,
            MapKind::Array | MapKind::PerCpuArray | MapKind::ProgArray
        )
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

    /// A program array of `slots` tail-call targets (4-byte keys and
    /// values, the values being program fds written by the loader).
    pub fn prog_array(name: &str, slots: u32) -> Self {
        Self::new(name, MapKind::ProgArray, 4, 4, slots)
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
                if k == MapKind::ProgArray && self.value_size != 4 {
                    return Err("program arrays have 4-byte values".into());
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
    /// `BPF_PROG_TYPE_XDP`: context `struct xdp_md`.
    Xdp,
    /// `BPF_PROG_TYPE_SCHED_CLS` (tc classifier): context `struct __sk_buff`.
    SchedCls,
    /// `BPF_PROG_TYPE_TRACEPOINT`: context is the read-only tracepoint
    /// record (at most `PERF_MAX_TRACE_SIZE` bytes).
    Tracepoint,
    /// `BPF_PROG_TYPE_KPROBE`: context is the read-only `struct pt_regs`
    /// of the target architecture.
    Kprobe,
}

impl ProgramType {
    pub const ALL: [ProgramType; 4] = [
        ProgramType::Xdp,
        ProgramType::SchedCls,
        ProgramType::Tracepoint,
        ProgramType::Kprobe,
    ];

    pub fn kernel_type(self) -> u32 {
        match self {
            ProgramType::Xdp => 6,
            ProgramType::SchedCls => 3,
            ProgramType::Tracepoint => 5,
            ProgramType::Kprobe => 2,
        }
    }

    pub fn from_kernel_type(t: u32) -> Option<ProgramType> {
        ProgramType::ALL.into_iter().find(|p| p.kernel_type() == t)
    }

    /// Whether the program reads and writes packet bytes directly
    /// (`data`/`data_end` in its context).
    pub fn has_packet(self) -> bool {
        matches!(self, ProgramType::Xdp | ProgramType::SchedCls)
    }

    /// Whether `BPF_PROG_TEST_RUN` can execute the program type.
    pub fn test_runnable(self) -> bool {
        self.has_packet()
    }

    /// Tracing programs (kprobe, tracepoint) run in process context with
    /// the tracing helper set.
    pub fn is_tracing(self) -> bool {
        matches!(self, ProgramType::Tracepoint | ProgramType::Kprobe)
    }
}

/// A kernel function (kfunc) call site: `CALL` with `src = 2` whose
/// immediate is the BTF id of `name` in the target kernel's BTF, written
/// by the loader ([`crate::kernel::resolve_kfuncs`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KfuncRef {
    pub slot: usize,
    pub name: String,
}

/// One `.BTF.ext` line-info record: source position of the instruction at
/// `slot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineInfo {
    pub slot: usize,
    pub file: String,
    /// The source line's text (the kernel prints it in verifier logs).
    pub text: String,
    pub line: u32,
    pub col: u32,
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
    /// Attach target appended to the ELF section name, e.g.
    /// `syscalls/sys_enter_openat` for `tracepoint/syscalls/sys_enter_openat`
    /// or `do_unlinkat` for `kprobe/do_unlinkat`. Empty: no target.
    pub attach: String,
    /// kfunc call sites, resolved to BTF ids at load time.
    pub kfuncs: Vec<KfuncRef>,
    /// BTF CO-RE relocations (`.BTF.ext` core_relo records).
    pub core_relos: Vec<crate::core::CoreRelo>,
    /// Source line info (`.BTF.ext` line_info records), sorted by slot.
    pub lines: Vec<LineInfo>,
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
    JumpTooFar {
        at: usize,
    },
    UnknownMap(String),
    DuplicateMap(String),
    BadMap(String),
    /// The object BTF could not be built (for `TYPE_ID_*` local ids).
    Btf(String),
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
            AsmError::Btf(m) => write!(f, "object BTF: {m}"),
        }
    }
}

impl std::error::Error for AsmError {}

#[derive(Clone, Copy, Debug)]
enum Fixup {
    Jump(Label),
    Call(Label),
    /// `LD_IMM64` with `BPF_PSEUDO_FUNC` (a callback address).
    Func(Label),
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
    kfuncs: Vec<KfuncRef>,
    core_relos: Vec<crate::core::CoreRelo>,
    lines: Vec<LineInfo>,
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

    /// `dst = &callback` (`LD_IMM64` with `BPF_PSEUDO_FUNC`), the callback
    /// argument of `bpf_loop`; the callback also becomes a subprogram.
    pub fn ld_func(&mut self, dst: Reg, callback: Label) -> &mut Self {
        let at = self.slots.len();
        self.fixups.push((at, Fixup::Func(callback)));
        self.insn(Insn::LdImm64 {
            dst,
            imm: Imm64::Pseudo {
                src: crate::insn::pseudo::FUNC,
                imm_lo: 0,
                imm_hi: 0,
            },
        })
    }

    /// Call kernel function `name` (a kfunc); the BTF id is written at
    /// load time.
    pub fn call_kfunc(&mut self, name: &str) -> &mut Self {
        self.kfuncs.push(KfuncRef {
            slot: self.slots.len(),
            name: name.to_owned(),
        });
        self.insn(Insn::Call(CallTarget::Kfunc(0)))
    }

    /// `dst = <CO-RE value>`: the local value of `spec`, relocated for the
    /// target kernel by the loader (or by [`crate::core::apply`]).
    pub fn core(
        &mut self,
        dst: Reg,
        spec: crate::core::CoreSpec,
    ) -> Result<&mut Self, crate::core::CoreError> {
        // a TYPE_ID_* immediate is the root's id in the object BTF, known
        // once the program is complete: `finish` fills it in
        let v = if spec.kind.needs_local_id() {
            0
        } else {
            spec.local_value()?
        };
        let slot = self.slots.len();
        match i32::try_from(v) {
            Ok(i) => self.mov64(dst, i),
            Err(_) => self.ld_imm64(dst, v),
        };
        self.core_relos.push(crate::core::CoreRelo { slot, spec });
        Ok(self)
    }

    /// `dst = *(size *)(src + <CO-RE field offset>)`: a load whose offset
    /// is the byte offset of a field (`spec` of kind
    /// [`FieldByteOffset`](crate::core::CoreKind::FieldByteOffset)),
    /// relocated for the target kernel.
    pub fn core_ldx(
        &mut self,
        size: Size,
        dst: Reg,
        src: Reg,
        spec: crate::core::CoreSpec,
    ) -> Result<&mut Self, crate::core::CoreError> {
        let slot = self.slots.len();
        let v = spec.local_value()?;
        let off = i16::try_from(v).map_err(|_| crate::core::CoreError::BadInstruction {
            slot,
            detail: format!("local offset {v} does not fit 16 bits"),
        })?;
        self.ldx(size, dst, src, off);
        self.core_relos.push(crate::core::CoreRelo { slot, spec });
        Ok(self)
    }

    /// Record a source position for the next instruction (`.BTF.ext`
    /// line info).
    pub fn line(&mut self, file: &str, line: u32, col: u32, text: &str) -> &mut Self {
        let slot = self.slots.len();
        self.lines.retain(|l| l.slot != slot);
        self.lines.push(LineInfo {
            slot,
            file: file.to_owned(),
            text: text.to_owned(),
            line,
            col,
        });
        self
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
                Fixup::Call(l) | Fixup::Func(l) => (l, true),
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
        let mut prog = Program {
            name: name.to_owned(),
            prog_type,
            insns: self.slots,
            maps: self.maps,
            map_relocs: self.map_relocs,
            functions,
            license: "Dual MIT/GPL".to_owned(),
            attach: String::new(),
            kfuncs: self.kfuncs,
            core_relos: self.core_relos,
            lines: {
                let mut l = self.lines;
                l.sort_by_key(|l| l.slot);
                l
            },
        };
        crate::btf_ext::assign_local_ids(&mut prog).map_err(|e| AsmError::Btf(e.0))?;
        Ok(prog)
    }
}
