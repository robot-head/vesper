# RFC: Native Pulse continuation ABI v1 (prototype)

Status: prototype, not integrated. Owner paths: `crates/vesper-fiber-abi/**`,
`experiments/native-pulse/**`, this file. Design references: §7 (stage 4/5
lowering of suspending linear handlers to stack switches), §10 (workspace
crates), §12.5/§12.7 (evidence vector, `vesper_fiber_yield`/`resume`,
`FiberHeader`).

This RFC records what the prototype implements, the contracts generated code
and the runtime must keep, the evidence for each claim, and a narrow adapter
for wiring it into real source lowering. Nothing here changes `src/native.rs`,
the evaluator, or `vesper-pulse` internals.

## 1. What exists

| Piece | Where | Notes |
| --- | --- | --- |
| Versioned `repr(C)` layouts and offsets | `src/abi.rs` | `ABI_VERSION = 1`, magic `VFIB`. Every offset used by codegen is a const checked against `offset_of!` at compile time. |
| Evidence identities and vectors | `src/evidence.rs` | Monotonic, never-reused `EvidenceId`s; O(1) static-depth slots. |
| Cranelift emission helpers | `src/clif.rs` | `declare_park`, `load_evidence`, `emit_park`, `branch_on_kind`, `emit_exit`, `emit_violation`. The only code that encodes ABI offsets into CLIF. |
| Pinned fiber stacks | `src/stack.rs` | `mmap` + `PROT_NONE` guard page, 256 KiB default. |
| Context switch | `src/switch.rs` | `global_asm!` trampolines for System V x86_64 and AAPCS64 Linux. |
| One-shot fibers | `src/fiber.rs` | `Fiber`, move-only `Parked`, `ContinuationTable`, `vesper_fiber_park`. |
| Relocatable data frame | `src/data.rs` | Base-relative data, relocation with poisoned retired buffers, transfer through `vesper_pulse::frames::FrameStack`. |
| Scheduler adapter | `src/pulse.rs` | `NativeTask: vesper_pulse::scheduler::Resume`. |
| Prototype entries | `src/prototype.rs` | Hand-built CLIF (not source lowering): `pulse_read`, `nested`, and a deliberately invalid `stale_base` fixture. JIT for the host; objects for x86_64/AArch64. |
| Demo | `experiments/native-pulse` | N fibers on the Pulse scheduler (kernel io_uring when available), writes both objects and their CLIF. |

The switch is real stack switching: generated code calls the imported symbol
`vesper_fiber_park`, which saves callee-saved registers on the fiber's own
stack and installs the scheduler's stack pointer. Resumption switches back and
the `call` in generated code returns. There is no interpreter, no VM, and no
evaluator fallback anywhere in the crate.

## 2. Layouts (ABI v1, 64-bit only)

All pointers are stored as `u64`. Little-endian is assumed by tests, not by
the layout.

`FiberContext` (`repr(C, align(64))`, 192 bytes): the `FiberHeader` of §12.7.

| Offset | Field | Writer | Meaning |
| --- | --- | --- | --- |
| 0 | `magic: u32` | runtime | `b"VFIB"` |
| 4 | `abi_version: u32` | runtime | `1` |
| 8 | `state: u32` | runtime | `Created 0`, `Running 1`, `Parked 2`, `Exited 3`, `Poisoned 4` |
| 12 | `flags: u32` | runtime | bit 0 `STACK_PINNED` (always set in v1) |
| 16 | `generation: u64` | runtime | +1 on every park and every resume |
| 24 | `fiber_id: u64` | runtime | process-unique, never reused |
| 32 | `saved_sp: u64` | trampoline | fiber SP while not running (points into the pinned stack) |
| 40 | `scheduler_sp: u64` | trampoline | resumer SP while the fiber runs |
| 48 | `evidence: *const EvidenceVectorHeader` | runtime | borrowed, outlives the fiber |
| 56 | `data_base: u64` | runtime | data-frame base at the latest (re)entry |
| 64 | `data_len: u64` | runtime | data-frame length in bytes |
| 72 | `park: ParkRequest` | generated code | valid while `Parked` |
| 104 | `resume: ResumeSignal` | resumer | valid after `vesper_fiber_park` returns |
| 136 | `entry: u64` | runtime | generated entry address |
| 144 | `entry_arg: u64` | runtime | entry argument |
| 152 | `exit_status: u64` | runtime | entry return value once `Exited` |
| 160 | `result: u64` | generated code | result word |
| 168 | `stack_base`, 176 `stack_len` | runtime | usable stack range (diagnostics, scans) |

