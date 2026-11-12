# Native lifecycle milestone: shutdown, drain and machine-state ownership

Lane 2 of the [next-wave prompts](../agent-prompts/next-wave/02-native-lifecycle.md).
Branch `claude/project-thread-k0gceb` (requested name `next/native-lifecycle`),
base `c2ba0b1` on `origin/main` (minimum baseline `0e6caf9`). The revised ABI
is in [native-pulse-abi.md](../rfcs/native-pulse-abi.md) (§2, §3, §5).

Edited paths only: `crates/vesper-fiber-abi/**`, `experiments/native-pulse/**`,
`docs/rfcs/native-pulse-abi.md`, this report. `vesper-pulse` (including
`scheduler.rs`), `src/native.rs`, root manifests and the lockfile are
untouched: the scheduler interface lane 6 reads is unchanged, and no new
dependency was added.

## What changed

1. **Owned cancellation and drain.** `Fiber::cancel` (before start, no code
   runs), `Parked::cancel` (one `Cancelled` resume; a re-park comes back as a
   live `Outcome::Parked`), `ContinuationTable::cancel_all` (once per fiber;
   re-parked owners stay under new tokens) and `into_parked`. In
   `pulse.rs`, `NativeRuntime<D>` wraps the unchanged scheduler with
   `cancel`, `drain(budget, wait)` and `shutdown(budget, wait)`. Success
   returns `Drained { driver, completed, failures, records }`; anything else
   returns `IncompleteShutdown { reason, runtime }` with every owner still
   inside (`BudgetExhausted`, `Driver`, `Retained`, `Stalled`, `Lost`).
2. **Adapter cancellation contract.** Before start, after yield and during
   I/O are distinct and recorded as `CancelPoint`. A cancellation
   acknowledgement never releases the request, file, buffer or continuation;
   the original completion does, and only then does the cancellation edge run.
   Each task gets at most one cancellation. Faults that used to drop the
   owner behind a `Step::Failed` string now retain the whole task in the
   runtime (`take_retained`, `NativeTask::into_owner`). Tasks made with
   `NativeTask::new` outside a runtime keep the v1 in-flight behaviour
   (`ECANCELED` on the error edge).
3. **Drop audit.** Drop is the documented fallback: at most one best-effort
   cancellation, then abandonment (counted, diagnostic only). Stack, context
   and data are freed only when nothing can run on or point into them, and
   before the code share. `Installation::direct` now forbids handlers from
   retaining stack addresses or re-entering their fiber. A dropped live task
   in a runtime is reported as `Lost`. No source linear-exhaustion claim is
   made from RAII, counters or `mem::forget`.
4. **ABI v2 machine state.** The switch frame now saves MXCSR and the x87
   control word on x86_64 (64-byte frame) and FPCR/FPSR on AArch64 (192
   bytes). New fibers start from the ABI default environment. `repr(C)`
   layouts and CLIF offsets are unchanged; `ABI_VERSION = 2` and the new
   `FLAG_FP_ENV_SAVED` mark the change. Register, alignment and unwind review
   is in RFC §3. `vesper_fiber_start` is now a hidden global: as a file-local
   asm label it failed to link in the AArch64 release build of the
   experiment. That bug is pre-existing; it reproduces on `c2ba0b1`.
5. **Pinned and thread-local.** Stacks stay pinned; no relocation, `Send` or
   work-stealing path was added (compile-fail doctest: `Parked: !Send`).
   Request IDs and per-request buffers are the existing `IoDriver`
   behaviour, unchanged.
6. **Experiment.** After the happy path, `native-pulse` runs a clean
   shutdown, a recoverable incomplete shutdown (budget exhausted, owners
   returned, second drain finishes), and a kernel pipe read cancelled during
   I/O when io_uring is available.

New fixture: `prototype::cancel_repark` (CLIF), whose cancellation edge
parks again for cleanup (`CLEANUP_DONE`, `CANCEL_TWICE_NOTE`). Objects now
carry seven park sites.

## Acceptance evidence

