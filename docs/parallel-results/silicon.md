# Wave five, lane 3: Vesper Silicon (§26)

Claim: issue #123. Branch `claude/project-thread-foentg`, based on
`af03122ea74d403f203170926945f9d35786b289` (origin/main at start).
New crate `crates/vesper-silicon`, Lean model `lean/SiliconFrames.lean`.

## What is implemented

| §26 item | Module | Evidence |
| --- | --- | --- |
| 26.5 / 26.1.2 worst-case stack depth over Cranelift machine frames | `stack`, `prologue` | `tests/stack.rs` (10 tests, x86_64 and AArch64) |
| 26.1.3 IrqGuard, sleep-in-atomic, ISR suspension, lock order | `atomic` (static), `irq` (protocol model) | `tests/atomic.rs` (7), `tests/runtime.rs`, 3 doctests |
| 26.1.4 CMSIS-SVD register model and W1C/RC/RO hazards | `svd`, `hazard` | `tests/svd.rs` (4) |
| 26.1.4 typestate peripheral singletons, access-typed registers | `mmio` | `tests/runtime.rs`, 4 `compile_fail` doctests |
| 26.1.5 / 26.2 / 26.3 DMA transfers, descriptor, barriers | `dma` | `tests/runtime.rs` |
| 26.1.5 / 26.3 page frames, unmap + TLB shootdown, auditor | `frames`, `ledger` | `tests/runtime.rs` incl. 64-seed randomized conservation test |
| 26.4 W1C safety and frame conservation proofs | `lean/SiliconFrames.lean` | `tests/lean.rs` (elaborated under `VESPER_LEAN`) |

### Stack analyzer (`stack`, `prologue`)

