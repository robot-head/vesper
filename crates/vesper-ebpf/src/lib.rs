//! Vesper eBPF (design §27): an eBPF instruction codec and assembler, the
//! §27.5 pre-flight abstract verifier, BPF ELF emission, BTF with CO-RE
//! relocation (field offsets and bitfields, type ids, sizes, existence,
//! `TYPE_MATCHES`, enumerators; vmlinux then module BTF), a concrete
//! userspace interpreter for XDP, tc, tracepoint and kprobe programs, the
//! §27.3 XDP SYN-flood mitigator, and (Linux only, the one `unsafe`
//! module) kernel load and test-run evidence.
//!
//! The linear host-side builders combine consuming handles with checked
//! finalization, including the verifier's `ref.leak` rule on every emitted
//! path. This is Rust library discipline, not an implementation of Vesper
//! source typing rules or a compiler eBPF backend.

#![deny(unsafe_code)]

// Under `vesper-port` (package vesper-ebpf only; the compiler links the
// Rust seed copy, crates/vesper-ebpf/seed), the ported functions forward to
// the Vesper port in vesper/ through the generated binding (port wave P3a
// lane 6, #1041). The binding is committed outside src/ so formatters never
// reflow it (see docs/maintenance/porting-a-crate.md).
#[cfg(vesper_port)]
#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;
#[cfg(vesper_port)]
mod port;

/// Which build this is: `"vesper"` when the ported functions run the
/// Vesper port, `"rust"` for the Rust build (the default, and the seed copy
/// `vesper-ebpf-seed` that the compiler links).
pub const IMPLEMENTATION: &str = if cfg!(vesper_port) { "vesper" } else { "rust" };

pub mod btf;
pub mod btf_ext;
pub mod builder;
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
