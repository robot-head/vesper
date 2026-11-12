# Checked source to native one-shot suspension (wave three, lane 2)

Status: implemented on branch `claude/project-thread-kgl1yh`, PR against `main`
(not merged). Base: `1ffd0aec27dfcc24c5564585ae9068255490bf61` (descendant of
the required `02b3a1d175efb5d2561b7fa5b0aaf80121ae1bc6`).

**The branch needs the one-line coordinator dependency patch below to build.**
It was applied uncommitted for every check reported here.

## What now works

A checked Vesper entry function whose effect row declares linear external
effects compiles to a fiber ABI v2 entry and runs on `NativeRuntime` with real
machine code. Every bound operation lowers to `clif::emit_park` and
`clif::branch_on_kind` into typed value, error and cancellation edges. There
is no evaluator fallback; the evaluator is used only as a test oracle.

```rust
use vesper::native::session::*;
let session = NativeSession::compile(&world, "main", &[EffectBinding {
    effect: "Pulse".into(),
    ops: vec![
        OperationBinding::new("pause", Operation::Yield, "Ticked", "TickFailed",
            Cancellation::Constructor("TickCancelled".into())),
        OperationBinding::new("read", Operation::Read, "Got", "IoFailed",
            Cancellation::Constructor("IoCancelled".into())),
    ],
}])?;
let mut runtime = NativeRuntime::new(driver);
session.spawn(&mut runtime, Some(file))?;
let drained = runtime.drain(64, true)?;   // Err keeps every owner
session.decode_completed(&world, &drained.completed[0])?;
```

Source examples: `examples/native-pulse/ticket_tally.ves`, `lease_cleanup.ves`
and `typed_failures.ves`. `cargo run --example native-pulse -- <dir>` runs the
first with a kernel io_uring read and writes x86_64/AArch64 session objects.

## Public API (`vesper::native::session`, in `src/native_session.rs`)

* Bindings: `EffectBinding { effect, ops }`, `OperationBinding { op,
  operation, value, error, cancel }`, `Operation::{Yield, Read}`,
  `Cancellation::{Constructor(String), ErrorCode(u32)}`, `ECANCELED`, `MAX_READ`.
* `plan(world, entry, bindings) -> Result<Plan, String>`: validation only.
  `Plan::{entry, frame, result_type, failures, effects}`, `FrameLayout`.
* `object(world, entry, bindings, target) -> (bytes, clif)` for the host,
  `x86_64-*` or `aarch64-*`; `entry_symbol(entry)` (`vesper_session_<entry>`).
* `check_data_base_freshness(func, ctx, park)`: the lowering dataflow check.
* Linux x86_64/AArch64 only: `NativeSession::{compile, compile_with_options,
  plan, clif, evidence, check_evidence, fiber, fiber_with, spawn, decode_exit,
  decode_completed}` and `SessionResult::{Returned, Failed { instance, error },
  CancelledBeforeStart}`.

The module is declared from `src/native.rs` with `#[path = "native_session.rs"]
pub mod session;`, so `src/lib.rs` is unchanged. It is a child of `native` so
it can reuse the private `Codegen` without widening the backend's API.

## Supported slice and ABI

Validated before any machine code exists (`plan`):

* The entry has a body, no parameters, no generics, a concrete row and no
  closure-typed result. Each row instance is either an unlabeled `Fail[E]` or
  an unlabeled, non-generic `linear effect` with a binding. Every operation of
  a bound effect is bound exactly once. Unknown, duplicate, missing and extra
  bindings are errors.
* `Yield`: `fn op() -> R`, parks with `OP_YIELD`.
* `Read`: `fn op(inout buffer: B) -> R`. `B` is a *word buffer*: `u64`, `i64`,
  or a struct/tuple of word buffers, 8 to 4096 bytes. Enums, `bool`, `u8`,
  `u32`, `f64`, strings and functions are rejected because arbitrary bytes are
  not valid values of those types. This is a fixed-size buffer, not a general
  byte-slice API.
* `R` must be an enum. Value constructor: Yield takes no field or one unit
  field; Read takes one `u64` byte count. Error constructor: one `u32`/`u64`
  code. Cancellation: a distinct field-less constructor, or the error
  constructor with a fixed code.
* Recursion through effectful (inlined) functions is rejected. The backend
  inlines effectful calls, which is how every park site lands in one entry.
  Ordinary JIT reports the same shape as "exceeds 64 inline frames"; sessions
  reject it up front and do not overflow the compiler's stack.

Data frame: `[aggregate result | failure payload | read staging]`, each
8-aligned (`FrameLayout`). Evidence: one `Installation::parking(depth + 1)`
slot per bound effect, in row order, with a fresh installation (new
identities) per fiber. `check_evidence` rejects too few slots, wrong tags or
direct handlers.

