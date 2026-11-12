# Checked-source WAL crash and network simulation

Wave-three lane 4 ([prompt](../agent-prompts/wave-three/04-crash-wal-network-simulation.md)). Base: `origin/main` at `1ffd0ae` (a descendant of the required `02b3a1d`). Branch: `claude/project-thread-bmfybq`. The commit and PR are in the PR description.

Everything is inside `crates/vesper-sim-session/**`. I did not create the optional `vesper-sim-models` crate. A new crate would need root registration and a lockfile change, and the model state is small enough to live in `vesper_sim_session::model`. It has no source or evaluator dependency, so it can move to its own crate later without API changes. The SimPulse kernel and journal, the compiler and runtime, native sessions and allocation are untouched. The root `Cargo.toml`/`Cargo.lock` and CI are unchanged, so **no coordinator patch is needed**.

This runs checked source on the **reference CEK evaluator** under deterministic simulated processes, storage and network. It is not native execution, DPOR or exhaustive scheduling. It does not check crash consistency of any real filesystem or kernel I/O path, and it proves no theorem about source behaviour.

## What it does

### Versions

| Item | Version |
| --- | --- |
| Scenario | `vesper-sim-scenario` version 1 is unchanged and must have no model. Version 2 requires a `model` section. Version 1 files serialize identically, because the new fields are skipped when empty. |
| Recording | Version 2 (`RECORDING_VERSION = 2`) adds `model_trace`, `durable` and `lost`. Version 1 recordings are rejected with an explicit error. They were already unreplayable, because the executable digest changes with every build. |
| Identity | `ExecutionIdentity` gains `model_semantics` = `MODEL_SEMANTICS` (`vesper-sim-models-v1(process-epochs-v1,wal-frame-v1,net-v1)`). `SEMANTICS` is bumped to `…+sim-session-v2`. Replay compares the executable, semantics, model semantics, source, scenario (which includes the whole model, faults and crash choices), bindings, options and seed before spawning anything. |
| SimPulse journal | The v1 journal is unchanged. Model timer tasks are ordinary SimPulse tasks, so their spawn, sleep and finish appear in it. |

### Model (`src/model/`)

**Processes (`process-epochs-v1`).** A scenario declares named processes. Each process has endpoints on named networks and a list of restart tasks. Every task names its process. A `crash` fault ends the current epoch of one process:
- Every unfinished task of that process becomes explicit **lost process state**: a `TaskLost` event with what it was doing (`not_started`, `ready`, or `parked` with effect, operation and request).
- A lost task is never resumed. When SimPulse next hands out its token, the session retires it with `Simulator::finish` and keeps the owner as a `LostOwner` (`Session::take_lost_owners`). Its linear values are lost, not destructured, and no `CancelRequested` or cancelled resume is produced. Tests check both.
- If SimPulse still holds an unscheduled wait (`park_io`: external, receive or flush), the session releases it with an `IoResult::Cancelled` completion so the kernel can retire the task. This is recorded as `WaitReleased`. The kernel has no kill transition, and the continuation is not resumed with that result.
- Volatile log records and the inbox vanish. Requests of the dead epoch become stale.

A `restart` fault starts the next epoch and spawns the restart tasks as `<name>@<epoch>`. Crash and restart must alternate at distinct times per process, and this is checked at load.

**WAL (`wal-frame-v1`, `model/wal.rs`).** Each log has three regions:
- `volatile`: appended and not yet flushed.
- One `in_flight` flush batch.
- The `durable` byte medium.

Frames are `[len u32 LE][lsn u64 LE][first 8 bytes of SHA-256(lsn ‖ payload)][payload]`. The payload is the canonical JSON of the checked source value.

Behaviour:
- **Append** is immediate and volatile.
- **Flush** moves all volatile records into the batch and parks the caller. A flush timer task makes the batch durable after `flush_delay_ns`, then returns `Ok(durable_lsn)`.
- **Crash** keeps exactly `persisted_records` of the in-flight batch, plus `torn_bytes` of the next frame. This is the explicit per-log `CrashChoice` in the fault. It is clamped to the data actually in flight, and both the requested and the applied values are recorded. All volatile records disappear.
- **Recovery** (`recover`, required once per epoch before any other log operation) keeps the longest prefix of complete frames with matching checks, UTF-8 payloads and LSNs 1, 2, 3, and so on. The first violation (`torn_header`, `torn_payload`, `bad_check`, `out_of_order` or `bad_payload`) and everything after it is reported and truncated. LSNs continue after the last valid record.
- A **failed flush** (injected, and delivered after the delay) makes nothing durable and returns the batch, oldest first, to volatile.
- A **second concurrent flush** on one log is rejected as unsupported. It is not queued.

