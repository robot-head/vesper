# Wave five, lane 2: Vesper Chorus (§25)

Claim: #122. Branch: `claude/project-thread-7383td`. Base: `84407eb` (main after the
wave-five plan, PR #137; descendant of `af03122`, which only added docs).

## What was built

New crate `crates/vesper-chorus` (depends only on `vesper-simpulse`, no
unsafe code: `#![forbid(unsafe_code)]`) and `lean/ChorusQuorum.lean`.

| Item from the prompt | Where | Status |
| --- | --- | --- |
| 1. Global choreography AST with textual form and well-formedness | `chor.rs`, `parse.rs` | Done |
| 2. §25.5 endpoint projection with full branch mergeability | `project.rs`, `check.rs` | Done |
| 3. Linear `Session<P>` over in-memory and SimPulse transports | `session.rs`, `simnet.rs` | Done |
| 4. Projected Raft-style replication on 5 and 7 nodes under SimPulse | `raft.rs` | Done |
| 5. Software RDMA model | `rdma.rs` | Done (software only) |
| 6. `lean/ChorusQuorum.lean` | `lean/ChorusQuorum.lean`, `tests/lean.rs` | Done |

### Public API

- `parse_choreography(&str) -> Result<Choreography, ParseError>`. Syntax:
  `roles A, B;`, `A -> B: Label(Type);`, `A -> B { L1(T) { .. } L2() { .. } }`
  (choice at `A`, told to `B`), `A: action;`, `rec X { .. }`, `continue X;`,
  `end;`. Statements after a choice or loop continue every branch that does
  not `continue`/`end`. Errors carry line and column.
- `Choreography::new(roles, GlobalChor)` checks: roles declared and unique,
  no self-messages, non-empty choices with unique labels, bound and guarded
  recursion (a `continue` must follow at least one message), no shadowed
  recursion variables, no unused roles, at most 64 roles, 100,000 nodes and
  tree depth 256. Every failure is a `WellFormedError` variant.
- `project(&chor, role) -> Result<LocalType, ProjectionError>`,
  `project_all`, `are_dual`, `LocalType::{render, alpha_eq, unfold}`.
- `check::explore(&chor, limits)` runs every projection together under
  asynchronous FIFO semantics, exploring all interleavings and choices up to
  a state budget, and reports `Deadlock`, `UnexpectedMessage`, `Orphans` or
  `StateBudgetExceeded`. `explore_locals` does the same for hand-written
  local types.
- `session::{Session, Send, Recv, Select, Offer, Branch, Act, Rec, Var, Z, S,
  End}` typestate channels; `Session::bind` checks the Rust type against the
  projection; `send`, `recv`/`try_recv`, `select_left`/`select_right`,
  `offer`/`try_offer`, `act`, `enter`/`repeat`/`pop`, `close`.
  `RawSession` is the untyped monitored owner used by the Raft model.
  `Monitor`, `LeakAuditor`, `Fault`, `MemoryNet`, `Transport`, and the
  `role!`, `unit_message!`, `u64_message!`, `string_message!` macros.
- `simnet::{SimNet, run_roles, SimRole, SimMode}`.
- `raft::{run_cluster, RaftConfig, QuorumRule, Checker, Violation,
  RaftAppendWireHeader, AppendAck, VoteReq, encode_batch, decode_batch,
  log_matching_violation, VOTE_CHOREOGRAPHY, APPEND_CHOREOGRAPHY}`.
- `rdma::{RdmaFabric, RdmaRegion, InFlightRdmaWrite, LocalKey,
  RemoteSlotGrant, Access, WorkCompletion, RecvCompletion, WcStatus,
  RdmaError}`.

## Decisions

- **Projection rules.** Sender gets `Send`/`Select`, receiver `Recv`/`Offer`,
  others merge. Merge is the "full merge": identical types merge; receptions
  from the same peer merge into one `Offer` with the union of labels
  (shared labels must agree on payload type and merge their continuations);
  types with an identical first action merge their continuations; anything
  else is `ProjectionError::Unmergeable` with role, decider, the informed
  role, the path of choices taken, both labels and both behaviours, for
  example: ``role `C` cannot tell whether `A` chose `Left` or `Right` (only
  `B` is told): it does `send Ping to B` in one branch and `end` in the
  other``. `rec X` projects to `End` for a role absent from a loop whose only
  exits are `X` or termination; otherwise the body is projected and merged
  normally, so a role absent from an inner loop that can jump to an outer one
  is rejected rather than wrongly ended.
- **n-ary choices.** The design's `Branch` is binary with one notified role.
  The AST generalises to n labelled branches (`Comm` with one branch is a
  message). The Rust typestate `Select`/`Offer` stays binary; a choreography
  with three or more labels can be used through `RawSession` and its monitor.