There is no `saved_ip` field. The resume address is the trampoline's return
address, stored inside the pinned stack at `saved_sp`. A separate field would
be redundant, and as §6 explains, `saved_sp`/`saved_ip` are not sufficient to
move a stack anyway.

`ParkRequest` (32 bytes): `evidence u64 @0`, `op u32 @8`, `flags u32 @12`,
`arg0 u64 @16`, `arg1 u64 @24`.

`ResumeSignal` (32 bytes): `kind u32 @0` (`Value 0`, `Error 1`,
`Cancelled 2`), `value u64 @8`, `data_base u64 @16`, `evidence u64 @24`.

`EvidenceVectorHeader` (16 bytes): `abi_version u32 @0`, `len u32 @4`,
`slots *const EvidenceSlot @8`.

`EvidenceSlot` (32 bytes): `identity u64 @0`, `effect u32 @8`, `depth u32 @12`,
`handler_fn Option<extern "C" fn(*mut u8, u32, u64) -> u64> @16`,
`state_ptr *mut u8 @24`.

Exit statuses (entry return register): `COMPLETE 0`, `FAILED 1`,
`CANCELLED 2`, `ABI_VIOLATION 3`.

### Deviation from §12.7's `EvidenceSlot`

The design's slot carries `scheduler_sp`/`scheduler_ip` per slot. In v1 the
scheduler context lives once per fiber in `FiberContext`; slots carry an
identity, a static effect tag, and an optional direct handler. Every
suspending operation therefore returns to the fiber's own resumer. A handler
installed *inside* a fiber that itself wants to capture a sub-continuation
(nested fibers) is not supported in v1; see §10.

## 3. Calling conventions

All three functions use the target's default C convention
(`isa.default_call_conv()`: System V on x86_64, AAPCS64 on AArch64).

* Entry: `extern "C" fn(ctx: *mut FiberContext, arg: u64) -> u64`. Runs on
  the fiber stack. Returns an exit status and writes `ctx.result`.
* Park: `extern "C" fn vesper_fiber_park(ctx: *mut FiberContext) -> *const ResumeSignal`.
  Generated code fills `ctx.park` first. A null return means the context was
  not a running v1 context, so generated code must take its violation edge.
  The park call clobbers caller-saved registers as any call does. It preserves
  callee-saved registers and the whole pinned stack, so every SSA value and
  stack slot defined before the park is intact after it.
* Direct handler (tail-resumptive tier, §12.5 item 13):
  `extern "C" fn(state: *mut u8, op: u32, arg: u64) -> u64`, called with
  `call_indirect` from the slot. No switch occurs.

`emit_park` also checks `signal.evidence == park.evidence` before
continuing, so generated code never consumes an answer meant for another
handler installation, even if a buggy resumer bypassed the Rust API.

Switch frame contents: x86_64 saves `rbp rbx r12–r15` plus the return address
(56 bytes); AArch64 saves `x19–x30` and `d8–d15` (176 bytes). The FP control
state (MXCSR, x87 CW, FPCR) is not saved. The prototype's generated code
never changes it.

## 4. Ownership

| Object | Owner | Borrowers | Freed when |
| --- | --- | --- | --- |
| `FiberContext` | `Fiber` (raw `Box`) | generated code (during entry), trampolines | `Fiber` dropped (not running) |
| Fiber stack | `Fiber` | the fiber's own frames | `Fiber` dropped (not running) |
| Data frame | `Fiber` | generated code between (re)entry and next park; resumer while parked | `Fiber` dropped |
| Evidence vector, handler state | `Rc<EvidenceVector>` shared by installer and fibers | generated code (O(1) loads) | last `Rc` |
| Machine code | `Rc<JitCode>` shared by `JitPrototype` and each fiber | return addresses on parked stacks | last `Rc` (`JITModule::free_memory`) |
| Parked continuation | exactly one `Parked` value | none | `resume` consumes it |

Caller/callee rules:

* The resumer (scheduler side) owns the continuation. Generated code never
  owns its context. It borrows it for the duration of its entry and may not
  stash the pointer anywhere that outlives the entry.
* `Parked::resume(self, ..)` consumes the continuation. A compile-fail doctest
  shows a second `resume` is a use-after-move. A rejected resume (wrong
  evidence) returns the untouched continuation in `Rejected`, so a failed
  attempt never loses ownership.
