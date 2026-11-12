# RFC: Native Pulse continuation ABI v2 (prototype)

Status: prototype, not integrated. Revision 2 (native lifecycle milestone,
2026-11-12): ABI v2 switch frame with floating-point control state, explicit
owned cancellation/shutdown/drain, and the scheduler-adapter cancellation
contract. See `docs/parallel-results/native-lifecycle.md` for the
milestone report. Owner paths: `crates/vesper-fiber-abi/**`,
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
| Versioned `repr(C)` layouts and offsets | `src/abi.rs` | `ABI_VERSION = 2`, magic `VFIB`. Every offset used by codegen is a const checked against `offset_of!` at compile time. |
| Evidence identities and vectors | `src/evidence.rs` | Monotonic, never-reused `EvidenceId`s; O(1) static-depth slots. |
| Cranelift emission helpers | `src/clif.rs` | `declare_park`, `load_evidence`, `emit_park`, `branch_on_kind`, `emit_exit`, `emit_violation`. The only code that encodes ABI offsets into CLIF. |
| Pinned fiber stacks | `src/stack.rs` | `mmap` + `PROT_NONE` guard page, 256 KiB default. |
| Context switch | `src/switch.rs` | `global_asm!` trampolines for System V x86_64 and AAPCS64 Linux. |
| One-shot fibers | `src/fiber.rs` | `Fiber`, move-only `Parked`, `ContinuationTable`, `vesper_fiber_park`; `Fiber::cancel`, `Parked::cancel`, `ContinuationTable::cancel_all`. |
| FP environment | `src/fpenv.rs` | The callee-saved floating-point control state each switch preserves; test helpers. |
| Relocatable data frame | `src/data.rs` | Base-relative data, relocation with poisoned retired buffers, transfer through `vesper_pulse::frames::FrameStack`. |
| Scheduler adapter | `src/pulse.rs` | `NativeTask: vesper_pulse::scheduler::Resume`; `NativeRuntime` (owned cancel/drain/shutdown, `IncompleteShutdown`, retained faults). |
| Prototype entries | `src/prototype.rs` | Hand-built CLIF (not source lowering): `pulse_read`, `nested`, `cancel_repark` (cancellation edge parks again) and a deliberately invalid `stale_base` fixture. JIT for the host; objects for x86_64/AArch64. |
| Demo | `experiments/native-pulse` | N fibers on the Pulse scheduler (kernel io_uring when available), writes both objects and their CLIF, then a clean shutdown, a recoverable incomplete shutdown, and (with io_uring) a kernel read cancelled during I/O. |

The switch is real stack switching: generated code calls the imported symbol
`vesper_fiber_park`, which saves callee-saved registers on the fiber's own
stack and installs the scheduler's stack pointer. Resumption switches back and
the `call` in generated code returns. There is no interpreter, no VM, and no
evaluator fallback anywhere in the crate.

## 2. Layouts (ABI v2, 64-bit only)

ABI v2 keeps every v1 `repr(C)` layout and offset below byte for byte, so
generated CLIF and the emitted objects are unchanged. The version moved to 2
because the runtime-owned switch frame (§3) and the lifecycle contract (§5)
changed, and `vesper_fiber_park` rejects contexts of any other version.

All pointers are stored as `u64`. Little-endian is assumed by tests, not by
the layout.

`FiberContext` (`repr(C, align(64))`, 192 bytes): the `FiberHeader` of §12.7.

| Offset | Field | Writer | Meaning |
| --- | --- | --- | --- |
| 0 | `magic: u32` | runtime | `b"VFIB"` |
| 4 | `abi_version: u32` | runtime | `2` |
| 8 | `state: u32` | runtime | `Created 0`, `Running 1`, `Parked 2`, `Exited 3`, `Poisoned 4` |
| 12 | `flags: u32` | runtime | bit 0 `STACK_PINNED` (always set), bit 1 `FP_ENV_SAVED` (always set since v2) |
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