**Network (`net-v1`, `model/net.rs`).** Messages carry the checked message type. Sends are immediate. Each message is in flight for its link delay (a default, with optional directional per-link delays) and arrives through its own timer task.

Drops:
- A partitioned link, or a destination process that is down, drops the message at send time. The sender still gets `Ok(message_id)`.
- The same checks are repeated at arrival time, so a partition after the send also drops it.

Otherwise the message goes to the oldest waiting receive, or to the bounded inbox. A receive returns a queued message at once. If none is queued it parks, with a timeout timer task that returns `Err(timeout_code)`. Other behaviour:
- An unknown destination returns `Err(unreachable_code)`.
- Injected send failures return `Err(code)`.
- Partition and heal are timed faults.
- A crash discards the endpoint's inbox and waiting receivers. Messages already in flight to it are not recalled.

### Source binding

There are new scenario actions, each checked against the source signature at load:

| Action | Source signature |
| --- | --- |
| `wal_recover {log}` | `fn() -> u64` |
| `wal_append {log}` | `fn(T) -> Result[u64, u32]` |
| `wal_flush {log}` | `fn() -> Result[u64, u32]` |
| `wal_read {log}` | `fn(u64) -> Option[T]` |
| `net_send {network}` | `fn(u64, T) -> Result[u64, u32]` |
| `net_recv {network}` | `fn(u64) -> Result[T, u32]` |

Load also checks the following:
- The payload type `T` of a log or network must be the same across all of its bindings.
- A task may use a log only if its process owns the log, and a network only if its process has an endpoint on it.
- Model actions require version 2.
- Fault times must be within `max_virtual_ns`.
- Crash choices may only name the crashed process's logs.
- Failures name a real target and kind.
- Hard bounds apply, along with `max_epochs` and `max_instances`.

### Stale completions and owner retention

Every parked operation has a session-unique `request` id and its epoch, both shown in `PendingOperation`. `Session::complete_request(request, …)` handles host completions:
- For a crashed epoch's request, it records `StaleRejected { source: host_completion }` and returns `Err(Box<RejectedCompletion>)` with the submitted outcome, response and writes handed back unchanged.
- Any other rejection also hands the response back.
- `complete_external(task, …)` on a lost task returns `TaskLost`.

Internal stale completions are also recorded as `StaleRejected`:
- A flush timer that fires after a crash (a generation mismatch).
- A receive timeout for a request that was already delivered or that crashed.

Budget, misuse and replay errors are checked on a copy of the model before anything changes. The session is then interrupted, and the requesting `Suspension` (or the internal timer token, `RecoveredOwner::Internal`) stays recoverable through `take_stalled`. The baseline limitation still applies: evaluator faults or fuel exhaustion inside `begin_external`/`resume` are `Terminal`. `TerminalFailure` now also carries the model trace.

### Trace and replay

Every fault, choice, failure and model transition is a typed `ModelEvent`, checked against the recording before it takes effect:
- Crash, task lost, wait released, inbox discarded, WAL crash (choice, applied counts, durable size and SHA-256), restart and recovery.
- Append, flush submitted, flushed (durable LSN and digest), flush failed and read.
- Injected failure, sent, unreachable, dropped, delivered, queued, taken, waiting, timed out, partitioned, healed and stale-rejected.

At completion, replay compares, in this order:
1. The source trace, then the model trace, both fully consumed.
2. The outputs.
3. The exact durable bytes of every log.
4. The lost-task list.

The first difference is an error.

### Budgets

Every bound is checked before data is retained or an owner changes:
- **Event budget:** `max_events` covers source plus model events.
- **Payload and log budgets:** per-record bytes, and log records and bytes across all regions.
- **Queue budgets:** in-flight messages, and the per-endpoint inbox, reserved at send time. A full queue is a budget error, never a silent drop.
- **Epoch and instance budgets:** crashes and task instances.
- **Virtual time and work:** virtual-time overflow, plus the existing virtual-time, dispatch (work) and fuel limits, which also apply to model timer tasks.

