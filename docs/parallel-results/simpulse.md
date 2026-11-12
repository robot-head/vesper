# SimPulse deterministic kernel checkpoint

Implemented in `crates/vesper-simpulse`, against design §12.6. This is a standalone deterministic scheduling/completion model, not a replacement for the native execution engine.

## Delivered API

- `Simulator<T>` with private, monotonic, instance-scoped task/completion IDs and a non-Clone `RunToken<T>` that owns the continuation payload.
- Seeded SplitMix64 selection from sorted ready IDs, with per-round fairness for a fixed finite runnable set. No `T: Clone` bound or payload snapshots.
- Virtual nanosecond clock, timers, and delayed success/error/cancellation I/O injections. The clock jumps only when all work is parked, never while a token is running.
- Manual actual completions for externally driven I/O. Cancelling a request never releases its payload or overwrites its eventual result.
- Version 1 serde/JSON journals and exact transition replay, including seed-based choice validation. Replay prevalidates multi-event batches before advancing time or moving owners. Unsupported versions, unknown fields, invalid transitions, and unconsumed trace suffixes are rejected.
- Ownership-preserving `SpawnError<T>` and `TransitionError<T>`. Foreign tokens, duplicates, stale completion IDs, timer-as-I/O completion attempts, and time overflow leave the existing owner available.

## Integration contract

The current `scheduler::Resume` and `io_driver::Request<T>` APIs move owned continuation tokens. A future Pulse adapter should use `Simulator<Option<C>>` or a small continuation-state enum, take `C` from a checked-out token to execute one source/runtime step, and put the next continuation back before yielding or parking. Runtime completion payloads stay with their existing driver request; SimPulse should determine delivery order and virtual time, then transfer the corresponding owned completion exactly once. A cancellation acknowledgement CQE must never be translated into `complete_io` for the underlying request; only its actual I/O CQE may release it.

`WakeReason::Io` contains deterministic result metadata, not buffers or files. Place those resources in the owned continuation-state payload or retain them in the driver until actual completion, following the existing ledger ownership protocol. The simulator has no kernel pointers and performs no I/O itself.

For production embedding, a `RunToken` must be returned on every successful execution step. The model intentionally permits one running token at a time. It is not a multi-worker Pulse scheduler or a source-language interpreter, and Rust's ability to drop a token means it is not a formal proof of source linearity.

## Validation

Run in isolation before workspace registration:

```sh
cargo test --locked --manifest-path crates/vesper-simpulse/Cargo.toml
cargo fmt --manifest-path crates/vesper-simpulse/Cargo.toml --check
cargo clippy --locked --manifest-path crates/vesper-simpulse/Cargo.toml --all-targets -- -D warnings
```

The integration suite covers non-Clone owners and drop counts, seed repeatability and variation, JSON replay, finite-set round fairness, 24-hour virtual clock jumps, zero-time timers, pending-I/O quiescence, cancellation races, duplicate/stale completions, manual completion overriding an injection, foreign tokens, failure ownership recovery, strict journal parsing, replay batch atomicity, overflow, and simultaneous completions. README examples include a compile-fail check that `RunToken` cannot be cloned.

The crate has a temporary local `[workspace]` and lockfile so it can be tested while other worktrees change the root. During integration, remove that local `[workspace]` and `Cargo.lock`, register the crate in the root workspace, and resolve serde versions through the workspace lock. The crate-local `/target` ignore prevents isolated build artifacts from being committed.

## Remaining design work

Native/evaluator adapter integration; `vesper test --sim`; deterministic cancellation checkpoints in executed source; DPOR/backtracking with a defined state-equivalence model; simulated networks and storage crash consistency; telemetry and wait-for graphs. Seeded runs and event replay are implemented; they do not imply exhaustive schedule exploration or proof certificates.