- **Typestate plus monitor.** Rust types make out-of-order use fail to
  compile (two `compile_fail` doctests in the README). `bind` rejects a
  protocol type that is not alpha-equivalent to the projection
  (`TypestateMismatch`, printed in local-type syntax). Every frame carries
  channel, sender, receiver, sequence number, label and payload type; the
  receiving monitor rejects anything outside the projection
  (`ProtocolViolation`), payloads that do not decode (`Decode`) and
  misrouted frames. Duplicates are discarded by sequence number and frames
  are reordered back into sequence.
- **Linear owners.** `Session`, `RawSession`, `Fault`, `RdmaRegion` and
  `InFlightRdmaWrite` are non-`Clone`, `#[must_use]`. Exits: `close` at
  `End`; `Fault::into_error` after a transport failure (the design's
  hand-off to a `NetFail` recovery handler); `RawSession::fault(reason)`;
  `RdmaFabric::deregister`, `await_wc`, `poll_wc`. A drop elsewhere is a
  counted leak in `LeakAuditor` (or the fabric), and a dropped session also
  marks the channel abandoned so a blocked peer gets `NetFail::PeerGone`
  instead of hanging. Failed consuming calls return the owner
  (`RawSession::close`, `RdmaFabric::deregister`, `try_recv`).
- **RPC choreographies for Raft.** Raft's leader fans out to a variable set
  of followers, which a fixed-role choreography does not express. Each RPC is
  one session instance of a two-role choreography, and both sides hold a
  `RawSession` monitor for their projected role:

  ```text
  roles Candidate, Voter;
  Candidate -> Voter: RequestVote(VoteReq);
  Voter -> Candidate { Grant(VoteResp) { } Deny(VoteResp) { } }

  roles Leader, Follower;
  Leader -> Follower: AppendEntries(AppendBatch);
  Follower: persist;
  Follower -> Leader { Accept(AppendAck) { } Reject(AppendAck) { } }
  ```

  Unanswered requests leave through `fault(Timeout)`; crashes fault every
  open session with `Crash`; the run ends by faulting the rest with
  `Shutdown`. Every run checks that nothing leaked and nothing stayed open.
- **Wire layouts.** `RaftAppendWireHeader` is the design's 40 bytes
  (8+4+4+8+8+8, big-endian, no padding), consistent with its fields.
  `AppendBatch` is the header, a `u32be` count (at most 256) and
  `count × (term u64be, cmd u64be)`. `AppendAck` is 21 bytes
  (4+8+8+1; flag must be 0 or 1). `VoteReq` is 28 bytes. All decoders reject
  wrong lengths, oversized counts and bad flags; arbitrary bytes are fuzzed
  in tests without panics.
- **Simulation.** One SimPulse simulator per run: one task per node, one
  network task and one chaos task. Nodes park on scheduled I/O completions
  (their next timer) and the network completes them early on delivery. From
  the seed: 1 to 3 crash/restart events (50 to 600 ms down; term, vote and
  log persist), 1 to 3 partition windows (100 to 800 ms) each blocking a
  random 15 to 60 percent of directed links (asymmetric), 2 percent loss,
  5 percent duplication, 0.5 to 15 ms latency (reordering), per-node clock
  rate error of up to ±15 percent applied to election and heartbeat
  timeouts. Election timeout 150 to 300 ms, heartbeat 50 ms, RPC timeout
  120 ms, 2 s of virtual time per run.
- **Checker.** On every leader election: at most one leader per term. On
  every log change: log matching between the changed log and every other
  (same index and term implies same entry and same prefix). On every commit
  advance: the committed prefix agrees with every previously committed
  entry. `QuorumRule::HalfFloor` (`n / 2`) is the deliberately buggy
  variant.
- **Depth limit.** Projection and the other recursive passes overflow a
  2 MiB test-thread stack in debug builds somewhere past about 300
  sequential steps, so the parser refuses trees deeper than 256 while it
  builds them (also keeping drops bounded) and brace nesting deeper than 64.
  Longer protocols should use `rec`.

## Evidence (exact commands and results)

Toolchain: cargo 1.97.0 locally; Lean 4.24.0 (`leanprover/lean4:v4.24.0`,
the pinned toolchain).