Switch frame (ABI v2), at the saved stack pointer, low to high:

| Target | Bytes | Contents |
| --- | --- | --- |
| x86_64 System V | 64 | `mxcsr u32`, `x87 control word u16`, 2 pad; `r15 r14 r13 r12 rbx rbp`; return address |
| AArch64 AAPCS64 | 192 | `x19–x28`, `x29`, `x30`, `d8–d15`, `fpcr u64 @160`, `fpsr u64 @168`, 16 pad |

Register review. These are exactly the target C ABI's callee-saved state:
System V preserves `rbx rbp r12–r15`, the MXCSR control bits and the x87
control word (no XMM register is callee-saved); AAPCS64 preserves `x19–x29`,
`sp`, the low 64 bits of `v8–v15`, and FPCR. Every context therefore owns its
floating-point environment: a fiber that changes rounding or FTZ does not
change the scheduler's, nor another fiber's. The switch saves the whole MXCSR
and FPSR, so sticky exception flags are context-local too (stricter than the
ABI). A new fiber starts from the ABI default environment (`fpenv::FpEnv::DEFAULT`:
round-to-nearest, exceptions masked, no FTZ/DAZ), not its creator's. Not
supported: live x87 stack contents across a switch (the ABI requires an empty
x87 stack at calls), AVX upper halves (caller-saved), SVE/SME state, and x18
as a reserved platform register (caller-saved on Linux).

Alignment. On x86_64 the initial frame is placed so that `rsp` is 16-byte
aligned at the `call` in `vesper_fiber_start` (checked with a debug
assertion); AArch64 `sp` is always 16-aligned. Unwinding: the trampolines
carry no CFI, nothing unwinds across a switch, and panics abort at the
`extern "C"` boundaries (entry, park, direct handlers). `vesper_fiber_start`
is a hidden global symbol; as a file-local asm label it failed to link when
Rust code referencing it landed in another codegen unit (observed in the
AArch64 release build of the experiment on the v1 base).

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
  unchanged on every rejection. Fiber identity exhaustion returns an error
  during creation; generation exhaustion rejects the next park as an ABI
  violation. Neither counter wraps to a previously valid token.
* `NativeTask` implements `vesper_pulse::scheduler::Resume` without changing
  the scheduler. Every queue transition moves the task. `Step::Io` moves the
  task's `File` and a fresh buffer into the driver; the completion moves them
  back. The existing one-shot completion ownership (`Resume::resume(self)`,
  submit-failure returns the request, poll errors keep harvested completions)
  is used unchanged.

## 5. Resumption, errors, cancellation and shutdown

* `Value(v)`: the operation produced `v`.
* `Error(code)`: the operation failed (errno for I/O). Generated code takes its
  error edge. The adapter maps `FAILED` to `Step::Failed`. A driver completion
  claiming more bytes than its buffer or the requested length is rejected as
  `EIO`, rather than reported as a successful partial copy.
* `Cancelled`: the fiber is resumed exactly once along its cancellation edge,
  where linear obligations are discharged; per §12.2 the source type system
  only permits abortive cancellation at `Cancel` points. A cancellation edge
  may itself park (to await cleanup). That is a normal park: the
  continuation stays live and owned.

### 5.1 Explicit owners (fiber level)

| API | Effect | Ownership on every path |
| --- | --- | --- |
| `Fiber::cancel(self) -> Exit` | Cancel before start. No generated code runs. | Returns `Exit { status: CANCELLED, parks: 0 }`; stack/context/code share released. |
| `Parked::cancel(self) -> Outcome` | One `Cancelled` resume of the current park. | `Exited` or `Parked` (re-parked at a new generation). Never rejected, never converted into completion. |
| `Parked::resume(self, r)` | One resume. | `Rejected { error, continuation }` hands the owner back. |
| `ContinuationTable::cancel_all(&mut self) -> TableCancel` | Cancels each owned continuation once. | Exits returned; re-parked owners stay in the table under new tokens; old tokens become `StaleToken`; a second call does not cancel cleanup parks again. |
| `ContinuationTable::into_parked(self)` | Hands every owner back. | No resume happens. |

