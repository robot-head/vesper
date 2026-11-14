//! Linear host-side builders for acquired kernel references.
//!
//! Handles borrow the builder, are neither `Clone` nor `Copy`, and have
//! consuming terminal calls. Dropping a handle records a leak; even
//! `mem::forget` leaves an outstanding obligation. No release is emitted by
//! `Drop`. [`BpfBuilder::finish`] refuses either case, then runs the existing
//! bytecode verifier on *all* paths (including paths introduced by [`Insn`]).
//! This is Rust library discipline, not Vesper source-language linear typing.
//!
//! ```
//! use vesper_ebpf::{BpfBuilder, MapDef, ProgramType, Reg, RingbufWakeup};
//! use vesper_ebpf::insn::{AluOp, Insn, Size};
//! let mut b = BpfBuilder::new();
//! let ring = b.map(MapDef::ringbuf("events", 4096))?;
//! let mut slot = b.reserve_ringbuf(ring, 8, Reg::R6)?;
//! slot.store(Size::DW, 0, 42)?;
//! slot.submit(RingbufWakeup::Adaptive);
//! b.emit(Insn::Alu { wide: true, op: AluOp::Mov, dst: Reg::R0, src: 2.into() });
//! b.emit(Insn::Exit);
//! let program = b.finish("events", ProgramType::Xdp)?;
//! # Ok::<(), vesper_ebpf::BuildError>(())
//! ```

use crate::helpers as h;
use crate::insn::{Insn, JmpOp, Operand, Reg, Size};
use crate::program::{Asm, AsmError, Label, MapDef, MapKind, Program, ProgramType};
use crate::{VerifierPolicy, VerifyError, verify};
use std::fmt;

/// Host-side obligations, including forgotten handles. Counts refer to
/// emitted branch arms, not the number of runtime acquisitions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinearAudit {
    pub outstanding: usize,
    pub dropped: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildError {
    Asm(AsmError),
    InvalidArgument(&'static str),
    Unreleased(LinearAudit),
    Verify(VerifyError),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asm(e) => e.fmt(f),
            Self::InvalidArgument(e) => f.write_str(e),
            Self::Unreleased(a) => write!(
                f,
                "{} unreleased builder handles ({} dropped)",
                a.outstanding, a.dropped
            ),
            Self::Verify(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<AsmError> for BuildError {
    fn from(e: AsmError) -> Self {
        Self::Asm(e)
    }
}

/// Kernel ringbuf notification flags. Invalid combinations are unrepresentable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(i32)]
pub enum RingbufWakeup {
    #[default]
    Adaptive = 0,
    NoWakeup = 1,
    ForceWakeup = 2,
}

/// `bpf_sock_tuple` bytes in network byte order, without native padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketTuple {
    Ipv4([u8; 12]),
    Ipv6([u8; 36]),
}

impl SocketTuple {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Ipv4(b) => b,
            Self::Ipv6(b) => b,
        }
    }
}

/// An assembler whose only program-producing operation verifies the result.
/// Raw instructions can be mixed with typed handles; `finish` checks their
/// control flow and reference use too. The underlying assembler is not exposed.
#[derive(Debug, Default)]
pub struct BpfBuilder {
    asm: Asm,
    audit: LinearAudit,
}