Park protocol, unchanged ABI v2 records and `NativeTask` op table:

* Read: generated code copies the current buffer into staging through a base
  loaded after the previous park, then parks `OP_READ(len, staging_offset)`.
  The task delivers `n` bytes. On `Value(n)` the whole staging area is copied
  back through `ParkResult::data_base`, so the first `n` bytes are new and the
  rest keep their previous contents (POSIX `read` semantics). Error and
  cancellation leave the buffer untouched.
* Exit: `COMPLETE` with the scalar result word (f64 bit pattern), or with an
  aggregate written to the frame through a fresh base. An unhandled raise of
  row `Fail` instance `i` stores its payload in the frame and exits `FAILED`
  with result `i + 1`. The direct-fiber `Exit` decodes the payload, for
  example `DivisionByZero` versus `IndexOutOfBounds`. Through `NativeRuntime`
  only the `TaskRecord` survives (`status`, `result = i + 1`), because the
  adapter turns `FAILED` into `Step::Failed` and drops the frame image.
  `ArithOverflow` and `RuntimeError` are still distinct instances there.

## Decisions

* **Cancellation is a typed value.** A `Cancelled` resumption builds the
  binding's cancellation result, and the source program handles it like any
  other result. Cleanup is ordinary source code that consumes its linear
  values and may park again. Operations whose result type cannot express
  cancellation are rejected at binding time. No obligation is dropped by
  generated code.
* **Repeated cancellation is abortive.** A second `Cancelled` (explicit
  `Parked::cancel` at a cleanup park) produces the cancellation result again
  without doing the operation. Source still runs to its own exit. The runtime
  itself delivers only one cancellation per task, as before.
* **Fresh bases only.** Every data-frame access after a park derives from that
  park's `ParkResult::data_base` or from a reload of `ctx.data_base`. Each
  generated session entry is passed through `check_data_base_freshness`
  before definition. The check is conservative: a false positive rejects the
  program, and a stale use is never accepted.
* Physical stacks stay pinned. Stack-resident locals, aggregates and inout
  places survive parks in place. Only the data frame moves, and the tests
  relocate it at every park.
* Ordinary `jit`/`object`/`build` are unchanged. `compile` was split into
  `declare_instances`/`define_instances` with identical behavior, and the
  session path reuses them.
* Local captures only: fibers are `!Send`, there is no work stealing and no
  `Send` claim.

## Evidence (all with the coordinator patch applied)

