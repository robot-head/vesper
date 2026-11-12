# Source simulation under SimPulse

Lane 6 of the [next-wave prompts](../agent-prompts/next-wave/06-source-simulation.md). New crate: `crates/vesper-sim-session`. Base: `origin/main` at `c2ba0b1`. Branch: `claude/project-thread-cqe9su`. The commit and PR are recorded in the PR description.

This crate runs checked Vesper source on the **reference CEK evaluator** under deterministic simulated effects. It is not native execution, kernel I/O, exhaustive scheduling or DPOR, and not a production Storage or Net implementation.

## What it does

1. `CheckedProgram::new(source)` parses and fully checks the source. Any diagnostic rejects it. The program owns the `World`; a `Session<'w>` borrows it, so every `Runtime<'w>` and `Suspension<'w>` lives inside a payload whose borrow outlives it. There is no self-referential or `unsafe` code.
2. A versioned `Scenario` (format `vesper-sim-scenario`, version 1, JSON, `deny_unknown_fields`) binds every operation of every effect installed by a task's entry to one action:
   - `clock` for `fn() -> u64`: resume at once with the virtual clock.
   - `yield` for `fn(..)`: `Simulator::yield_task`.
   - `sleep { duration_argument }` for a `u64` argument and `()` result: `Simulator::sleep`.
   - `cancel_requested` for `fn() -> bool`: resume at once with the task's cancellation flag.
   - `script`: the task's ordered script supplies the exact expected arguments, a delay, an outcome (`success`, `error` or `cancelled`), a typed response and optional inout writes. The adapter uses `Simulator::schedule_io`.
   - `external`: `Simulator::park_io`. The host calls `Session::complete_external` with a typed response.
   Each binding states the declared `params`/`result` (such as `"inout u64"` or `"Result[u64, u32]"`), and a binding that differs from the checked signature is rejected. Load also rejects unknown effects or operations, duplicate bindings, unbound operations of installed effects, non-linear or generic effects, incompatible actions, unsupported host types (functions, rows, unresolved generics), malformed or out-of-range values, writes to non-inout parameters, unknown fields and wrong versions. Hard bounds cover scenario size (1 MiB), tasks (64), bindings (256), script entries (4096), fuel, operations per task, total dispatches and virtual time. Nothing reads real sockets, files, clocks or randomness.
3. Inout aliases go through `Suspension::write_argument` before resume, both for scripted writes and for host-supplied writes. Every linear operation completes by resuming with a value checked against its signature. Cancellation (`cancel: { after_submissions: n }`) calls `Simulator::request_cancel` after the task's n-th parked submission. The request and its continuation stay parked until the scripted completion actually fires, and the source then resumes with the scripted value (for example `Err(125)`). A `cancelled` outcome without a prior cancellation request is a policy error.
4. A `Recording` holds an `ExecutionIdentity`, the unmodified SimPulse v1 `Journal`, a separate source-level `trace` (start, operation and arguments, resume with response/writes/outcome, cancellation, output) and the outputs. The identity has these fields:
   - `executable_sha256`: SHA-256 of `std::env::current_exe()`, which statically links the compiler, checker, evaluator and SimPulse. There is no package-version or caller-supplied label.
   - `semantics`: the adapter's mapping constant.
   - `source_sha256` of the exact checked bytes.
   - `scenario_sha256`, `bindings_sha256` (resolved checked signatures plus actions) and `options_sha256` (limits, fuel, optimize), all over canonical sorted-key JSON.
   - `seed`.
   `Session::replay` compares every identity field before spawning anything, then replays the journal with `Simulator::replay`. It also compares each source event before the transition it describes, and at the end checks journal consumption, trace consumption and outputs.
5. `Session::run(self, max_steps)` returns one of these results:
   - `Complete(Report)`.
   - `Budget(Session)`: every owner is retained; call `run` again. Slicing a run by budget gives the same report.
   - `Quiescent(Session)`: only external completions are pending.
   - `Interrupted { session, error }`: a pre-transition error such as a replay or trace mismatch, a script mismatch, a missing or extra script entry, an operation, dispatch or virtual-time limit, a policy or argument-write error, or an unexpected wake. The checked-out owner is kept and `take_stalled()` returns it as a `RecoveredOwner::NotStarted(Runtime)` or `Suspended(Suspension)`. A test resumes a recovered continuation by hand to completion. Parked owners stay inside the simulator.
   - `Terminal(TerminalFailure)`: `begin_external` or `resume` returned an `Err(String)` (interpreter fault or fuel exhaustion). The baseline API has already consumed that machine, so this is reported as a terminal failure with the trace so far. No owner is invented.
6. `fixtures/pipeline.ves` and `fixtures/pipeline.json` run two `worker` tasks. Each one holds a linear `Ticket` across a clock read, a virtual sleep, scripted `recv` (one success, one error), a yield, an inout `fill` and a cancellation check, then destructures the ticket once after its last resumption. Their I/O interleaves at virtual times 4, 8, 12 and 14. The fixtures use no `+`, `-` or `*`.