* A raw token `RawContinuation { fiber_id, generation }` is what a completion
  queue would carry in `user_data`. `ContinuationTable::resume` rejects
  `UnknownToken` (already resumed or never inserted), `StaleToken` (the fiber
  re-parked at a newer generation), and `WrongEvidence`, and leaves the table
  unchanged on every rejection.
* `NativeTask` implements `vesper_pulse::scheduler::Resume` without changing
  the scheduler. Every queue transition moves the task. `Step::Io` moves the
  task's `File` and a fresh buffer into the driver; the completion moves them
  back. The existing one-shot completion ownership (`Resume::resume(self)`,
  submit-failure returns the request, poll errors keep harvested completions)
  is used unchanged.

## 5. Resumption, errors and cancellation

* `Value(v)`: the operation produced `v`.
* `Error(code)`: the operation failed (errno for I/O). Generated code takes its
  error edge. The adapter maps `FAILED` to `Step::Failed`.
* `Cancelled`: the scheduler is cancelling (`Event::Cancelled`). The fiber is
  still resumed exactly once and takes its cancellation edge. Linear
  obligations are discharged there; per §12.2 the source type system only
  permits abortive cancellation at `Cancel` points. The prototype's edge
  writes `CANCEL_NOTE` into a place and exits `CANCELLED`. An in-flight read
  cancelled by the driver arrives as `ECANCELED` (125) and takes the error
  edge. This matches the existing scheduler/driver contract.
* A fiber cancelled while still `Created` is dropped without running.
* Dropping a `Parked` delivers one `Cancelled` resume. If the fiber parks again
  during cancellation, it is abandoned: its stack is freed (memory safe, since
  nothing executes on it or points into it), its remaining obligations are
  lost, and `fiber::abandoned_fibers()` counts it. The runtime never unwinds
  Rust panics through generated frames; `extern "C"` aborts on panic.
* ABI violations (parking from a non-running context, mismatched evidence)
  make generated code return `ABI_VIOLATION`, which the adapter reports as
  `Step::Failed`.

## 6. Relocation invariant

**Physical stacks are pinned.** A suspended native stack contains absolute
addresses of itself: saved frame pointers (`rbp`/`x29` chains), the switch
frame's saved registers, and spilled addresses of stack slots (for example, a
`stack_addr` passed to a handler). `saved_sp` and a resume IP do not describe
those words. Copying the stack bytes and updating `saved_sp` would resume
frames that still point at the old stack. Cranelift provides no stack maps for
arbitrary pointers into the stack, so such words cannot be found and rewritten
soundly. The test `suspended_physical_stack_contains_self_pointers` scans the
suspended stack at both optimization levels and finds such self-pointers. ABI
v1 therefore sets `STACK_PINNED` and exposes no stack-relocation operation.
The `Fiber`/`Parked` *handle* moves freely, because the context is boxed and
the stack is a separate mapping
(`continuation_handles_move_between_owners`).

This is a narrower reading of §12.5 item 3 (the "Zero-Pin Stack Relocation
Corollary"). The absence of first-class `&'a` borrows in Vesper source does
not remove compiler-generated self-pointers from native frames.

**What moves: the data frame.** Data that the resumer may move or write while
the fiber is parked lives in a separate data frame under a base-relative
invariant:

1. The frame contains no absolute address of itself (references inside it are
   offsets, as in `vesper_pulse::frames`).
2. Generated code holds the frame's base only from a (re)entry to the next
   park. Each park returns the current base in `ResumeSignal::data_base`, and
   code uses only that value afterwards. `clif::emit_park` returns it as
   `ParkResult::data_base`; the CLIF helpers never reuse an older base.
3. While parked, the owner may copy the frame to any 8-aligned allocation
   (`Parked::relocate_data`), or route it through a `FrameStack` frame,
   including a `FrameStack::relocate()` of its segments
   (`Parked::relocate_data_through`).

Evidence for the implemented representation:

* `live_scalar_aggregate_and_places_survive_two_parks_and_two_moves` relocates
  the data frame at both parks, once directly and once through `FrameStack`.
  It checks that the place updates before and after each park and the inbox
  bytes written while parked are all observed. The scalar SSA value and the
  24-byte aggregate on the pinned stack also survive.
* The scheduler test relocates each fiber's data frame while its read is
  pending in the driver, where the driver owns the continuation.
* `stale_data_base_across_a_park_observes_relocation` is the negative
  control. The `stale_base` fixture deliberately keeps its entry base across
  a park. Without relocation it reads the right value. With relocation it
  reads `POISON`, because retired buffers are poisoned and kept until the
  fiber next stops. The invariant is therefore load-bearing, and a stale base
  is detected rather than silently reading freed memory.

## 7. Evidence identities

`EvidenceRegistry::fresh` hands out monotonic nonzero ids and never reuses
them. Each installation of a handler gets a fresh id, including a nested
installation of the same effect or a reinstallation at the same depth.
Generated code addresses slots by static lexical depth. The innermost
installation answers unqualified operations; an explicit outer depth models
named forwarding. Tests:
`nested_handlers_of_one_effect_have_distinct_identities` (inner park names the
inner identity, outer park names the outer one, a cross-identity resume is
rejected, and reinstallations get fresh ids), plus the evidence unit tests.
`prototype::Requirements::check` rejects a vector whose depths or effect tags
do not match what an entry will load, before any code runs.

## 8. Module and code lifetime

Machine code must outlive every stack that can return into it. Each fiber
holds an `Rc` share of the code owner, and `JITModule::free_memory` runs only
when the last share drops (`code_stays_mapped_until_the_last_fiber_is_gone`
drops the module handle while a fiber is parked, resumes it twice, and checks
the code is freed after exit). Evidence vectors and handler state are shared
the same way. For AOT objects the same rule applies at link level: the
object's code and the `vesper_fiber_park` runtime symbol (exported
`#[no_mangle]`) must stay mapped for the process lifetime.