impl BpfBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt an existing prefix. It is checked together with the completed
    /// program at `finish`, including any manually acquired references.
    pub fn from_asm(asm: Asm) -> Self {
        Self {
            asm,
            audit: LinearAudit::default(),
        }
    }

    pub fn audit(&self) -> LinearAudit {
        self.audit
    }

    pub fn map(&mut self, def: MapDef) -> Result<usize, BuildError> {
        Ok(self.asm.map(def)?)
    }

    pub fn label(&mut self) -> Label {
        self.asm.label()
    }

    pub fn bind(&mut self, label: Label) -> Result<(), BuildError> {
        Ok(self.asm.bind(label)?)
    }

    pub fn emit(&mut self, insn: Insn) {
        self.asm.insn(insn);
    }

    /// Emit reserve + a null guard. `ptr` is a caller-owned scratch register
    /// (r0..r9), overwritten on success. Callee-saved r6..r9 survive helpers.
    /// A failed kernel reservation skips to the instruction after the handle's
    /// terminal call or its complete `branch`; there is no reference to release.
    pub fn reserve_ringbuf(
        &mut self,
        map: usize,
        bytes: u32,
        ptr: Reg,
    ) -> Result<BpfRingSlot<'_>, BuildError> {
        writable(ptr)?;
        // Check before emitting anything; an argument error leaves the builder usable.
        let def = self.asm_map(map)?;
        if def.kind != MapKind::RingBuf
            || bytes == 0
            || bytes > def.max_entries / 2
            || bytes > i32::MAX as u32
        {
            return Err(BuildError::InvalidArgument(
                "invalid ringbuf map or record size",
            ));
        }
        self.asm.ld_map(Reg::R1, map);
        self.asm.mov64(Reg::R2, bytes as i32);
        self.asm.mov64(Reg::R3, 0);
        self.asm.call(h::RINGBUF_RESERVE);
        let handle = self.acquire(ptr);
        Ok(BpfRingSlot { handle, bytes })
    }

    /// Look up TCP in the current network namespace. `ctx` must still hold
    /// the XDP context. The caller provides a 4-byte-aligned scratch stack
    /// range at `tuple_offset`, which is overwritten with all tuple bytes.
    /// `ptr` and null handling follow `reserve_ringbuf`.
    pub fn sk_lookup_tcp(
        &mut self,
        ctx: Reg,
        tuple: SocketTuple,
        tuple_offset: i16,
        ptr: Reg,
    ) -> Result<BpfSocketRef<'_>, BuildError> {
        writable(ptr)?;
        let bytes = tuple.bytes();
        let end = i32::from(tuple_offset) + bytes.len() as i32;
        if tuple_offset < -512 || tuple_offset % 4 != 0 || end > 0 {
            return Err(BuildError::InvalidArgument(
                "socket tuple must fit an aligned scratch stack range",
            ));
        }
        for (i, word) in bytes.as_chunks::<4>().0.iter().enumerate() {
            let imm = i32::from_le_bytes(*word);
            self.asm
                .st(Size::W, Reg::R10, tuple_offset + (i * 4) as i16, imm);
        }
        self.asm.mov64(Reg::R1, ctx);
        self.asm.mov64(Reg::R2, Reg::R10);
        self.asm.add64(Reg::R2, i32::from(tuple_offset));
        self.asm.mov64(Reg::R3, bytes.len() as i32);
        self.asm.ld_imm64(Reg::R4, h::CURRENT_NETNS);
        self.asm.mov64(Reg::R5, 0);
        self.asm.call(h::SK_LOOKUP_TCP);
        Ok(BpfSocketRef {
            handle: self.acquire(ptr),
        })
    }

    fn asm_map(&self, map: usize) -> Result<&MapDef, BuildError> {
        self.asm
            .map_def(map)
            .ok_or(BuildError::InvalidArgument("unknown ringbuf map"))
    }

    fn acquire(&mut self, ptr: Reg) -> Handle<'_> {
        let end = self.asm.label();
        self.asm.jmp(JmpOp::Eq, Reg::R0, 0, end);
        if ptr != Reg::R0 {
            self.asm.mov64(ptr, Reg::R0);
        }
        Handle::new(self, ptr, Some(end))
    }

    /// Refuse leaked host handles, resolve the assembly and run the existing
    /// verifier, including `ref.leak`. No unchecked `Program` can be extracted.
    pub fn finish(self, name: &str, prog_type: ProgramType) -> Result<Program, BuildError> {
        if self.audit.outstanding != 0 {
            return Err(BuildError::Unreleased(self.audit));
        }
        let program = self.asm.finish(name, prog_type)?;
        verify(&program, &VerifierPolicy::default()).map_err(BuildError::Verify)?;
        Ok(program)
    }
}

fn writable(ptr: Reg) -> Result<(), BuildError> {
    if ptr == Reg::R10 {
        Err(BuildError::InvalidArgument(
            "r10 cannot hold an acquired reference",
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct Handle<'a> {
    builder: &'a mut BpfBuilder,
    ptr: Reg,
    null_end: Option<Label>,
    active: bool,
}

impl<'a> Handle<'a> {
    fn new(builder: &'a mut BpfBuilder, ptr: Reg, null_end: Option<Label>) -> Self {
        builder.audit.outstanding += 1;
        Self {
            builder,
            ptr,
            null_end,
            active: true,
        }
    }

    fn complete(mut self, helper: i32, flags: RingbufWakeup) {
        self.builder.asm.mov64(Reg::R1, self.ptr);
        if helper != h::SK_RELEASE {
            self.builder.asm.mov64(Reg::R2, flags as i32);
        }
        self.builder.asm.call(helper);
        self.active = false;
        self.builder.audit.outstanding -= 1;
        if let Some(end) = self.null_end.take() {
            self.builder.bind_private(end);
        }
    }

    fn branch(
        mut self,
        op: JmpOp,
        dst: Reg,
        src: Operand,
        on_true: impl for<'b> FnOnce(Handle<'b>),
        on_false: impl for<'b> FnOnce(Handle<'b>),
    ) {
        let yes = self.builder.asm.label();
        let join = self.builder.asm.label();
        self.builder.asm.jmp(op, dst, src, yes);
        // One obligation becomes one for each emitted runtime branch arm.
        self.active = false;
        self.builder.audit.outstanding -= 1;
        on_false(Handle::new(self.builder, self.ptr, None));
        self.builder.asm.ja(join);
        self.builder.bind_private(yes);
        on_true(Handle::new(self.builder, self.ptr, None));
        self.builder.bind_private(join);
        if let Some(end) = self.null_end.take() {
            self.builder.bind_private(end);
        }
    }
}

impl BpfBuilder {
    fn bind_private(&mut self, label: Label) {
        self.asm
            .bind(label)
            .expect("builder-private label is bound exactly once");
    }
}

impl Drop for Handle<'_> {
    fn drop(&mut self) {
        if self.active {
            self.builder.audit.dropped += 1;
        }
    }
}