- `cargo test -p vesper-chorus` with `VESPER_LEAN` set: 42 tests plus 3
  doctests (two `compile_fail`), all pass.
  - `tests/projection.rs` (18): two-buyer projections match the textbook
    local types and run to completion in the explorer; the unmergeable
    third-party branch is rejected with the exact message above; an informed
    third party merges into an `Offer`; identical third-party behaviour
    needs no notification; nested choice paths are reported; payload
    mismatches on merged labels; recursion projects and runs (bounded
    queues reported); a role never told that a loop ended is rejected; a role
    outside a loop projects through it; a role absent from an inner loop
    that exits to an outer loop is rejected; local actions; binary duality;
    hand-written non-projected endpoints deadlock, see unexpected messages,
    or leave orphans; every well-formedness error; positioned parse errors;
    exponential-size, deeply nested, oversized and random inputs are typed
    errors without panics; a 250-step protocol projects and a deeper one is
    `TooDeep`.
  - `tests/session.rs` (7): two-buyer on three threads over `MemoryNet` for
    both outcomes (3 opened, 3 closed, 0 leaked); `bind` rejects mismatched
    protocol types; injected frames with the wrong label, an undecodable
    payload, an unknown offer label are rejected at the receiver; a dropped
    session is a counted leak with its state ("send Title(String) to S") and
    unblocks the peer with `PeerGone`; a dropped `Fault` and a dropped
    `RawSession` are leaks; a recursive stream protocol loops 100 times;
    two-buyer under SimPulse over 300 seeds with 30 percent duplication and
    random latency, each replayed from its journal with identical events;
    a SimPulse deadlock is reported with the blocked roles.
  - `tests/raft_sim.rs` (8): **2,000 seeds (0..2000) on 5 nodes and 1,000
    seeds (10000..11000) on 7 nodes**, every run reaching 2 s with no
    election-safety, log-matching or committed-prefix violation, no leaked
    or open sessions, and no rejected frames; the chaos counters are
    nonzero and the clusters commit entries (aggregate numbers below). The
    half-floor quorum is caught on both sizes within seeds 0..300 (5 nodes:
    first at seed 14 by committed-prefix divergence and seed 16 by election
    safety in term 1; 7 nodes: first at seed 5); seed 16 on 5 nodes is
    pinned. Runs replay bit-identically: same journal, digest and stats on
    rerun, via `Simulator::replay` of the journal and of its JSON round
    trip; a tampered journal is refused. Wire formats and the log-matching
    checker are unit-tested.
  - `tests/rdma.rs` (5): mutation and deregistration are refused while a
    write is in flight, the bytes land only on `progress`, the receiver gets
    the immediate; a stale local key is `UseAfterDeregister`; a write to a
    deregistered and re-registered remote slot completes with
    `RemoteInvalidKey` and leaves the new region untouched; bounds and
    access errors; dropped leases and regions are counted leaks (the source
    stays pinned); leases are bound to their fabric.
  - `tests/lean.rs` (2): source has no `sorry`, `admit`, `native_decide`,
    `axiom`, `partial` or `unsafe` outside comments; with `VESPER_LEAN` the
    file compiles, each of the 9 audited theorems depends only on `propext`,
    `Classical.choice` and `Quot.sound`, and the `#eval` majority table for
    0..64 nodes equals `QuorumRule::Majority`.
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: clean.
- `cargo test --workspace --locked` (with `VESPER_LEAN` set, Linux x86_64):
  exit 0, 298 test binaries, none failed. The Raft sweep takes about 10 s in
  a debug build; `CHORUS_SIM_RUNS` overrides the seed count.

## Lean: exactly what is proved

About the models in `lean/ChorusQuorum.lean` only, not about the Rust code:

- `raft_election_single_leader_per_term`: the §25.4 arithmetic statement.
- `majority_quorums_intersect`, `exists_common_voter`: two equal-length
  Boolean vote vectors that each have a strict majority set share a set
  index.
- `castVote_keeps_vote`: the modelled vote rule never changes a recorded
  vote. `election_safety`: in any one-term ballot vector (one candidate per
  voter), two candidates holding strict majorities are equal.
- `half_floor_quorums_can_be_disjoint`: two disjoint size-2 quorums exist for
  5 nodes.
- `append_yields_leader_prefix`, `follower_append_preserves_matching`: under
  the modelled AppendEntries rule, a follower accepting from a leader whose
  log matches every log ends with a prefix of the leader's log that matches
  every log. `leader_append_preserves_matching`: appending an entry of the
  leader's term preserves matching, given that every entry of that term
  already agrees with the leader's log (assumed, as election safety
  provides; not derived as a whole-system invariant).

The model's append rule truncates to `prevLogIndex` before appending; the
Rust follower truncates only at a conflicting entry, as Raft specifies.

## Not claimed

- Rust ownership plus run-time monitors and auditors stand in for the source
  typing of `Session[P]` and linear `InFlightRdmaWrite`; this is not an
  implementation of the Vesper source typing rules, Theorem 2 or the [T-Let]
  frame condition.
- No `@choreography` macro (Forge macro work is claimed by others), no code
  generation from projections, no `vesper test --sim` command.
- No Paxos, no real network, no real RDMA NIC, ibverbs, RoCEv2 or CXL. The
  RDMA fabric validates the modelled ownership protocol only. No DmaSlab or
  hugepage allocation.
- The Raft model is a checked simulation of election and replication, not a
  production consensus library: no snapshots, membership changes, WAL, or
  client sessions. The "<80us" projection time in §25.3 was not measured.
- No "0 Alloc" claim is made.

## Remaining gaps for later waves

- `@choreography` source macro and per-role code generation (needs Forge).
- Typestate for n-ary choices and multi-peer fan-out sessions.
- Lean extraction of projected roles and a session-duality proof
  (`raft_replicate_slot_session_dual`), and a whole-system log-matching
  invariant combining election safety with the append lemmas.
- Hardware RDMA backend behind a gated module; Warp transport for sessions.