`Fiber`, `Parked` and `NativeTask` are `!Send`, because they contain `Rc` and
raw pointers. Fibers run on the thread that created them, which matches the
single-threaded scheduler.

## 9. Adapter proposal for source lowering

This is a narrow, reviewable path that reuses the shared Place IR and the
existing native backend. The integration owner would make these changes in
`src/native.rs`; this milestone does not edit it.

1. **Classify handler operations.** Keep today's specialization for handlers
   that are lexically visible and tail-resumptive (§12.5 item 13). Inline fusion
   is unchanged. An operation of a linear external effect whose handler is
   *not* in the specialized activation (the `Runtime::begin_external` /
   `io_session` cases today) lowers to a park site.
2. **Entry wrapper.** A function that can reach a park site gets an ABI v1
   entry (`clif::entry_signature`). Its existing body is called from that
   entry with the context threaded as a hidden parameter, the "vmctx" of
   §12.5. Pinning it to `r15`/`x28` is an optimization this ABI does not
   require.
3. **Evidence.** Map each lexical handler identity (currently a `String` in
   `native.rs`'s `Handler`) to a static depth at compile time and to a fresh
   `EvidenceId` at installation time. Emit `clif::load_evidence(depth)` at the
   operation and pass its identity to `emit_park`. Install with
   `EvidenceVector::install`, and check `Requirements` before starting.
4. **Live values.** SSA values and stack-resident aggregates may stay where
   Cranelift puts them, because the stack is pinned. Places that the resumer
   writes while parked (`inout` DMA buffers per §12.3), and places whose
   storage must be movable, get offsets in the data frame. Place IR
   projections crossing a park are rebased on `ParkResult::data_base`.
   A lowering check should reject any CLIF value derived from an older
   `data_base` that is used after a park. That is a dataflow check over the
   park sites, and the `stale_base` fixture is its test case.
5. **Dispatch.** Lower each park result with `clif::branch_on_kind` into the
   operation's value continuation, the enclosing `Fail` edge (`Error`), and
   the cancellation edge (`Cancelled`). That edge runs the same
   obligation-discharge code as an abort (§12.2), which the checker already
   restricts to `Cancel` points.
6. **Runtime bridge.** Replace the evaluator continuation in `io_session` by
   `NativeTask` for native runs, mapping effect operations to `Step` the way
   `pulse.rs` maps `OP_YIELD`/`OP_READ`. The scheduler and driver need no
   changes.
7. **Validation.** Add differential tests against the reference evaluator.
   The evaluator serves as a test oracle only, never as a fallback. Keep
   running AArch64 object checks, and run the suite under QEMU or on
   hardware.

Proposed public surface for the adapter (already present):
`clif::{PARK_SYMBOL, entry_signature, park_signature, handler_signature,
declare_park, load_evidence, entry_data_base, emit_park, branch_on_kind,
emit_exit, emit_violation}`, `fiber::{Fiber, FiberConfig, Parked, Outcome,
Resumption, ContinuationTable, RawContinuation, vesper_fiber_park}`,
`evidence::{EvidenceRegistry, EvidenceVector, Installation}`,
`pulse::NativeTask`. The op-tag table in `pulse.rs` and the `prototype` module
are prototype-specific. A real adapter would replace them with a table
generated from effect declarations.

## 10. Limitations (not established by this prototype)

* Not source lowering. The entries are hand-written CLIF, and no Vesper
  program runs through this path yet.
* Linux only, on x86_64 and AArch64. AArch64 *execution* was validated under
  QEMU user-mode emulation, not on hardware. Under QEMU, `io_uring_setup`
  returns `ENOSYS`, so the AArch64 run used the fixture driver.
* No physical stack relocation (by design, see §6). There is no stack growth or
  segmented stacks, and the guard page is not a stack-overflow guarantee:
  generated code has no stack probes, and a frame larger than one page can
  skip the guard. Stack pooling, lazy commit, and the 64 KiB reservation
  sizing in §12.5 are not implemented. Each fiber `mmap`s its own stack.
* No claims about switch latency (the "~4–6 ns" in §12.5), zero allocation,
  or performance. By inspection, park and resume do not allocate inside
  `Fiber`, but starting a fiber boxes it and the adapter allocates I/O
  buffers. None of this was measured.
* No multi-threading, work stealing, or `Send` fibers. There is no nested
  fiber-local handler that captures its own sub-continuation.
* The FP environment (MXCSR, FPCR) is not part of the switch frame.
* The one-shot guarantee is enforced by Rust ownership and token checks in the
  runtime, not by a formal proof. Nothing here is checked in Lean.
* An abandoned continuation (dropped, then parked again during cancellation)
  loses its obligations. It is counted, not prevented.
* No deterministic simulation, telemetry, or DPOR integration.

## 11. Ambiguities and proposed decisions

| Ambiguity | Decision in v1 |
| --- | --- |
| §12.7 puts `scheduler_sp`/`scheduler_ip` in every evidence slot | One resumer per fiber, so it lives in the context. Slots carry identity, effect, and an optional direct handler. |
| §12.5 item 3 suggests suspended stacks can be relocated | Physical stacks are pinned; only the base-relative data frame moves (§6). |
| `FiberHeader` lists `saved_ip` | Omitted. The resume IP is on the pinned stack, so a separate field adds nothing. |
| Is cancellation of a parked *linear* operation allowed? | It is delivered as one `Cancelled` resume, never as a drop. Source typing decides which edges may abort. An in-flight kernel read is cancelled by the driver and arrives as `ECANCELED`. |
| What happens when a parked continuation is dropped | It is cancelled once. If it parks again it is abandoned and counted. |
| Register pinning of the evidence/vmctx pointer (`r15`/`x28`) | Not required. The context is an ordinary first parameter; pinning is a later optimization. |
| Which calling convention | The target default C convention, so the runtime can be ordinary Rust `extern "C"`. |
| Fiber identity reuse | Never reused (process-wide counter). Tokens add a generation. |
| How `Step::Complete` bytes encode a native result | `[status u8, result u64 LE, data frame image]` (`pulse::encode_exit`), adapter-local. |

## 12. Validation

Run from `crates/vesper-fiber-abi`:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                      # x86_64 host: 29 tests incl. compile-fail doctest
cargo test --release
# AArch64 execution under QEMU user-mode (needs gcc-aarch64-linux-gnu, qemu-user,
# rustup target aarch64-unknown-linux-gnu):
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="qemu-aarch64 -L /usr/aarch64-linux-gnu" \
cargo test --target aarch64-unknown-linux-gnu
cargo clippy --target aarch64-unknown-linux-gnu --all-targets -- -D warnings
```

From `experiments/native-pulse`: `cargo run --release -- --out target/np`.
That command schedules 4 native fibers and writes `pulse-x86_64.o`,
`pulse-aarch64.o` and their CLIF. Inspect the AArch64 object with
`aarch64-linux-gnu-objdump -dr target/np/pulse-aarch64.o`; the park sites
appear as `R_AARCH64_ADR_GOT_PAGE`/`LD64_GOT_LO12_NC` against
`vesper_fiber_park`.

Results on the development host (x86_64 Linux, 2026-11-12): all commands pass.
The kernel io_uring round trip ran (`io_uring kernel round trip validated`).
Under QEMU AArch64, all 29 tests pass, and the kernel test reports
`io_uring unavailable ... (os error 38); kernel round trip NOT validated`.