`Program::compile(arch, Vec<cranelift_codegen::ir::Function>)` compiles each
function with `cranelift-codegen` 0.125 (`opt_level=speed`, probestack off,
frame pointers kept) for `x86_64` or `aarch64`. The activation size of every
function is **read from the emitted machine code**: `prologue` decodes the
prologue Cranelift emits (`push rbp; mov rbp,rsp; sub rsp,imm8/imm32; mov
[rsp+d],r` on x86_64 plus the caller's 8-byte return address; `stp x29,x30,
[sp,#-16]!; mov x29,sp; stp/str …,[sp,#-n]!; sub sp,sp,#imm{,lsl 12}` or
`movz/movk w16|x16 + sub sp,sp,x16` on AArch64) and sums every SP decrement.
The result is cross-checked against `CompiledCode::frame_size`; a decoded
total below the layout minimum is a typed `FrameMeasurementMismatch` error,
never a guess. (A 70 000-byte frame on AArch64 first failed that check, which
is how the `movz w16` form was found and added.)

The call graph (`CallGraph { functions, calls, indirect_calls,
external_calls }` with `FnId(u32)` and per-function frame bytes, per the
wave-five contract) comes from the IR's `call`, `return_call`, `try_call`,
`call_indirect`, `return_call_indirect` and `try_call_indirect`
instructions, plus libcall relocations in the machine code (for example
`ceil` on baseline x86_64 becomes a call to `libcall:CeilF64` that is not
visible in the IR). `check_stack(&StackPolicy)` runs Tarjan's SCC algorithm
and rejects every recursive SCC (`silicon.stack.recursion`), indirect calls
without a policy target list, and external calls without a declared bound;
for each `@stack_limit` root it computes the longest path in reverse
topological order with `u64` accumulation and reports the worst path, and a
`silicon.stack.budget_exceeded` finding when over budget or unbounded.
Findings use the contract's `VerifierFinding { function, inst, rule,
message }` with the Cranelift instruction index where there is one.

Conservative points: tail calls are counted as ordinary calls (an
over-approximation); findings for unresolved/recursive functions are
graph-wide, not only for functions reachable from a root.

Measured on this host (x86_64 Linux): for `fn(i64)->i64` functions with a
32/256/64/16-byte explicit slot, frames were 48/272/80/32 bytes on both
ISAs: on x86_64 the 16 bytes over the slot are the return address pushed by
`call` plus `push rbp`; on AArch64 they are the `stp x29, x30` frame record
(`bl` pushes nothing). These are measurements of Cranelift 0.125's output on
this host, not of real hardware stacks.

### Atomic-context checker (`atomic`)

`check_atomic(&Program, &AtomicPolicy)` takes the guard acquire/release
symbols, lock specs (acquire/release symbol, ISR-shared flag), suspending and
allocating symbols, `@isr` roots and indirect-call targets. It runs a forward
dataflow over each function's CFG tracking held guards and locks, and an
interprocedural BFS that finds the shortest path from each function to a
forbidden symbol. Rules: `sleep_in_atomic`, `alloc_in_atomic`,
`suspend_in_isr`, `alloc_in_isr`, `unresolved_call_in_atomic`,
`lock_without_guard` (ISR-shared lock outside ISR without a guard),
`recursive_lock`, `release_not_held`, `unbalanced_join`,
`guard_or_lock_escapes` (returning with a guard or lock held: the IR-level
form of "a linear IrqGuard cannot leave its scope"), and `lock_order_cycle`
(Tarjan over the lock-order graph, including order induced by callees'
transitive lock sets).

### SVD model and hazards (`svd`, `hazard`)

Parsing uses `svd-parser` 0.14.10 with `expand` (derivedFrom, `dim` arrays)
and property inheritance. Each field becomes one of `ReadWrite`, `ReadOnly`,
`WriteOnly`, `Write1ToClear`, `ReadToClear`, `Write1ToSet`, `Write1ToToggle`,
`Write0ToClear`; any other `modifiedWriteValues`/`readAction` combination is
`SvdError::UnsupportedAccess`. Overlapping or out-of-range fields, missing
or unsupported sizes, address overflow and duplicate paths are typed errors.
The fixture `crates/vesper-silicon/fixtures/vsim32.svd` is hand-written to the
CMSIS-SVD 1.3 schema (SPI1 + derived SPI2, an Ethernet-MAC W1C interrupt
block, a UART with a read-to-clear data register); it describes no real
device.

Hazard rules (`check_ops` over an operation trace and `check_function` over
Cranelift loads/stores at constant addresses): RMW of a register with a
W1C/W1T/W0C field (detected in IR as a store whose value data-depends on a
load of the same register), a plain read of a read-to-clear register
(allowed only as `Consume`, or in IR only in functions the policy lists as
consumers), writes that set read-only bits or any write to a read-only
register, reads of write-only registers, out-of-range values, unmodelled
addresses. Non-constant addresses are counted and not analysed.

### Typestate MMIO (`mmio`)

`SimBus` is a software register file applying each field's semantics.
`Reg<A>` binds a register path only when the model's class equals the marker
(`ReadOnly`, `ReadWrite`, `WriteOnly`, `Write1ToClear`, `ReadToClear`);
`Reg<Write1ToClear>` has `read` and `write_clear_mask` but no `modify`,
`Reg<ReadToClear>` has only `consume`. `Spi<Disabled>::take` binds `CR1/SR/DR`
of a named peripheral from the model at run time, is a per-bus singleton, and
`into_master` → `Spi<MasterReady>` → `transfer`/`disable`; `transfer` on a
disabled SPI does not compile (doctest).

### IrqGuard, DMA, frames (`irq`, `dma`, `frames`, `ledger`)

All linear owners (`IrqGuard`, `DmaTransfer<B>`, `PhysFrame4K`,
`PendingTlbShootdown`) are non-`Clone`, `#[must_use]`, have only documented
consuming exits, return themselves from failed consuming calls, and register
with a `Ledger`; a drop without the consuming call is a counted leak that the
`FrameAuditor` reports. `Cpu` models DAIF.I / RFLAGS.IF / mstatus.MIE with
LIFO nested guards; `IsrSpinLock::lock(&IrqGuard)` borrows the guard so the
lock ends before interrupts can be restored; interrupts raised while masked
are delivered on the unmasking restore. `dma_barriers` returns `DC CVAC`
per line + `DSB SY` before the device and `DC IVAC` per line + `DSB SY`
after a device write on AArch64, `FENCE IORW, IORW` on RISC-V and nothing on
coherent x86_64. `SgDmaDescWire` is the 16-byte layout of §26.2 (its fields
sum to 16; no correction needed). `PageTable::unmap_leaf_4k` returns a
`PendingTlbShootdown` whose `flush_ipi_and_reclaim` records the per-arch
invalidation (`DSB ISHST; TLBI VAE1IS; DSB ISH; ISB`, `INVLPG`,
`SFENCE.VMA`) and clears stale translations in the `SmpIpi` model before the
frame comes back. The randomized test runs 64 seeds × 600 random
alloc/free/map/unmap/shootdown/DMA submit/complete/poll steps over two
address spaces and checks, after every step, `total = free + mapped +
pending + in_dma + held`, no duplicate frame owner, and no leaks; it then
drains everything and checks the allocator is full.

