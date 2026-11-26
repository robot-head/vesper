//! Frozen Rust seed copy of `vesper-ebpf` for the compiler's own dependency
//! closure (`cargo tree -p vesper`) and `vesper-ebpf` differential/ABBA tests.

#![deny(unsafe_code)]

/// Which build this is: `"rust"` for the seed copy `vesper-ebpf-seed`.
pub const IMPLEMENTATION: &str = "rust";

#[path = "engine/btf.rs"]
pub mod btf;
#[path = "engine/btf_ext.rs"]
pub mod btf_ext;
#[path = "../../src/builder.rs"]
pub mod builder;
#[path = "engine/callgraph.rs"]
pub mod callgraph;
#[path = "engine/core.rs"]
pub mod core;
#[path = "../../src/elf.rs"]
pub mod elf;
#[path = "engine/helpers.rs"]
pub mod helpers;
#[path = "engine/insn.rs"]
pub mod insn;
#[path = "engine/interp.rs"]
pub mod interp;
#[cfg(target_os = "linux")]
#[path = "../../src/kernel.rs"]
pub mod kernel;
#[path = "engine/mitigator.rs"]
pub mod mitigator;
#[path = "engine/program.rs"]
pub mod program;
#[path = "engine/scalar.rs"]
pub mod scalar;
#[path = "engine/verifier.rs"]
pub mod verifier;
#[path = "../../src/wire.rs"]
pub mod wire;

pub use builder::{
    BpfBuilder, BpfRingSlot, BpfSocketRef, BuildError, LinearAudit, RingbufWakeup, SocketTuple,
};
pub use callgraph::{CallGraph, FnId, FnNode};
pub use insn::{Insn, RawInsn, Reg};
pub use program::{Asm, KfuncRef, LineInfo, MapDef, MapKind, Program, ProgramType};
pub use verifier::{
    BPF_MAX_STACK_BYTES, VerifierFinding, VerifierPolicy, VerifyError, VerifyReport, findings,
    verify,
};
