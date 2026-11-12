# SimPulse deterministic kernel

This crate supplies the first execution-model-independent part of design §12.6: seeded task selection, a virtual clock, completion ownership, cancellation requests, and versioned event replay. It executes no Vesper instructions or host system calls.

```rust
use vesper_simpulse::{Simulator, WakeReason};

let mut sim = Simulator::new(42);
let task = sim.spawn(String::from("owned continuation state")).unwrap();
let token = sim.next_task().unwrap().unwrap();
assert_eq!(token.task(), task);
assert_eq!(token.reason(), WakeReason::Start);
sim.sleep(token, 86_400_000_000_000).unwrap();
let token = sim.next_task().unwrap().unwrap();
assert_eq!(sim.now_ns(), 86_400_000_000_000);
let owned = sim.finish(token).unwrap();
assert_eq!(owned, "owned continuation state");
assert!(sim.is_complete());
let json = sim.journal().to_json().unwrap();
```

A `RunToken<T>` moves the continuation out of the simulator without requiring `T: Clone`. Returning the token to `yield_task`, `sleep`, `park_io`, or `schedule_io` restores its owner to the simulator. `finish` transfers ownership to the caller. A failed transition returns the original token in `TransitionError<T>`; a rejected spawn returns its payload. Tokens are bound to their simulator instance, preventing accidental transfer between two simulations with identical logical task IDs.

```compile_fail
use vesper_simpulse::Simulator;
let mut sim = Simulator::new(0);
sim.spawn(()).unwrap();
let token = sim.next_task().unwrap().unwrap();
let duplicate = token.clone(); // RunToken deliberately does not implement Clone.
```

At most one token may be checked out at a time. A fixed finite runnable task set is scheduled once each per round; seeded SplitMix64 rejection sampling determines the order within a round. Newly arriving tasks can join that round. This is cooperative scheduling, not preemption, and continuous unbounded task creation has no bounded starvation guarantee.

The virtual clock starts at zero and advances only when no task can run and no token is checked out. It jumps directly to the earliest scheduled timer or I/O completion, delivering all events due at that timestamp in completion-ID order before selecting a runnable task. Zero-duration events do not advance time. `next_task() == None` can also mean quiescence while external I/O is pending; use `is_complete()` to distinguish a terminated run.

`park_io` retains its payload until `complete_io` receives the actual completion. `schedule_io` injects delayed success, failure, or cancellation outcomes. An explicit completion cancels any delayed injection for that ID. `request_cancel` only records a flag; it never manufactures a completion or releases a parked continuation. A successful I/O result may therefore win a cancellation race. Completion IDs are never reused within a simulator; stale, duplicate, and timer-as-I/O completions are rejected without changing ownership. IDs are instance-scoped logical addresses, not security capabilities.

To replay a run, construct `Simulator::replay(journal)`, execute the same embedding actions with new owned payloads, then call `finish_replay()`. Replay re-computes seeded scheduling choices and checks every committed event. Batch validation occurs before advancing time or moving payloads; a mismatch leaves the simulator intact. `finish_replay()` verifies consumption of the recorded prefix; separately check `is_complete()` if the journal is intended to describe a terminated program. Version 1 fixes the PRNG, task ordering, and event schema. JSON parsing rejects unsupported versions and unknown fields. Journals carry no payloads or pointers and provide reproducibility, not authentication.

The embedder must return every checked-out token. Rust can still drop a token, panic, or retain it forever; this API is not itself a proof of Vesper's linear exhaustion theorem. The language checker and eventual runtime adapter must enforce the stronger source-level protocol.

The crate does not yet implement production Pulse adapters, native stack switching, the `vesper test --sim` command, DPOR/backtracking, network partitions, WAL crash consistency, or telemetry. See the integration report in `docs/parallel-results/simpulse.md`.
