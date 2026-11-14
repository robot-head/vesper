//! Register unwinding of parked fibers through DWARF CFI (§12, §31).
//!
//! The saved switch frame ([`RegisterFrame`]) gives the callee-saved
//! registers, stack pointer and resume address of the parking function. The
//! frames *below* it, the functions that called the parking function, are
//! recovered here by executing their DWARF call-frame information with
//! `gimli`: each step computes the frame's canonical frame address (CFA)
//! and the caller's callee-saved registers and return address from the
//! stack bytes.
//!
//! * CFI comes from a [`CfiTable`]: `.eh_frame` bytes built from Cranelift's
//!   per-function unwind information at JIT time (the System V kind on every
//!   host, Windows included, since Cranelift emits it for any calling
//!   convention of the target architecture), with each function's address
//!   range and name.
//! * Memory comes from a [`StackImage`] (a live parked fiber's or one decoded
//!   from a [`crate::FiberImage`]); reading outside it stops the walk with
//!   a typed reason instead of touching memory.
//! * Frame 0 is the parking function itself:
//!   [`crate::fiber::vesper_fiber_park`] has no frame (it tail-jumps into the
//!   switch), so the switch frame's return address is the park call site.
//!
//! The walk ends at the first return address that no CFI covers (normally
//! the host's `fiber_main`, which calls the generated entry), at a zero
//! return address, at a frame limit, or when the stack does not grow toward
//! the top. Host Rust frames are not unwound: their CFI lives in the host
//! binary's own sections (and on macOS mostly in compact unwind), which this
//! table does not load. Only registers the target ABI makes callee-saved are
//! reported; caller-saved registers have no recoverable value in a caller.
//! Vector callee-saved registers (AArch64 d8–d15, Windows xmm6–xmm15) are
//! reported for frame 0 from the switch frame but not unwound further.
use crate::image::{FrameAbi, RegisterFrame, StackImage};
use crate::switch::SWITCH_FRAME_BYTES;
use cranelift_codegen::gimli;
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::isa::unwind::UnwindInfo;
use gimli::{BaseAddresses, CfaRule, EhFrame, LittleEndian, Register, RegisterRule, UnwindSection};
use std::fmt;

/// One function with CFI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CfiFunction {
    pub name: String,
    pub start: u64,
    pub len: u64,
}

/// `.eh_frame` for a set of generated functions. See the module docs.
#[derive(Clone, Default)]
pub struct CfiTable {
    eh_frame: Vec<u8>,
    functions: Vec<CfiFunction>,
}

impl fmt::Debug for CfiTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CfiTable")
            .field("eh_frame_bytes", &self.eh_frame.len())
            .field("functions", &self.functions)
            .finish()
    }
}

/// Why a [`CfiTable`] could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CfiError {
    /// The ISA has no System V CIE (not x86_64 or AArch64).
    NoCie,
    /// A function's unwind information is not of the System V kind.
    NotSystemV(String),
    /// Overlapping or empty function ranges.
    BadRange(String),
    Write(String),
}

impl fmt::Display for CfiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cfi table: {self:?}")
    }
}
impl std::error::Error for CfiError {}

impl CfiTable {
    /// Build `.eh_frame` for functions at their final addresses.
    /// `functions` are `(name, start, len, unwind)`, the unwind information
    /// from `CompiledCode::create_unwind_info_of_kind(isa, SystemV)`.
    pub fn build(
        isa: &dyn TargetIsa,
        functions: &[(String, u64, u64, UnwindInfo)],
    ) -> Result<Self, CfiError> {
        let cie = isa.create_systemv_cie().ok_or(CfiError::NoCie)?;
        let mut table = gimli::write::FrameTable::default();
        let cie = table.add_cie(cie);
        let mut ranges = vec![];
        for (name, start, len, info) in functions {
            let UnwindInfo::SystemV(info) = info else {
                return Err(CfiError::NotSystemV(name.clone()));
            };
            if *len == 0 || start.checked_add(*len).is_none() {
                return Err(CfiError::BadRange(name.clone()));
            }
            table.add_fde(cie, info.to_fde(gimli::write::Address::Constant(*start)));
            ranges.push(CfiFunction {
                name: name.clone(),
                start: *start,
                len: *len,
            });
        }
        ranges.sort_by_key(|f| f.start);
        for w in ranges.windows(2) {
            if w[0].start + w[0].len > w[1].start {
                return Err(CfiError::BadRange(w[1].name.clone()));
            }
        }
        let mut eh = gimli::write::EhFrame(gimli::write::EndianVec::new(LittleEndian));
        table
            .write_eh_frame(&mut eh)
            .map_err(|e| CfiError::Write(e.to_string()))?;
        // A zero terminator, as a linker would append.
        let mut eh_frame = eh.0.into_vec();
        eh_frame.extend_from_slice(&[0; 4]);
        Ok(Self {
            eh_frame,
            functions: ranges,
        })
    }
    /// The raw `.eh_frame` bytes (absolute FDE addresses).
    pub fn eh_frame(&self) -> &[u8] {
        &self.eh_frame
    }
    pub fn functions(&self) -> &[CfiFunction] {
        &self.functions
    }
    /// The function whose code contains `pc`.
    pub fn function_at(&self, pc: u64) -> Option<&CfiFunction> {
        let i = self.functions.partition_point(|f| f.start <= pc);
        let f = self.functions.get(i.checked_sub(1)?)?;
        (pc < f.start + f.len).then_some(f)
    }
}