Runnable: `cargo run --example two_tasks` prints the trace and replays it. The `vesper-sim` binary does `record <source> <scenario> [recording.json]` and `replay <source> <scenario> <recording.json>`.

## Public API

`CheckedProgram`, `SourceRejected`. `Scenario` and its parts: `Limits`, `RuntimeOptions`, `Binding`, `Action`, `TaskSpec`, `ScriptEntry`, `Outcome`, `ArgumentWrite`, `CancelSpec`, `ScenarioError`. `Session::{record, replay, run, complete_external, pending, take_stalled, stalled_task, trace, journal, identity, now_ns}`. `RunResult` (with `finish`), `Report`, `Recording::{to_json, from_json}`, `SourceEvent`, `SessionError`, `TerminalFailure`, `StalledOwner`, `RecoveredOwner`, `PendingOperation`, `ExecutionIdentity`, `IdentityMismatch`, `SEMANTICS`, `executable_sha256()`, and `value::{decode, encode, type_name}`.

## Validation

Run in isolation on the x86_64 cloud host:

```sh
cd crates/vesper-sim-session
cargo fmt --check                                    # clean
cargo clippy --locked --all-targets -- -D warnings   # clean
cargo test --locked                                  # 2 unit + 15 integration + 1 CLI, all pass
cargo run --locked --example two_tasks               # prints trace, "replay reproduced the run"
```

The integration tests cover these cases:
- Two-task interleaving, virtual times and inout writes.
- Same inputs give the same report, replay reproduces it, and a recording round-trips through JSON.
- Seeds vary the schedule but not the results.
- Budget slicing matches an uninterrupted run.
- Dispatch and virtual-time limits keep owners.
- Reordered, changed, missing and extra script entries.
- Changed source, scenario and fuel, and forged executable, semantics, bindings and options digests.
- An altered `IoResult`, reordered completions, a truncated journal, a duplicated completion event, altered `Ok` and `Err` response data, a truncated trace and altered outputs.
- Cancellation retaining the parked `recv` until its completion at 55 ns, with replay. Cancellation without a request is rejected.
- Host completions: invalid shape, range and write, cancellation without a request, an unknown task, a duplicate completion, and replay requiring the same host response.
- Fuel exhaustion before and after a resumption is reported as terminal.
- Nested non-tail handlers plus inout produce the same result as direct reference evaluation, with and without the optimizer.
- 26 malformed-scenario rejections, unchecked source and non-linear effects.
- An operation limit.

Ordinary tests use no real sleeps, network or real clock. The root workspace suite was not rerun because this branch does not touch root code.

## Integration steps (coordinator)

1. Remove the `[workspace]` table and `crates/vesper-sim-session/Cargo.lock`. Add the crate to root `members` and `default-members`, then regenerate the root lockfile. The local lockfile already resolves to the root's versions (`serde 1.0.229`, `serde_json 1.0.154`, `sha2 0.10.9`), and the only new package is the crate itself.
2. The binary target is `vesper-sim` in its own package, so root `default-run = "vesper"` is unaffected.
3. After checked arithmetic merges, rerun this crate's tests. The fixtures avoid implicit wrapping arithmetic. Bump `SEMANTICS` if the evaluator's observable effect behavior changes. Recordings already stop replaying across rebuilds because the executable digest changes.

## Limits and follow-up requests

- **Owned runtime errors (coordinator API proposal).** Baseline `Runtime::begin_external(self, ..)` and `Suspension::resume(self, ..)` return `Err(String)` after consuming the machine. A later change could return `Result<Execution<'a>, RuntimeFailure<'a>>`, where `RuntimeFailure` carries the message plus, for fuel exhaustion, the still-valid `Suspension`/`Runtime` (refuel and continue), and for faults an explicit "consumed" marker. Until then, fuel and faults are terminal here.
- **Checked resume.** `Suspension::resume` silently coerces integers (`coerce_value` masks). This adapter validates every value against the signature first, but the runtime itself does not; a checked resume belongs in `src/runtime.rs`.
- **Parked owners after interruption.** SimPulse exposes no way to take payloads of parked tasks without a dispatch. After an interruption, only the checked-out owner is recoverable. The others are retained inside the session and dropped with it. A `Terminal` result drops the whole session, including other tasks' owners. Interrupted sessions do not continue.
- **Not in scenario v1:** generic effects (so `Fail[E]` rows cannot be entries), inout entry parameters, and function or closure host values. Virtual-time limits are detected right after SimPulse advances the clock, so that dispatch is interrupted with its owner retained.
- **Partial writes.** If one of several inout writes fails, the earlier writes have already been applied to the retained continuation.
- **Evaluator semantics.** Handler captures are snapshots (see `handler_captures_are_snapshots` in `tests/backends.rs`), and the nested fixture reflects that.
- **Strict replay identity.** A recording replays only under the same executable bytes. Each Cargo test binary has its own digest. Rust can still drop a session holding live continuations, so this is not a formal linearity proof.
