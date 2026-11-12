# Fifth-wave integration contracts

Baseline: `af03122ea74d403f203170926945f9d35786b289` or a descendant. This is an
ownership and interface agreement so fourteen lanes can run at once. It is not a
soundness theorem, and it freezes nothing new in the existing compiler.

Wave five covers every design section that had no implementation on `main` at
the baseline (§23 Vault, §25 Chorus, §26 Silicon, §27 eBPF, §29 Crucible, §30
Nexus, §31 Chrono, §32 Resonance, §33 Aegis, §36 Kinetic), plus the three gaps
the Warp integration left open (TLS handshake, stream retransmission, source
simulation on the real software NIC). The [section status review](wave-five-review.md)
lists what remains in already-started sections for later waves.

## Work claimed by other agents (do not touch)

At planning time other agents hold open claims on §16.4 source macros and
Forge authority (issues #103–#109, PR #110) and on §39 Stylus formatting and its
proofs (issues #111–#120, PR #115). Wave-five lanes must not edit
`crates/vesper-forge*/**`, `crates/vesper-fmt/**`, `crates/vesper-lint/**`,
`crates/vesper-loom-sandbox/**` or the root macro and formatter modules. Where a
section asks for a Forge macro (`@choreography`, `@svd_pac`, `@btf_core`), the
lane implements the host library the macro would call and leaves the macro to a
later wave. Re-check the open issue and PR board before starting.

## Dependency rule

Every lane depends only on crates already on `main`. Allowed internal edges:

| Lane | Crate | May depend on |
| --- | --- | --- |
| 1 | new `vesper-vault` | none |
| 2 | new `vesper-chorus` | `vesper-simpulse` |
| 3 | new `vesper-silicon` | none |
| 4 | new `vesper-ebpf` | none |
| 5 | new `vesper-crucible` | `vesper-simpulse` |
| 6 | new `vesper-nexus` | `vesper-ffi` |
| 7 | new `vesper-chrono` | `vesper-simpulse`, `vesper-ffi` |
| 8 | new `vesper-resonance` | `vesper-simd` |
| 9 | new `vesper-aegis` | `vesper-loom-calculus`, `vesper-loom-cas` |
| 10 | new `vesper-kinetic` | `vesper-simd` |
| 11 | new `vesper-migrate` | `vesper-ffi` |
| 12 | new `vesper-warp-tls` | `vesper-warp-crypto` |
| 13 | existing `vesper-warp` | its existing dependencies |
| 14 | existing `vesper-sim-session` | its existing dependencies, `vesper-warp-umem` |

No lane imports another wave-five crate. A library crate must not depend on the
root `vesper` package: integration B makes the root depend on these crates, so
that edge would be a cycle. Lanes must not edit the read-only crates they
depend on.

## Shared vocabulary

Lanes define these locally with exactly these names and meanings, so
integration writes thin adapters instead of reconciling semantics.

- **Time** is `u64` nanoseconds on a caller-supplied virtual or monotonic
  clock. No library code reads a real clock.
- **Backend verifiers** (lanes 1, 3, 8, 10, and lane 4 for eBPF bytecode)
  analyse real input, never only a hand-built enum: lanes 1, 3 and 10 take
  `cranelift_codegen::ir::Function` at the workspace's Cranelift version
  (0.125), built in tests with `cranelift-frontend`. The section's sketched
  enum may be kept as an internal abstraction. Annotations that the source
  language cannot express yet (secret parameters, stack budgets, realtime
  roots) are passed in an explicit `…Policy` struct. Each verifier returns
  `Vec<VerifierFinding>` with
  `VerifierFinding { function: String, inst: Option<u32>, rule: &'static str, message: String }`,
  where `inst` is the Cranelift instruction index when there is one.
- **Call graphs** (lanes 3, 4, 8): `CallGraph { functions: Vec<FnNode>, calls: Vec<(FnId, FnId)> }`
  with `FnId(u32)` and per-function frame bytes. Indirect calls are an explicit
  edge kind that the analysis rejects unless the policy lists their targets.
- **Linear obligations** (`NonceTicket`, `SecretSlot`, `Session[P]`,
  `IrqGuard`, `DmaTransfer`, `BpfRingSlot`, WIT `own<T>`, `RtBufferLease`,
  `UnacceptedGpaRange`, rollback tickets) are non-Clone, non-Copy `#[must_use]`
  owners whose only exits are the documented consuming calls. A dropped owner
  is detected (debug panic or counted leak reported by an auditor), never
  silently released. Failed consuming operations return the owner. This is
  host-library discipline, not an implementation of the source typing rules;
  every report must say so.
- **Wire structs** quoted by the design keep their stated byte layouts where
  they are internally consistent. When a stated size disagrees with its fields,
  the lane picks the layout its fields imply, tests it, and records the
  correction in its report.

## Evidence and claims

- "0 Alloc" claims need a counting global allocator test over the warmed-up
  steady state.
- Hardware and kernel features (DIT/DOITM, mlock limits, MPK/PKRU, RDMA NICs,
  Intel PT/CoreSight, `bpf()` program load, TDX/SEV-SNP/CCA, FTZ/DAZ, GPU
  compute) are reported as unavailable unless actually exercised on the host or
  in CI. A software model validates the modelled protocol only.
- Signature and attestation checks use real cryptography from mature crates
  over test-generated keys and chains. Real vendor root certificates are only
  claimed if a test verifies a real vendor sample.
- Lean files compile with the pinned toolchain, contain no `sorry`, `admit`
  or new axioms, and are run from the owning crate's tests when `VESPER_LEAN`
  is set (follow `crates/vesper-simd/tests/model.rs`). Each report states
  exactly which statement is proved about which model, and that it is not a
  proof about the Rust code.
- Unsafe Rust is forbidden except in a lane's explicitly allowed, gated,
  commented module: lane 1 (mlock/madvise and DIT register access), lane 3
  (none; models only), lane 4 (the `bpf()` syscall load test), lane 8 (FTZ/DAZ
  control register), lane 10 (`std::arch` SIMD, optional). Prefer mature crates
  that already wrap these.

## Root and coordinator ownership

Each lane may commit only its own root registration patch (members,
default-members, new `[workspace.dependencies]`, `Cargo.lock`) on its PR
branch, in its own commit. CLI wiring, CI workflows, global docs, `src/**`,
`crates/vesper-lean-syntax/**` and `crates/vesper-loom-tuf/**` stay with the
integration steps. Report paths under `docs/parallel-results/` named in each
prompt are exclusive to their lane.

## Integration steps (not parallel lanes)

- **A, Warp handshake (#135)**, after lanes 12 and 13: `crates/vesper-warp`
  sends and accepts long-header packets, runs the lane-12 handshake over the
  lane-13 CRYPTO buffers, and opens a connection with no caller-supplied
  secrets.
- **B, CLI and compiler hooks (#136)**, after lanes 1–11: root wiring for
  `vesper fuzz`, `vesper verify --refute`, `vesper migrate`, `vesper replay`,
  `vesper ebpf check`, and running the Vault, Silicon, Resonance and Kinetic
  verifiers on the Cranelift functions `src/native.rs` emits, behind explicit
  flags. It also merges the lane-3 and lane-4 stack analyzers into one.
