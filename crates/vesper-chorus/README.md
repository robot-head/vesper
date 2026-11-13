# vesper-chorus

Host library for design §25 (Vesper Chorus): choreographies, endpoint
projection, linear session channels, a Raft-style replication model under
SimPulse, and a software RDMA lease model.

- `parse_choreography` reads the textual global protocol (roles, messages
  `A -> B: Label(T);`, choices `A -> B { L(T) { .. } .. }`, local actions
  `A: act;`, `rec X { .. }`, `continue X;`, `end;`) and checks
  well-formedness (declared roles, no self-messages, unique labels, bound and
  guarded recursion, no unused roles, size and depth limits).
- `project` performs endpoint projection with full branch merging: a role
  not told about a choice must behave identically in every branch, or merge
  receptions from one peer into a labelled `Offer`. Unmergeable choreographies
  are rejected with the role, the choice, the branch path and the two
  conflicting behaviours. `check::explore` runs all projections together
  under asynchronous FIFO semantics and reports deadlocks, unexpected
  messages and orphans within a state budget.
- `session::Session<P>` is a typestate channel over `Send`, `Recv`, `Select`,
  `Offer`, `Act`, `Rec`/`Var` and `End`. `Session::bind` checks that the Rust
  protocol type equals the projection; every frame is checked again by a
  dynamic monitor on the receiving side. Transports: `MemoryNet` (threads)
  and `simnet::SimNet` (SimPulse-scheduled delivery with reordering and
  duplication).
- `raft::run_cluster` runs leader election and log replication on 5 or 7
  simulated nodes under SimPulse with seeded asymmetric partitions, loss,
  reordering, duplication, clock skew and crash/restart, checking election
  safety, log matching and committed-prefix agreement throughout.
- `rdma::RdmaFabric` models region registration and `InFlightRdmaWrite`
  leases that keep the source buffer immutable until the completion.

```rust
use vesper_chorus::{parse_choreography, project};

let chor = parse_choreography(
    "roles C, S;
     C -> S: Req(u64);
     S -> C { Ok(u64) { } Busy() { C -> S: Retry(); } }",
)
.unwrap();
assert_eq!(
    project(&chor, "C").unwrap().render(&chor),
    "S!Req(u64); offer S { Busy(): S!Retry(); end, Ok(u64): end }"
);
```

Out-of-order use of a session does not compile:

```compile_fail
use vesper_chorus::session::{End, Send, Session};
use vesper_chorus::{role, unit_message};
role!(B = "B");
unit_message!(Ping = "Ping");
fn misuse(s: Session<Send<B, Ping, End>>) {
    let _ = s.recv(); // a `Send` state has no `recv`
}
```

Nor does reusing a consumed state:

```compile_fail
use vesper_chorus::session::{End, Send, Session};
use vesper_chorus::{role, unit_message};
role!(B = "B");
unit_message!(Ping = "Ping");
fn twice(s: Session<Send<B, Ping, End>>) {
    let _a = s.send(&Ping);
    let _b = s.send(&Ping); // `s` was moved by the first send
}
```

Limits: Rust ownership plus run-time auditing stands in for the Vesper
source typing of `Session[P]`; it is not an implementation of those rules.
There is no `@choreography` macro, no Paxos, no real network and no real
RDMA NIC. See `docs/parallel-results/chorus.md`.