### Inspection

These accessors expose the model state:
- `Session::{model_snapshot, model_trace, lost, take_lost_owners}`.
- `Report::{lost, model}`.
- `ModelSnapshot`: per log, the durable size and digest, the records recovery would accept right now and any damage, plus the in-flight and volatile LSNs. Per network, the partitions, in-flight messages, inboxes and waiters. Per process, its epoch and whether it is up.

The CLI also prints lost tasks and durable log sizes and digests.

### Fixtures

`fixtures/kv.ves` is checked source with no implicit arithmetic. It has:
- A client and a retrying client.
- Four server variants: `serve_ack_first` (acknowledges before the flush), `serve_flush_first` (corrected), `serve_lazy` (acknowledges, then waits before flushing) and `serve_batch` (two records per flush).
- An `audit` restart task.

Servers hold a linear `Lease` across every suspension. `fixtures/kv_ack_first.json` crashes the unsafe server at 12 ns and restarts it at 20 ns.

## Acceptance evidence

All of this runs in `tests/models.rs` (11 tests) on real checked source.

1. **Counterexample.** With `serve_ack_first`, the client receives `Ack(1)` at 10 ns, and the crash at 12 ns finds that record in the in-flight flush. `audit@1` recovers 0 records. The run is identical under re-recording and replay, and the flush completion at 15 ns is recorded as a stale rejection.
   - A bounded crash-point search re-executes from scratch for every crash time from 0 to 30 ns, with no owner cloned. With the fixture seed, the unsafe server loses an acknowledged write exactly for crashes at 5–14 ns. The corrected `serve_flush_first` never does.
   - The 12 ns counterexample holds for seeds 0 to 5. With the corrected server, a crash at 17 ns, after the flush and while the ack is in flight, still has the client acknowledged and the audit recovering `{records: 1, first: 42}`.
2. **Recovery policy.**
   - Unflushed: `serve_lazy` crashes with `volatile_lost: 1`, and the audit recovers 0 records.
   - Torn header: persisted 1 plus torn 7 bytes recovers 1 record, discards 7 bytes as `torn_header` and truncates the medium to one frame.
   - Torn payload: torn 21 bytes is reported as `torn_payload`.
   - Fully persisted: persisted 2 recovers `{records: 2, first: 42, last: 43}`.
   - Clamping: an over-large choice is clamped and recorded.
3. **Replay of network faults and stale completions.**
   - Partitions: a partition at 2 ns drops the first Put at 5 ns, the heal comes at 50 ns, and the retry succeeds.
   - Delayed failures: an injected flush failure is delivered at 115 ns as `Err(5)`, and the record stays volatile.
   - Stale completions: the pre-crash flush completion, the receive timeouts, and the host completion for the crashed epoch (rejected with the response returned) are all recorded.
   - Each of these replays exactly. The stale host attempt must be repeated during replay, otherwise replay fails with `ModelTraceUnconsumed`.
4. **Replay rejection beyond scalar output.** Each of these is rejected:
   - An altered applied crash choice, restart epoch, crash-time durable digest or recovery damage gives `ModelTraceMismatch`, and the stalled owner is retained.
   - A truncated or extended model trace is rejected.
   - Altered final durable bytes, with every transition unchanged, give `DurableMismatch`.
   - An altered lost list gives `LostMismatch`.
   - An altered output gives `OutputMismatch`.
   - Changed `model_semantics` or a changed scenario crash choice is an identity mismatch.
5. **Budget slicing and owner retention.**
   - Running three model scenarios one dispatch at a time gives reports identical to uninterrupted runs.
   - Oversized records, a full log, oversized messages, the event budget and the virtual-time limit on a model timer task each interrupt with the owner retained and nothing retained in the model.
   - A crashed server's `LostOwner` is the `Suspension` parked on `flush`.
6. **Existing behaviour.** All 15 existing simulation tests, the CLI test and the identity rejections pass unchanged. A second CLI test records and replays the model fixture and rejects a changed crash time. 24 malformed-model load rejections are covered. Ordinary tests use no real filesystem, socket, clock, randomness or sleep.

## Validation