| Requirement | Test |
| --- | --- |
| Entry that re-parks after cancellation | `lifecycle::cancellation_edge_that_reparks_returns_the_live_owner` (both opt levels) |
| Drain budget returns an owner that later completes | `drain_budget_returns_reparked_owners_that_later_complete` |
| Repeated/stale resume rejection | `table_cancel_all_keeps_reparked_owners_and_rejects_stale_tokens`, existing `repeated_and_stale_raw_resumes_are_rejected` |
| Code/evidence lifetime across shutdown | `cancellation_before_start_after_yield_and_during_io_is_distinct`, `drain_budget_...`, `dropping_a_reparked_cancellation_is_counted_never_completed` |
| Submission and completion errors | `submission_and_completion_errors_keep_every_owner` |
| Completion delayed past the cancel acknowledgement | `cancellation_before_start_after_yield_and_during_io_is_distinct` (`DelayedDriver`: ack at once, completion three polls later; owners and code alive until then) |
| Faults keep owners; lost owners are reported | `faulted_tasks_are_retained_and_recoverable`, `a_driver_that_drops_requests_is_reported_as_lost` |
| Real kernel cancellation, or a reported skip | `kernel_cancellation_during_io_or_reported_unavailable` (idle pipe read, io_uring `AsyncCancel`) |
| FP control isolation across two fibers and the scheduler | `fp_env::fp_control_state_is_isolated_between_two_fibers_and_the_scheduler`: scheduler in round-toward-zero, fibers in up/down, hardware division fingerprints checked after every resume. It fails if the `ldmxcsr` restore is removed (checked). |
| Cross-target layout and objects | `objects.rs` for x86_64 and AArch64; compile-time offset asserts |

## Checks run (2026-11-12, x86_64 Linux cloud container)

* `cargo fmt --all -- --check`: pass.
* `cargo clippy --workspace --locked --all-targets -- -D warnings`: pass.
* `cargo test --workspace --locked`: 672 passed, 0 failed. Lean was not
  re-run, since no Lean or extraction code changed.
* `cargo test -p vesper-fiber-abi` and `--release`: 49 passed each (8 unit,
  1 FP, 12 lifecycle, 3 object, 14 round-trip, 8 scheduler, 3 doctests).
* Kernel: `io_uring kernel round trip validated` and `io_uring kernel
  cancellation during I/O validated` on this host.
* AArch64 under QEMU user-mode (`qemu-aarch64 -L /usr/aarch64-linux-gnu`):
  49 passed in debug and release; strict Clippy for the target passes; the
  experiment passes in debug and release. Both kernel tests report
  `io_uring unavailable (os error 38) ... NOT validated`. This is
  emulation, not AArch64 hardware.
* `cargo run -p native-pulse-experiment --release`: `round trip: OK`,
  `shutdown: OK`, kernel driver in use.

No switch-latency or allocation measurements were taken, and none are claimed.

## Integration steps for the coordinator

None required for the workspace: both packages are already registered, and
`Cargo.toml`/`Cargo.lock` are unchanged. Notes:

* `ABI_VERSION` is now 2. Nothing outside this crate reads it (checked by
  grep on the base).
* Lane 6 (`vesper-sim-session`) can keep using `vesper_pulse::scheduler`
  unchanged. If it wants native fibers under SimPulse later, `NativeRuntime`
  is generic over any `Driver<NativeTask>`.
* `docs/parallel-work.md` and the next-wave README can mark lane 2 delivered
  once reviewed.

## Source-lowering checklist (for the follow-up lane)

Prerequisites: checked arithmetic and this milestone merged.

1. Lower each linear-effect operation without a specialized handler to
   `clif::emit_park` with its static evidence depth, and branch with
   `branch_on_kind` into value, `Fail` and cancellation edges.
2. The cancellation edge runs the same obligation-discharge code as an
   abort at a `Cancel` point. It may park (cleanup I/O); it must not assume
   it runs to exit without parking, and must handle `Cancelled` again at
   that park (abortive cleanup), as `cancel_repark` does.
3. Rebase every Place IR projection that crosses a park on
   `ParkResult::data_base`, and add the dataflow check that rejects uses of
   an older base (`stale_base` is its negative test).
4. Source code must not change the floating-point environment except
   through a runtime helper. Each fiber's environment is saved per switch,
   and new fibers start from the default.
5. Map effect declarations to op tags and data-frame `inout` buffers,
   replacing `pulse.rs`'s prototype `OP_YIELD`/`OP_READ` table.
6. Run native programs under `NativeRuntime` and finish with `shutdown`.
   Treat any `IncompleteShutdown` as a failure that keeps owners, never as
   success.
7. Add differential tests against the reference evaluator (as an oracle
   only), including programs cancelled before start, after yield and during
   I/O. Keep AArch64 object checks and QEMU execution.

## Remaining limitations

* Still hand-written CLIF, not source lowering.
* AArch64 execution is QEMU only; QEMU has no io_uring.
* Abandonment on drop is counted, not prevented; only the explicit APIs are
  recoverable.
* Drain budgets count ticks; no deadlines or timers.
* No nested fiber-local handlers, stack growth, pooling or probes (as before).
* The one-shot and ownership guarantees rest on Rust ownership and runtime
  checks, not on a proof.