Host: Linux 6.18.44 x86_64 (Intel Xeon @ 2.30GHz), rustc 1.97.0, Cranelift
0.125.4. AArch64: `qemu-aarch64` 8.2.2 user mode, `aarch64-linux-gnu-gcc`
13.3.0. Isolated `CARGO_TARGET_DIR`, `CARGO_PROFILE_*_DEBUG=0`,
`CARGO_INCREMENTAL=0`.

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -p vesper --all-targets -- -D warnings` | clean |
| `cargo test --test source_native_suspension` | 10 passed |
| `cargo test --test native_suspend_abi` | 10 passed |
| `cargo test -p vesper` (whole root package, incl. existing backend tests) | 394 passed, 0 failed |
| `cargo test --target aarch64-unknown-linux-gnu --test source_native_suspension --test native_suspend_abi` with `RUNNER="qemu-aarch64 -L /usr/aarch64-linux-gnu"` | 10 + 9 passed under QEMU (JIT on emulated AArch64), at the first commit; the review-fix commit adds one planning-only test not rerun under QEMU |
| `cargo run --example native-pulse -- <dir>` | kernel io_uring run completed; x86_64 (5208 B) and AArch64 (4048 B) objects written |

Acceptance coverage (`tests/source_native_suspension.rs`):

* `linear_ticket_aggregates_and_inout_places_agree_with_the_evaluator_across_parks`:
  a linear `Ticket` is held across three parks and consumed once. An
  aggregate inout `Tally` and the read `Block` are updated through an
  effectful helper. Four answer scripts (full and short reads, errors,
  cancellations), optimized and unoptimized, with and without data-frame
  relocation at every park, all agree with the evaluator.
* `closures_and_handler_clauses_park_and_agree_with_the_evaluator`: parks
  inside a closure, inside a lexical handler clause that resumes its
  continuation afterwards, and under a lexical `handle Pulse` that shadows the
  external binding.
* `typed_arithmetic_and_runtime_failures_stay_distinct_from_io_errors`:
  `ArithOverflow` (instance 0) and `DivisionByZero` (instance 1) exit
  `FAILED`; I/O errors and `ErrorCode(125)` cancellation stay source values.
* `cancellation_cleanup_reparks_and_repeated_cancellation_is_abortive`:
  cancellation during the read re-parks in cleanup (6010). A second
  cancellation there is abortive (6030), and cancellation before start runs
  no code. Results match the oracle.
* `runtime_cancellation_before_start_after_yield_and_during_io`, with a
  deterministic fixture driver. The driver log shows that the cancel command
  releases nothing. A shutdown with budget 4 returns `IncompleteShutdown`
  (`BudgetExhausted`) holding the live owner. Only the original completion
  (`ECANCELED`) resumes `Cancelled`.
* `cleanup_parks_complete_after_an_incomplete_budgeted_shutdown`: two budget
  exhaustions (in flight, then in the cleanup park), then a clean drain with
  2 parks recorded. A short read that won the race still takes the
  cancellation edge.
* `submission_failure_and_io_errors_resume_the_typed_error_edge_in_order`: a
  rejected submit returns the request and resumes `IoFailed(11)`. Two
  sessions finish in a deterministic driver order.
* `kernel_io_uring_read_resumes_a_source_session`: a real io_uring read
  completed on this x86_64 kernel and matched the oracle. Under QEMU,
  `io_uring_setup` returns ENOSYS; the test prints `io_uring UNAVAILABLE` and
  collects no kernel evidence there. No fixture result is reported as kernel
  evidence.

Negative coverage (`tests/native_suspend_abi.rs`): wrong, missing, duplicate,
extra and unknown bindings; Yield/Read kind mismatches; bad constructors.
Unsupported shapes: non-linear or generic effects, by-value and `sink` buffers,
non-word buffers, scalar results, entry parameters, suspending recursion, and
failure types containing function values (no decodable byte image). Locals
that shadow a world function name are not counted as recursive calls.
The same errors come back from `object`. Also covered:

* Duplicate (`UnknownToken`) and stale (`StaleToken`) resumes through
  `ContinuationTable`, and a wrong-evidence resume from another installation,
  which hands the owner back.
* Evidence vectors without the session slots are rejected before start.
* Freshness: a hand-built CLIF function using a pre-park base after a park is
  rejected, directly and through a block parameter; reloads from `ctx` or
  from the signal are accepted.
* Relocation between delivery and resume is observed.
* x86_64/AArch64 ELF objects export `vesper_session_main` and import
  `vesper_fiber_park`; RISC-V is refused.
* Ordinary `jit`/`object` still reject external entries.

## Coordinator patch

Root dependency (the only shared-file change; no module wiring is needed):

```diff
--- a/Cargo.toml
+++ b/Cargo.toml
@@ [dependencies]
 vesper-pulse = { path = "crates/vesper-pulse" }
+vesper-fiber-abi = { path = "crates/vesper-fiber-abi" }
--- a/Cargo.lock
+++ b/Cargo.lock
@@ name = "vesper" dependencies
  "vesper-core",
+ "vesper-fiber-abi",
  "vesper-lean-syntax",
```

There is no cycle: `vesper-fiber-abi` depends only on `vesper-pulse` and
Cranelift. No shared schema, ABI, IR, checker, runtime, Lean, CI or global
doc change was needed. Optionally, CI could add the QEMU AArch64 invocation
above for these two test files.

## Remaining gaps (not claimed)

* Only `Yield` and a fixed-size word-buffer `Read`. There are no writes, no
  general byte slices, no `sink`/owned DMA buffers moved through the request,
  and no reads from offsets other than the task's own advancing file offset.
* Entries take no parameters. Labelled or generic external effects,
  suspending recursion and handler-local fibers (nested sub-continuations)
  are rejected.
* Typed failure payloads are recoverable from direct `Exit` only. Through
  `NativeRuntime`, the adapter (`vesper-fiber-abi/src/pulse.rs`, not this
  lane) drops the data image of `FAILED` tasks and keeps the instance tag. A
  later adapter change could encode `FAILED` exits like completions.
* The op tags are `NativeTask`'s fixed `OP_YIELD`/`OP_READ`. Bindings map
  source operations onto them; there is no generated op table yet.
* AArch64 *execution* is JIT under QEMU user mode, not hardware. The AArch64
  session object was generated and inspected but not linked and run.
* Linux only. macOS direct fibers (lane 6) do not make sessions portable.
* Cancellation safety rests on the checked source handling its cancellation
  result, on Rust ownership and on runtime checks. There is no proof.
  Abandoned continuations (a parked fiber dropped instead of cancelled or
  drained) still lose their obligations, as documented in the ABI RFC.
* Drain budgets count scheduler ticks, not time.