These were run on the x86_64 Linux cloud host (kernel 6.18, rustc 1.97.0) with `CARGO_TARGET_DIR=/home/claude/target-bmfybq` (isolated), `CARGO_INCREMENTAL=0` and debug info off:

```sh
cargo fmt --all -- --check                                          # clean
cargo clippy -p vesper-sim-session --locked --all-targets -- -D warnings  # clean
cargo test -p vesper-sim-session --locked
#   8 unit (WAL, network, value) + 2 CLI + 11 model + 15 simulation, all pass
cargo run --locked -p vesper-sim-session --example two_tasks        # "replay reproduced the run"
cargo run --locked -p vesper-sim-session --bin vesper-sim -- record fixtures/kv.ves fixtures/kv_ack_first.json rec.json
cargo run --locked -p vesper-sim-session --bin vesper-sim -- replay fixtures/kv.ves fixtures/kv_ack_first.json rec.json
#   client: 1 / audit@1: {"first":0,"last":0,"records":0} / lost: server ... / replay matched the recording
```

The full workspace suite was not rerun, because no root, compiler or other-crate file changed. The model tests take about 40 s in a debug build, mostly the 62 re-executions of the crash-point search.

## Coordinator notes

- **No root patch is needed.** There are no new crates or dependencies, and `Cargo.lock` is unchanged.
- **Suggested global-doc line** for `docs/parallel-work.md` or the wave-three README status, if wanted: "Crash/WAL/network models: integrated in `crates/vesper-sim-session` (scenario v2, recording v2, model semantics `vesper-sim-models-v1`); see `docs/parallel-results/crash-wal-network-simulation.md`."
- **Public API changes in this crate:**
  - `ExecutionIdentity` gained `model_semantics`.
  - `Recording` gained `model_trace`, `durable` and `lost`.
  - `Report` gained `lost` and `model`.
  - `TerminalFailure` gained `model_trace`.
  - `PendingOperation` gained `request` and `epoch`.
  - `RecoveredOwner` gained `Internal`.
  - `stalled_task()` now returns `Option<String>`.
  - `Scenario` gained `model`, and `TaskSpec` gained `process`.
  - `Action` gained six model variants.
  - There are new `SessionError` variants.
  - The existing record, replay, run, complete-external and take-stalled entry points are unchanged.

## Decisions

- **One SimPulse instance for all processes.** Per-process crashes stay inside one kernel, so stale pre-crash completions really happen and are rejected by epoch, generation or request id. Lost tasks drain at their next dispatch. The alternative, one kernel per epoch, would make every pre-crash completion vanish rather than be rejected.
- **Model timers are SimPulse tasks.** Arrivals, timeouts, flush completions and faults are SimPulse tasks rather than a side clock, because SimPulse has no clock-advance API and its kernel is read-only here. Their ordering against source tasks at equal virtual times is part of the seeded, journaled schedule. That is why the crash search reports results for the fixture seed.
- **Explicit crash choices.** They are clamped to real in-flight data and recorded as requested and applied, rather than chosen randomly.
- **Errors versus model outcomes.** Exhausted budgets and unsupported model states (append before recovery, concurrent flush) interrupt the session. Modelled faults (injected failures, timeouts, unreachable destinations) are typed `Err` values delivered to source.

## Remaining gaps

- There is no DPOR or exhaustive interleaving search. The crash-point search is a bounded sweep over crash times under one seed, run by re-execution. It is not a source exhaustion theorem.
- The WAL is a model with one medium per log and a simple frame and check policy. It does not model real filesystem crash consistency, fsync semantics, reordering below the frame level, or media corruption except through recovery's validation.
- There is one in-flight flush per log, and a concurrent flush is rejected as unsupported. Messages are typed datagrams with no connections, ordering guarantees beyond delay, duplication or reordering faults.
- Generic effects and model operations inside non-linear effects stay unsupported, as in version 1. Host-completed (`external`) operations can be stale-rejected, but scripted operations of lost tasks just drain.
- Evaluator faults and fuel exhaustion are still terminal for the whole session, because of the baseline owned-runtime-error limitation recorded in [source-simulation.md](source-simulation.md). Other tasks' owners are dropped with the session in that case.
- An interrupted session still does not continue. A SimPulse replay mismatch that occurs after a model step committed (for example while spawning its timer task) leaves that step visible in the interrupted session's snapshot.