### Lean (`lean/SiliconFrames.lean`)

Checked with the pinned Lean 4.24.0 (`lean lean/SiliconFrames.lean`, clean,
no warnings); no `sorry`, `admit`, axioms or `native_decide`. Proved, about
the models in that file only (not about the Rust code):

- registers as `Fin n → Bool` with the W1C write rule `hw i && !w i`:
  `w1c_preserves_unwritten`, `w1c_clears_written`, `w1c_single_ack` (a
  one-hot acknowledge leaves every other bit unchanged) and
  `rmw_clears_all_pending` (writing back the read value clears every pending
  bit: the hazard the checker rejects);
- memory as a list of `(pfn, location)` with seven operations moving one
  frame along a fixed transition table: `step_preserves_pfns` /
  `steps_preserve_pfns` (the frame list is unchanged by any step or
  sequence), `step_preserves_nodup`, `count_partition` and
  `step_conserves_total` (free + held + mapped + pending + dma = total),
  `mapped_only_to_pending`, `pending_exit_is_shootdown` and
  `no_reuse_without_shootdown`, plus a `decide`-checked concrete trace.

## Checks (exact commands)

```
cargo fmt --all -- --check
VESPER_LEAN=…/lean-4.24.0-linux/bin/lean cargo test -p vesper-silicon      # 38 tests + 6 doctests
VESPER_LEAN=…/lean cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
lean lean/SiliconFrames.lean
```

Results are recorded in the PR description.

## Root patch

One separate commit: `crates/vesper-silicon` added to `members` and
`default-members` before `experiments/native-pulse`, and `Cargo.lock`
regenerated (adds `svd-parser` 0.14.10, `svd-rs` 0.14.13, `roxmltree`,
`regex`). No new `[workspace.dependencies]`; the crate pins
`svd-parser = "=0.14.10"` and uses `cranelift-codegen` 0.125 (already in the
lockfile). No other root, CLI, CI or `src/**` edits.

## Not done / limits

- No freestanding target, real hardware, interrupt controller, MMIO, page
  tables or DMA engine: every hardware object is a software model and
  validates the modelled protocol only. No `@svd_pac` macro (later wave), no
  Devicetree input.
- The stack analyzer is not wired to the Cranelift functions `src/native.rs`
  emits; integration B (#136) does that and merges it with lane 4's analyzer.
  RISC-V is rejected by the analyzer (`UnsupportedIsa`: the Cranelift
  riscv64 backend is not enabled in the workspace).
- Prologue decoding covers the sequences Cranelift 0.125 emits with the
  settings above; other sequences stop the decoder and are then caught by the
  layout cross-check, not silently accepted.
- The linear owners are host-library discipline. They are not an
  implementation of the Vesper source typing rules for `linear` values, and
  the atomic checker works on IR call symbols named by a policy, not on
  source `eff [...]` rows.
- RISC-V DMA uses only `FENCE IORW, IORW` as the design states; Zicbom
  cache-block operations for non-coherent RISC-V DMA are not modelled.
- No performance claims; no "0 Alloc" claims are made by this crate.
