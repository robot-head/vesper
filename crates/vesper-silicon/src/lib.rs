//! Vesper Silicon (§26): host-side verifiers and protocol models for
//! bare-metal code.
//!
//! * [`stack`]: worst-case stack depth over real Cranelift 0.125 functions
//!   compiled for x86_64, AArch64 and riscv64, with frames read from the emitted
//!   machine code, recursion and unresolved-call rejection, and
//!   `@stack_limit` budgets per root.
//! * [`atomic`]: IrqGuard / sleep-in-atomic / ISR-suspension / lock-order
//!   checks over the same functions and call graph.
//! * [`svd`] and [`hazard`]: a CMSIS-SVD register model (parsed by
//!   `svd-parser`; exercised on the vendor RP2040 SVD) and MMIO hazard
//!   checks (W1C read-modify-write, plain reads of read-to-clear or
//!   read-side-effect registers, writes to read-only fields) over operation
//!   traces and over Cranelift loads/stores.
//! * [`pac`]: generation of host-side typestate accessor modules (Rust text)
//!   from the register model.
//! * [`dtb`]: flattened-devicetree input (versions 16 and 17) for memory
//!   banks, fixed and dynamic reservations, bus `ranges` translation with up
//!   to 4 address cells, `interrupt-map` nexus resolution, interrupt
//!   controllers (RISC-V hart intc, one or more PLICs, APLIC, IMSIC,
//!   CLINT/ACLINT, GIC) with trigger types and `interrupt-map-pass-thru`,
//!   ISA extensions, DMA coherence and `dma-ranges`, feeding the frame,
//!   IrqGuard and DMA models.
//! * [`aia`]: RISC-V AIA register models: APLIC domains (direct delivery
//!   through IDCs and MSI forwarding, delegation, edge/level/detached source
//!   modes) and IMSIC interrupt files (`seteipnum`, `eidelivery`,
//!   `eithreshold`, `eip`/`eie`, `*topei`).
//! * [`ccache`]: the SiFive L2 cache controller's `FLUSH64`/`FLUSH32` and
//!   way-enable registers, used for non-coherent DMA on the StarFive JH7100.
//! * [`intc`]: PLIC, APLIC, IMSIC and CLINT/ACLINT register-level emulation
//!   configured from devicetree numbers (with M-mode firmware setup of AIA
//!   MSI addresses and delegation), delivering interrupts to the IrqGuard
//!   `Cpu` model of each hart.
//! * [`mmio`]: access-typed registers and typestate peripheral singletons
//!   bound at run time from the model, on a software register file.
//! * [`irq`], [`dma`], [`frames`]: linear `IrqGuard`, `DmaTransfer` and
//!   `PhysFrame4K`/`PendingTlbShootdown` owners with a leak [`ledger`] and a
//!   frame-conservation auditor, plus the per-architecture DMA barrier and
//!   TLB-invalidation instruction sequences, including Zicbom `cbo.*`
//!   maintenance for non-coherent RISC-V DMA (gated by `menvcfg`/`senvcfg`
//!   `CBIE`/`CBCFE`) and a software cache model.
//!
//! Scope and evidence: there is no freestanding target, no real hardware,
//! interrupt controller (the PLIC, APLIC, IMSIC, CLINT and SiFive cache
//! controller are software models), page table or DMA engine here, and no `@svd_pac` macro; every hardware object is a
//! software model and validates the modelled protocol only. The linear
//! owners are host-library discipline (non-`Clone`, `#[must_use]`, consuming
//! exits, counted leaks); they are not an implementation of the Vesper source
//! typing rules for `linear` values.

#![forbid(unsafe_code)]

pub mod aia;
pub mod atomic;
pub mod ccache;
pub mod dma;
pub mod dtb;
pub mod frames;
pub mod hazard;
pub mod intc;
pub mod irq;
pub mod ledger;
pub mod mmio;
pub mod pac;
pub mod prologue;
pub mod stack;
pub mod svd;

/// Target architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
    /// riscv64 (RV64GC as Cranelift 0.125 targets it; the compressed
    /// encodings only when enabled with `has_zca`/`has_zcd`).
    Riscv64,
}

/// The wave-five verifier finding shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifierFinding {
    pub function: String,
    /// Cranelift instruction index (or trace op index) when there is one.
    pub inst: Option<u32>,
    pub rule: &'static str,
    pub message: String,
}