### 5.2 Scheduler adapter (`pulse.rs`)

A task receives at most one cancellation. A later `Event::Cancelled` for a
task whose cancellation was already delivered is unwrapped and handled as
the interrupted event, so a repeated `Scheduler::cancel_all` cannot cancel a
cleanup park.

| Cancellation point | Behaviour | Record |
| --- | --- | --- |
| Before start (`Cancelled(Start)`, or any first event once the runtime scope is cancelled, including spawns after `cancel`) | `Fiber::cancel`; no code runs. | `CancelPoint::BeforeStart`, status `CANCELLED`, 0 parks |
| After yield (`Cancelled(Yield)`) | One `Cancelled` resume. | `AfterYield` |
| During I/O | See below. | `DuringIo` |

During I/O the driver owns the `Request`: the file, the request's buffer and
the task with its continuation. A cancellation *command* (`Driver::cancel_all`,
an io_uring `AsyncCancel` and its CQE, which `IoDriver` already separates by
`CANCEL_BIT`) releases none of them. Only the original request's completion
returns them. For a task in a cancelled `NativeRuntime` scope, that completion
(ECANCELED, a short read, or a full read that won the race) returns the file
to the task, drops the request buffer, and resumes `Cancelled`. Tasks built
with `NativeTask::new` outside a runtime keep the v1 contract: an in-flight
read cancelled by the driver arrives as `ECANCELED` (125) on the error edge.
A task whose queued event is wrapped by `Scheduler::cancel_all` takes its
cancellation edge after its file is recovered, in both modes.

If the cancellation edge parks again, the task is rescheduled normally (yield
or I/O) and driven to its real exit; it is never reported complete early.

### 5.3 Runtime shutdown (`NativeRuntime`)

`NativeRuntime<D>` wraps an unchanged `Scheduler<NativeTask, D>` and a scope
shared by its tasks:

* `cancel(&mut self) -> io::Result<()>` marks the scope and calls
  `Scheduler::cancel_all` (a driver error leaves everything owned and a
  later call retries it). Once that succeeds, later calls (including a
  retried `shutdown`) send no further driver cancel, so cleanup I/O issued
  by a cancellation edge is not cancelled; not-yet-cancelled tasks still get
  their one cancellation through the scope.
* `drain(self, budget, wait)` runs at most `budget` ticks. `Ok(Drained)`
  means every task exited: it returns the driver, the decoded completions,
  failure strings and one `TaskRecord { fiber_id, status, result, cancelled,
  parks }` per task. Otherwise `Err(Box<IncompleteShutdown>)` carries the
  runtime, with every pending owner, and a reason:
  `BudgetExhausted { pending }`, `Driver(io::Error)` (poll/cancel failure;
  harvested completions already queued), `Retained { fiber_ids }`,
  `Stalled { pending }` (scheduler idle but tasks unfinished, for example a
  driver whose `pending` undercounts) or `Lost { fiber_ids }` (a task was
  dropped while owning a live fiber, for example by a driver discarding a
  request; reported, never treated as success).
* `shutdown(self, budget, wait)` is `cancel` then `drain`.
* Faults that previously produced `Step::Failed` and dropped the owner (a
  rejected resume, `Start` delivered to a parked task, I/O delivered to an
  unstarted task) still report `Step::Failed`, but inside a runtime the whole
  task is retained. `take_retained()` returns `RetainedTask { reason, task }`
  and `NativeTask::into_owner()` yields `TaskOwner::{Created(Fiber),
  Parked(Parked)}` plus the file, to cancel or resume explicitly.
* Submission errors come back from the driver with the request and resume
  the error edge; nothing is lost.

### 5.4 Drop and unresolved owners

Drop is the unrecoverable fallback, not a protocol:

* A created `Fiber` drops without running.
* A dropped `Parked` gets at most one best-effort `Cancelled` resume. If it
  parks again it is abandoned: its stack, context and data are freed while
  nothing can run on or point into them, then the evidence and code shares
  are released (field order frees the stack before the code). Freeing is
  memory safe because the data frame and runtime hold no stack addresses and
  `Installation::direct` now requires handlers not to retain stack addresses
  past their call. `fiber::abandoned_fibers()` counts this per thread for
  diagnostics only.
* A dropped `NativeTask` that still owns a fiber is recorded as lost in its
  runtime (if any), then its fiber drops as above.
* A dropped `NativeRuntime` drops its queue and driver. `IoDriver`'s `Drop`
  cancels and waits for kernel completions, and leaks request allocations on
  an irrecoverable ring error rather than freeing memory the kernel may still
  write.
* `mem::forget` of any owner leaks its stack, code share and evidence share;
  nothing is freed while usable.

None of this establishes source-level linear exhaustion. Rust RAII, the
counter and `mem::forget` behaviour are runtime safety properties only; the
source checker remains responsible for obligation discharge.

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

`EvidenceRegistry::fresh` hands out monotonic nonzero ids from one
process-wide counter, so ids are never reused even across registries. Each installation of a handler gets a fresh id, including a nested
installation of the same effect or a reinstallation at the same depth.
Generated code addresses slots by static lexical depth. The innermost
installation answers unqualified operations; an explicit outer depth models
named forwarding. Tests:
`nested_handlers_of_one_effect_have_distinct_identities` (inner park names the
inner identity, outer park names the outer one, a cross-identity resume is
rejected, and reinstallations get fresh ids), plus the evidence unit tests.
Raw handler/state pairs can only be built with the `unsafe`
`Installation::direct`, whose contract covers state validity and lifetime;
safe code can only install parking handlers. `prototype::Requirements::check` rejects a vector whose depths or effect tags
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

`Fiber`, `Parked`, `NativeTask` and `NativeRuntime` are `!Send`, because they
contain `Rc` and raw pointers (a compile-fail doctest checks `Parked`).
Fibers run on the thread that created them, which matches the
single-threaded scheduler. Shutdown preserves code and evidence lifetime: a
pending owner returned by `IncompleteShutdown` keeps both alive even after
the `JitPrototype` handle is dropped, and they are freed only after the last
owner exits (`cancellation_before_start_after_yield_and_during_io_is_distinct`,
`drain_budget_returns_reparked_owners_that_later_complete`).

## 9. Adapter proposal for source lowering

This is a narrow, reviewable path that reuses the shared Place IR and the
existing native backend. The integration owner would make these changes in
`src/native.rs`; this milestone does not edit it.

1. **Classify handler operations.** Keep today's specialization for handlers
   that are lexically visible and tail-resumptive (§12.5 item 13). Inline fusion
   is unchanged. An operation of a linear external effect whose handler is
   *not* in the specialized activation (the `Runtime::begin_external` /
   `io_session` cases today) lowers to a park site.
2. **Entry wrapper.** A function that can reach a park site gets an ABI v2
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
`pulse::NativeTask`, and for lifecycle `Fiber::cancel`, `Parked::cancel`,
`ContinuationTable::{cancel_all, into_parked}`, `pulse::{NativeRuntime,
Drained, IncompleteShutdown, Incomplete, TaskRecord, CancelPoint,
RetainedTask, TaskOwner}`. The op-tag table in `pulse.rs` and the `prototype` module
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
* FP isolation is validated on the x86_64 host and on AArch64 under QEMU
  user-mode, not on AArch64 hardware.
* The one-shot guarantee is enforced by Rust ownership and token checks in the
  runtime, not by a formal proof. Nothing here is checked in Lean.
* An abandoned continuation (dropped, then parked again during its drop
  cancellation) loses its obligations. It is counted, not prevented; the
  explicit `cancel`/`drain` APIs are the recoverable path.