/// One recovered frame, innermost first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnwoundFrame {
    /// The return address this frame resumes at (frame 0: the park call
    /// site).
    pub pc: u64,
    /// The stack pointer while this frame runs that call (the callee's
    /// CFA).
    pub sp: u64,
    /// This frame's own CFA, the stack pointer of its caller.
    pub cfa: u64,
    pub function: String,
    /// Callee-saved general-purpose registers as this frame sees them, in
    /// ABI order (rbp/x29 included). A register is listed only if its value
    /// is known.
    pub registers: Vec<(&'static str, u64)>,
}

impl UnwoundFrame {
    pub fn get(&self, name: &str) -> Option<u64> {
        self.registers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
    }
}

/// Why a walk ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The next return address is not covered by the table (for example the
    /// host function that called the generated entry).
    NoCfi { pc: u64 },
    /// A zero return address (the base of a fiber stack).
    End,
    /// `max_frames` frames were recovered.
    FrameLimit,
}

/// A walk that could not proceed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnwindError {
    /// The register frame does not belong to this host's ABI.
    WrongAbi(FrameAbi),
    /// The stack image is shorter than a switch frame.
    Truncated,
    /// A CFI rule needed memory outside the stack image.
    Unreadable { frame: usize, address: u64 },
    /// A CFI rule this unwinder does not evaluate (DWARF expressions,
    /// architectural rules), or a CFA register with no known value.
    Unsupported { frame: usize, detail: String },
    /// The CFA did not move toward the stack top, or left the stack.
    BadCfa { frame: usize, cfa: u64 },
    /// Malformed CFI.
    Cfi { frame: usize, detail: String },
}

impl fmt::Display for UnwindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unwind: {self:?}")
    }
}
impl std::error::Error for UnwindError {}

/// A recovered call chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backtrace {
    pub frames: Vec<UnwoundFrame>,
    pub stop: Stop,
    /// Frame 0's callee-saved vector registers, from the switch frame.
    pub vector: Vec<(&'static str, u128)>,
}

impl Backtrace {
    /// Function names, innermost first.
    pub fn functions(&self) -> Vec<&str> {
        self.frames.iter().map(|f| f.function.as_str()).collect()
    }
}

/// DWARF numbers and names of the host ABI's callee-saved general registers
/// and of the stack pointer and return-address column.
struct Arch {
    callee_saved: &'static [(u16, &'static str)],
    sp: u16,
    ra: u16,
}

#[cfg(all(target_arch = "x86_64", not(windows)))]
const ARCH: Arch = Arch {
    callee_saved: &[
        (3, "rbx"),
        (6, "rbp"),
        (12, "r12"),
        (13, "r13"),
        (14, "r14"),
        (15, "r15"),
    ],
    sp: 7,
    ra: 16,
};
#[cfg(all(target_arch = "x86_64", windows))]
const ARCH: Arch = Arch {
    callee_saved: &[
        (3, "rbx"),
        (6, "rbp"),
        (4, "rsi"),
        (5, "rdi"),
        (12, "r12"),
        (13, "r13"),
        (14, "r14"),
        (15, "r15"),
    ],
    sp: 7,
    ra: 16,
};
#[cfg(target_arch = "aarch64")]
const ARCH: Arch = Arch {
    callee_saved: &[
        (19, "x19"),
        (20, "x20"),
        (21, "x21"),
        (22, "x22"),
        (23, "x23"),
        (24, "x24"),
        (25, "x25"),
        (26, "x26"),
        (27, "x27"),
        (28, "x28"),
        (29, "x29"),
    ],
    sp: 31,
    ra: 30,
};

/// Callee-saved state of one frame: DWARF register number to value.
type Regs = Vec<(u16, u64)>;

