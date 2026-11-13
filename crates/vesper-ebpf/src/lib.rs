//! Vesper eBPF (design §27): an eBPF instruction codec and assembler, the
//! §27.5 pre-flight abstract verifier, BPF ELF emission, a concrete
//! userspace interpreter, the §27.3 XDP SYN-flood mitigator, and (Linux
//! only, the one `unsafe` module) kernel load and test-run evidence.
//!
//! Not in this crate: a compiler eBPF backend from Vesper source, and the
//! linear `BpfRingSlot`/`BpfSocketRef` host-side builders of the lane
//! prompt (dropped by the project owner; see
//! `docs/parallel-results/ebpf.md`). The obligation to submit, discard or
//! release every acquired reference on every path is enforced on the
//! bytecode by the verifier's `ref.leak` rule. That is a check of emitted
//! programs, not an implementation of the Vesper source typing rules
//! (Theorem 2, [T-Let]).

#![deny(unsafe_code)]

pub mod btf;
pub mod btf_ext;
pub mod callgraph;
pub mod core;
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
pub use program::{Asm, KfuncRef, LineInfo, MapDef, MapKind, Program, ProgramType};
pub use verifier::{
    BPF_MAX_STACK_BYTES, VerifierFinding, VerifierPolicy, VerifyError, VerifyReport, findings,
    verify,
};
