//! QEMU 8.2.2's output for the hypervisor script of `tests/aia_qemu.rs`,
//! shared with `tests/lean.rs`, which checks that `lean/SiliconAia.lean`'s
//! `qemuHyp` (the list its `hypTrace_matches_qemu` theorem proves the Lean
//! model reproduces) is this same list.

/// The result of an op that traps (illegal instruction, mcause 2).
pub const FAULT: u64 = 0xfa17_0000_0000_0002;

/// Recorded on QEMU 8.2.2 (Ubuntu 1:8.2.2+ds-0ubuntu1.18),
/// `virt,aia=aplic-imsic,aia-guests=2 -smp 4`.
pub const HYPERVISOR_EXPECTED: &[u64] = &[
    0, 6, 0, 0, 1, 0x20, 0, 0x50005, 2, 0x1400, 0x20, 0, 0, 0, 2, 0x400, 0x1400, 0x50005, 0, 0, 0,
    FAULT, FAULT, FAULT, FAULT, 0, FAULT, FAULT, 0, 0, 0, 4, 0x1400, 0x70007, 0x1000, 0, 0, 0,
    FAULT, FAULT, 0x70007, 0, 0,
];
