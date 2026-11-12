//! Vesper eBPF (design §27): an eBPF instruction codec and assembler, the
//! §27.5 pre-flight abstract verifier, BPF ELF emission, a concrete
//! userspace interpreter, the §27.3 XDP SYN-flood mitigator, and (Linux
//! only, the one `unsafe` module) kernel load and test-run evidence.
//!
//! The linear host-side builders combine consuming handles with checked
//! finalization, including the verifier's `ref.leak` rule on every emitted
//! path. This is Rust library discipline, not an implementation of Vesper
//! source typing rules or a compiler eBPF backend.

#![deny(unsafe_code)]

pub mod builder;
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

pub use builder::{
    BpfBuilder, BpfRingSlot, BpfSocketRef, BuildError, LinearAudit, RingbufWakeup, SocketTuple,
};
pub use callgraph::{CallGraph, FnId, FnNode};
pub use insn::{Insn, RawInsn, Reg};
pub use program::{Asm, MapDef, MapKind, Program, ProgramType};
pub use verifier::{
    BPF_MAX_STACK_BYTES, VerifierFinding, VerifierPolicy, VerifyError, VerifyReport, findings,
    verify,
};