/// A non-null kernel ring reservation. Consume with `submit`, `discard`, or
/// `branch` whose *both* arms consume their handle. Dropping poisons finalization.
///
/// ```compile_fail,E0382
/// use vesper_ebpf::{BpfRingSlot, RingbufWakeup};
/// fn twice(slot: BpfRingSlot<'_>) {
///     slot.submit(RingbufWakeup::Adaptive);
///     slot.discard(RingbufWakeup::Adaptive); // use after move
/// }
/// ```
#[derive(Debug)]
#[must_use = "a ring reservation must be submitted or discarded"]
pub struct BpfRingSlot<'a> {
    handle: Handle<'a>,
    bytes: u32,
}

impl BpfRingSlot<'_> {
    /// Write a scalar field within the record. An error leaves ownership with
    /// the caller, who can correct the write or discard the reservation.
    pub fn store(
        &mut self,
        size: Size,
        offset: i16,
        value: impl Into<Operand>,
    ) -> Result<(), BuildError> {
        if offset < 0
            || offset as u32 + size.bytes() > self.bytes
            || !(offset as u32).is_multiple_of(size.bytes())
        {
            return Err(BuildError::InvalidArgument(
                "ringbuf field is outside the record or misaligned",
            ));
        }
        match value.into() {
            Operand::Imm(imm) => {
                self.handle
                    .builder
                    .asm
                    .st(size, self.handle.ptr, offset, imm);
            }
            Operand::Reg(src) => {
                self.handle
                    .builder
                    .asm
                    .stx(size, self.handle.ptr, offset, src);
            }
        }
        Ok(())
    }

    /// Emit instructions on the success path. Clobbering the reference or
    /// jumping around its terminal call is rejected at `finish`.
    pub fn emit(&mut self, insn: Insn) {
        self.handle.builder.emit(insn);
    }

    pub fn submit(self, flags: RingbufWakeup) {
        self.handle.complete(h::RINGBUF_SUBMIT, flags);
    }
    pub fn discard(self, flags: RingbufWakeup) {
        self.handle.complete(h::RINGBUF_DISCARD, flags);
    }

    pub fn branch(
        self,
        op: JmpOp,
        dst: Reg,
        src: impl Into<Operand>,
        on_true: impl for<'b> FnOnce(BpfRingSlot<'b>),
        on_false: impl for<'b> FnOnce(BpfRingSlot<'b>),
    ) {
        let bytes = self.bytes;
        self.handle.branch(
            op,
            dst,
            src.into(),
            |handle| on_true(BpfRingSlot { handle, bytes }),
            |handle| on_false(BpfRingSlot { handle, bytes }),
        );
    }
}

/// A non-null `sk_lookup_tcp` reference. Consume with `release`, or release
/// in both arms of `branch`. Socket field reads remain unsupported by the verifier.
///
/// ```compile_fail,E0382
/// use vesper_ebpf::BpfSocketRef;
/// fn twice(socket: BpfSocketRef<'_>) {
///     socket.release();
///     socket.release(); // use after move
/// }
/// ```
#[derive(Debug)]
#[must_use = "a socket lookup reference must be released"]
pub struct BpfSocketRef<'a> {
    handle: Handle<'a>,
}

impl BpfSocketRef<'_> {
    pub fn emit(&mut self, insn: Insn) {
        self.handle.builder.emit(insn);
    }
    pub fn release(self) {
        self.handle.complete(h::SK_RELEASE, RingbufWakeup::Adaptive);
    }

    pub fn branch(
        self,
        op: JmpOp,
        dst: Reg,
        src: impl Into<Operand>,
        on_true: impl for<'b> FnOnce(BpfSocketRef<'b>),
        on_false: impl for<'b> FnOnce(BpfSocketRef<'b>),
    ) {
        self.handle.branch(
            op,
            dst,
            src.into(),
            |handle| on_true(BpfSocketRef { handle }),
            |handle| on_false(BpfSocketRef { handle }),
        );
    }
}