* `NativeRuntime` budgets count scheduler ticks, not time; there is no
  deadline or timer integration.
* No deterministic simulation, telemetry, or DPOR integration.

## 11. Ambiguities and proposed decisions

| Ambiguity | Decision in v1 |
| --- | --- |
| §12.7 puts `scheduler_sp`/`scheduler_ip` in every evidence slot | One resumer per fiber, so it lives in the context. Slots carry identity, effect, and an optional direct handler. |
| §12.5 item 3 suggests suspended stacks can be relocated | Physical stacks are pinned; only the base-relative data frame moves (§6). |
| `FiberHeader` lists `saved_ip` | Omitted. The resume IP is on the pinned stack, so a separate field adds nothing. |
| Is cancellation of a parked *linear* operation allowed? | It is delivered as one `Cancelled` resume, never as a drop. Source typing decides which edges may abort. An in-flight kernel read is cancelled by the driver and arrives as `ECANCELED`. |
| What happens when a parked continuation is dropped | Fallback only: it is cancelled once. If it parks again it is abandoned and counted. Explicit `Parked::cancel` / `NativeRuntime::drain` keep the re-parked owner. |
| Cancellation of a task whose I/O is in flight | The cancel acknowledgement releases nothing; the original completion returns the owners, then the task resumes `Cancelled` (runtime scope) or `Error(ECANCELED)` (unscoped v1 tasks). |
| Second cancellation of a re-parked cleanup | Not delivered by the adapter or `cancel_all`; `Parked::cancel` can still deliver one explicitly (the fixture's abortive edge). |
| Initial FP environment of a fiber | The ABI default, independent of the creating thread. |
| Register pinning of the evidence/vmctx pointer (`r15`/`x28`) | Not required. The context is an ordinary first parameter; pinning is a later optimization. |
| Which calling convention | The target default C convention, so the runtime can be ordinary Rust `extern "C"`. |
| Fiber identity reuse | Never reused (process-wide counter). Tokens add a generation. |
| How `Step::Complete` bytes encode a native result | `[status u8, result u64 LE, data frame image]` (`pulse::encode_exit`), adapter-local. |

## 12. Validation

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy -p vesper-fiber-abi -p native-pulse-experiment --all-targets -- -D warnings
cargo test -p vesper-fiber-abi            # 50 tests incl. three compile-fail doctests
cargo test -p vesper-fiber-abi --release
cargo run -p native-pulse-experiment --release -- --out target/np
# AArch64 execution under QEMU user-mode (needs gcc-aarch64-linux-gnu, qemu-user,
# rustup target aarch64-unknown-linux-gnu):
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="qemu-aarch64 -L /usr/aarch64-linux-gnu"
cargo test -p vesper-fiber-abi --target aarch64-unknown-linux-gnu
cargo test -p vesper-fiber-abi --release --target aarch64-unknown-linux-gnu
cargo clippy -p vesper-fiber-abi -p native-pulse-experiment --target aarch64-unknown-linux-gnu --all-targets -- -D warnings
cargo run -p native-pulse-experiment --release --target aarch64-unknown-linux-gnu -- --out target/np-a64
```

Inspect the AArch64 object with `aarch64-linux-gnu-objdump -dr
target/np/pulse-aarch64.o`; the park sites appear as
`R_AARCH64_ADR_GOT_PAGE`/`LD64_GOT_LO12_NC` against `vesper_fiber_park`
(seven sites: `pulse_read` 2, `nested` 2, `stale_base` 1, `cancel_repark` 2).

Results on the development host (x86_64 Linux cloud container, 2026-11-12):
all commands pass. The kernel io_uring round trip and the kernel
cancellation-during-I/O test both ran (`io_uring kernel round trip
validated`, `io_uring kernel cancellation during I/O validated`). Under QEMU
AArch64 all 50 tests pass in debug and release, and both kernel tests report
`io_uring unavailable ... (os error 38) ... NOT validated`.