fn get(regs: &Regs, r: u16) -> Option<u64> {
    regs.iter().find(|(n, _)| *n == r).map(|(_, v)| *v)
}

/// Walk the frames of a parked fiber below its switch frame. `max_frames`
/// bounds the walk (frame 0 included).
pub fn unwind(
    stack: &StackImage,
    frame: &RegisterFrame,
    cfi: &CfiTable,
    max_frames: usize,
) -> Result<Backtrace, UnwindError> {
    if frame.abi != FrameAbi::HOST {
        return Err(UnwindError::WrongAbi(frame.abi));
    }
    if stack.bytes.len() < SWITCH_FRAME_BYTES {
        return Err(UnwindError::Truncated);
    }
    let eh = EhFrame::new(&cfi.eh_frame, LittleEndian);
    let bases = BaseAddresses::default().set_eh_frame(cfi.eh_frame.as_ptr() as u64);
    let mut ctx = gimli::UnwindContext::new();

    // Frame 0: the switch frame's registers; the switch returns to `pc`
    // with the stack pointer just above the frame.
    let mut regs: Regs = ARCH
        .callee_saved
        .iter()
        .filter_map(|(n, name)| frame.get(name).map(|v| (*n, v)))
        .collect();
    let mut pc = frame.return_address;
    let mut sp = stack.saved_sp + SWITCH_FRAME_BYTES as u64;
    let mut frames = vec![];
    let stop = loop {
        if pc == 0 {
            break Stop::End;
        }
        if frames.len() == max_frames {
            break Stop::FrameLimit;
        }
        let index = frames.len();
        // `pc` is a return address: the call is the instruction before it.
        let Some(function) = cfi.function_at(pc - 1) else {
            break Stop::NoCfi { pc };
        };
        let fde = eh
            .fde_for_address(&bases, pc - 1, EhFrame::cie_from_offset)
            .map_err(|e| UnwindError::Cfi {
                frame: index,
                detail: e.to_string(),
            })?;
        let row = fde
            .unwind_info_for_address(&eh, &bases, &mut ctx, pc - 1)
            .map_err(|e| UnwindError::Cfi {
                frame: index,
                detail: e.to_string(),
            })?;
        let value_of = |r: Register| {
            if r.0 == ARCH.sp {
                Some(sp)
            } else {
                get(&regs, r.0)
            }
        };
        let cfa = match row.cfa() {
            CfaRule::RegisterAndOffset { register, offset } => value_of(*register)
                .ok_or_else(|| UnwindError::Unsupported {
                    frame: index,
                    detail: format!("CFA register {} has no known value", register.0),
                })?
                .wrapping_add_signed(*offset),
            CfaRule::Expression(_) => {
                return Err(UnwindError::Unsupported {
                    frame: index,
                    detail: "CFA expression".into(),
                });
            }
        };
        if cfa <= sp || cfa > stack.top {
            return Err(UnwindError::BadCfa { frame: index, cfa });
        }
        let read = |address: u64| {
            stack.word_at(address).ok_or(UnwindError::Unreadable {
                frame: index,
                address,
            })
        };
        let recover = |r: u16| -> Result<Option<u64>, UnwindError> {
            Ok(match row.register(Register(r)) {
                RegisterRule::Undefined => {
                    // Callee-saved registers keep their value unless a rule
                    // says otherwise; the return-address column has none.
                    if r == ARCH.ra { None } else { get(&regs, r) }
                }
                RegisterRule::SameValue => value_of(Register(r)),
                RegisterRule::Offset(o) => Some(read(cfa.wrapping_add_signed(o))?),
                RegisterRule::ValOffset(o) => Some(cfa.wrapping_add_signed(o)),
                RegisterRule::Register(other) => value_of(other),
                RegisterRule::Constant(v) => Some(v),
                other => {
                    return Err(UnwindError::Unsupported {
                        frame: index,
                        detail: format!("register {r} rule {other:?}"),
                    });
                }
            })
        };
        let mut caller: Regs = vec![];
        for (n, _) in ARCH.callee_saved {
            if let Some(v) = recover(*n)? {
                caller.push((*n, v));
            }
        }
        let ra = recover(ARCH.ra)?.unwrap_or(0);
        frames.push(UnwoundFrame {
            pc,
            sp,
            cfa,
            function: function.name.clone(),
            registers: ARCH
                .callee_saved
                .iter()
                .filter_map(|(n, name)| get(&regs, *n).map(|v| (*name, v)))
                .collect(),
        });
        regs = caller;
        sp = cfa;
        pc = ra;
    };
    Ok(Backtrace {
        frames,
        stop,
        vector: frame.vector.clone(),
    })
}
