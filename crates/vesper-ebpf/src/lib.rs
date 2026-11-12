//! Vesper eBPF (design §27): an eBPF instruction codec and assembler, the
//! §27.5 pre-flight abstract verifier, linear host-side builders for
//! ringbuf reservations and socket references, BPF ELF emission, a
//! concrete userspace interpreter, and the §27.3 XDP SYN-flood mitigator.
//!
//! Rust ownership in [`linear`] is host-library discipline for programs
//! built through this crate's API. It is not an implementation of the
//! Vesper source typing rules (Theorem 2, [T-Let]); a compiler eBPF backend
//! from Vesper source is a later milestone.

#![deny(unsafe_code)]

pub mod callgraph;
pub mod elf;
pub mod helpers;
pub mod insn;
pub mod interp;
#[cfg(target_os = "linux")]
pub mod kernel;
pub mod mitigator;
pub mod program;
pub mod scalar;
pub mod verifier;
pub mod wire;

pub use callgraph::{CallGraph, FnId, FnNode};
pub use insn::{Insn, RawInsn, Reg};
pub use program::{Asm, MapDef, MapKind, Program, ProgramType};
pub use verifier::{
    BPF_MAX_STACK_BYTES, VerifierFinding, VerifierPolicy, VerifyError, VerifyReport, findings,
    verify,
};
