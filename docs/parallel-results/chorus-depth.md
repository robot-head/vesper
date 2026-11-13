# Wave six, lane 17: Chorus over Warp, n-ary choices and duality proofs (§25)

Claim: #259. Branch: `claude/project-thread-87av14`. Base: `11e0d9d` (main
with Chorus #151 merged; a descendant of the required baseline `cda092f`).

## What was built

All in `crates/vesper-chorus` (still `#![forbid(unsafe_code)]`) and
`lean/ChorusQuorum.lean`, `lean/ChorusQuorum/SessionDuality.lean`.

| Item from the prompt | Where | Status |
| --- | --- | --- |
| 1. `Session[P]` endpoints whose frames are QUIC stream frames on a Warp connection over the software NIC | `src/warpnet.rs`, `tests/warp_session.rs` | Done |
| 2. Typestate for n-ary choices and multi-peer fan-out | `src/session.rs` (`SelectN`, `OfferN`, `Choice`, `Broadcast`, `Gather`) | Done |
| 3. Lean: `raft_replicate_slot_session_dual` and a whole-system log-matching invariant | `lean/ChorusQuorum/SessionDuality.lean`, `lean/ChorusQuorum.lean`, `tests/lean.rs` | Done (about the models) |
| 4. Raft depth: snapshots and membership change in the checked simulation | `src/raft.rs`, `tests/raft_depth.rs` | Done |
| Acceptance: replicated log over Warp sessions on a lossy link | `src/replog.rs`, `tests/replog_warp.rs` | Done |

### Public API (all additive)

- `session::{SelectN, OfferN, BNil, BCons, Here, There, Pick, BranchList}`
  and the `branches![..]` type macro: an n-ary choice over a type-level
  branch list. `Session<SelectN<To, Bs>>::select(&msg)` picks the branch by
  the message's type (its index is inferred through `Pick`; a message that
  is not a branch does not compile, see the `compile_fail` doctests on
  `Pick`). `Session<OfferN<From, Bs>>::{offer, try_offer}` return a
  `Choice<Bs, E>`, a linear owner matched one branch at a time with
  `split() -> Case::This(msg, session) | Case::Other(rest)`;
  `Choice<BNil, _>::exhausted()` is a typed fault (unreachable for a bound
  session, since binding and the monitor check the same projection).
- `session::{Broadcast, Gather, RNil, RCons, RoleList}` and `roles![..]`:
  fan-out sends one message to each listed role in order; fan-in receives one
  from each (`gather() -> (Vec<M>, Session<Next>)`). They describe themselves
  as one `Send`/`Recv` per role, so `bind` checks them against the projection
  like everything else.
- `Transport::finish(channel, me)` (default no-op), called by
  `Session::close`; stream transports end their streams there.
- `NetFail::Malformed`: a stream transport could not decode peer bytes.
- `warpnet::{WarpNet, WarpNetConfig, WarpNetStats, WarpNetError,
  WarpTransport}`. `WarpNet::new(cfg)` builds `nodes` hosts (2..=16) joined
  pairwise by one Warp QUIC connection each: two `WarpEndpoint`s (client
  and server) on `SoftNic` UMEM rings, reliable RFC 9000 streams, RFC 9002
  recovery, BBRv3, AES-128-GCM 1-RTT packet protection. The wire between
  NICs is seeded: per-datagram loss, hold-back reordering, duplication, and
  `set_blocked(src, dst)` partitions. `transport(node, placement)` gives the
  `Transport` for one session endpoint (`placement[role] = host`).
  `step()`, `settle(max_steps)`, `stats()`, `undelivered()`,
  `open_streams()`, `describe_open_streams()`, `link_failure()`, and
  `inject_stream(src, dst, bytes)` (a peer that ignores the session API).
- `replog::{run_replicated_log, ReplogConfig, ReplogReport, ReplogStats,
  ReplogError}` and the typed protocols `CandidateProto`, `VoterProto`,
  `LeaderProto`, `FollowerProto` with their messages (`RequestVote`, `Grant`,
  `Deny`, `AppendEntries`, `Accept`, `Reject`, `Persist`).
- `raft`: `SNAPSHOT_CHOREOGRAPHY`, `CONFIG_FLAG`, `config_of`;
  `RaftConfig::{snapshot_threshold, initial_members, membership_per_mille,
  min_members}` (defaults keep the wave-five behaviour: no snapshots, fixed
  membership); `RaftStats::{compactions, snapshots_sent,
  snapshots_installed, config_changes, leader_step_downs}`;
  `RaftRunReport::members`; `Violation::CompactedEntryChanged`;
  `Checker::compacted_changed`.

Breaking-change note: `NetFail`, `Violation`, `RaftStats`, `RaftConfig` and
`RaftRunReport` gained a variant or fields. Nothing outside this crate uses
them on `main`; code that builds `RaftConfig` should use `RaftConfig::new`.

### Sessions over Warp: wire mapping

Each `(channel, from-role, to-role)` direction of a session is one
unidirectional QUIC stream opened by the sending host on its connection to
the receiving host. The stream starts with a 16-byte header (`VCS1`, channel
`u64be`, from `u16be`, to `u16be`) and carries records: `1` = frame (seq
`u64be`, label and payload-type names of at most 64 bytes each with a `u8`
length, body with a `u32be` length up to `max_body`), `2` = abandon.
`Session::close` sends FIN on the session's streams; dropping or faulting a
session (the existing `Transport::abandon` hook) sends the abandon record and
FIN, which the peer's endpoint observes as `NetFail::PeerGone`. QUIC gives
per-stream order and retransmission, so the endpoint's sequence-number
reordering is never exercised on this transport; the per-frame monitor and
`bind` check are exactly those of the in-memory channels.

Receive side, untrusted bytes: a bad magic, unknown record tag, oversized
name or body, invalid UTF-8 or a record truncated at FIN stops the stream
(`STOP_SENDING`, code `0x56430001`) and is counted; if the header named a
session direction, receives on it fail with `NetFail::Malformed`. A well
formed frame that is off the projection is a `ProtocolViolation` from the
monitor; a bad payload encoding is a `Decode` error. The QUIC connection
survives all of these (tested).

Warp endpoints are not `Send` (the handshake's `CidSource` is a boxed
trait object), while session transports must be. The network therefore runs
on one thread that owns every endpoint; `WarpNet` handles and transports
send it closures over a channel and wait for the answer. A single-threaded
driver is deterministic for a seed (`replicated_log_is_deterministic_per_seed`).
Blocking receives step the network once per wait and release it between
steps, so sessions on several threads interleave (the two-buyer test runs
each role on its own thread); they time out after `max_wait_steps` steps of
virtual time with `NetFail::Timeout`. Idle timeouts are disabled in the
endpoint configuration, because a blocked receiver drives virtual time
forward.

### Replicated log over Warp sessions

`run_replicated_log` scripts Raft terms on 3 to 7 hosts: the candidate of
term `t` is node `(t - 1) mod n` and asks every other node for a vote over a
typed `VOTE_CHOREOGRAPHY` session (one vote per term, the up-to-date rule);
a winner with a strict majority replicates for some rounds over typed
`APPEND_CHOREOGRAPHY` sessions (`AppendEntries`, follower `persist`, then the
n-ary `Accept | Reject` reply), appending proposals, backing off
`next_index` on rejection and committing entries of its own term on a
majority. Every term draws partitions (each directed link blocked with
probability 0.15); an RPC across a blocked link times out on both ends and
leaves through `Fault::into_error`. A final healing phase runs elections
without partitions until a leader brings every log to equality and full
commitment. The Raft `Checker` verifies election safety, log matching and
committed-prefix agreement after each change.

### Raft depth in the SimPulse cluster

- Snapshots: once `commit - snapshot_index` reaches `snapshot_threshold`, a
  node compacts its committed prefix. A leader whose follower needs entries
  at or below its snapshot point runs an InstallSnapshot exchange
  (`SNAPSHOT_CHOREOGRAPHY`: `InstallSnapshot(SnapshotImage)`, follower
  `persist`, `Installed(AppendAck)`), monitored by `RawSession` like the other
  RPCs. The follower checks that the image ends at the stated index and
  term, keeps its log if it already holds that entry, and otherwise replaces
  its log with the image. A crash keeps the snapshot (commit restarts at
  the snapshot point). The image is the compacted prefix itself: in this
  model the state machine is the command history, so compaction bounds what
  is sent incrementally but does not free memory. The checker reports any
  change at an index inside a node's snapshot.
- Membership: configurations are log entries (`CONFIG_FLAG | member_mask`).
  A node uses the newest configuration in its log, committed or not; a
  leader proposes adding or removing one server only after it has committed
  an entry of its current term and while no configuration entry is
  uncommitted (the single-server rule with the fix from Ongaro's thesis);
  quorums, vote counting, commit counting and replication targets follow the
  leader's configuration; a leader removed by a committed configuration
  steps down; non-members do not campaign. Spare servers (`initial_members
  < nodes`) can be added and catch up through AppendEntries or snapshots.

## Lean

Both files compile with Lean 4.24.0 (the GitHub release zip), contain no
`sorry`, `admit`, `native_decide` or `axiom`, and every listed theorem's
`#print axioms` shows only the standard axioms: `propext` for the duality
theorems (none for the three `decide` instances), and `propext`,
`Classical.choice`, `Quot.sound` for the system invariant (inherited from
`election_safety` and the existing append lemmas). `tests/lean.rs`
checks this when `VESPER_LEAN` is set and always scans the sources.

`lean/ChorusQuorum/SessionDuality.lean` (new, self-contained): multiparty
global types without recursion (messages, n-ary labelled choices, local
actions, end), binary session types, `dual`, and the binary projection
`proj a b g` (role `a`'s view of role `b`; a choice between other roles must
project identically in every branch, else undefined).

- `dual_dual`: duality is an involution.
- `proj_dual`: for any global type and distinct roles `a`, `b`, if `a`'s
  view of `b` is defined then `b`'s view of `a` is defined and is its exact
  dual; `proj_defined_symm`: one is defined iff the other is;
  `sessionDual_of_proj`.
- `raft_replicate_slot_session_dual`, in the form §25.4 states it
  (`SessionDual (projectRole leader raftReplicateSlotChor) (projectRole
  follower raftReplicateSlotChor) = true := by decide`), for the modelled
  `APPEND_CHOREOGRAPHY`; `raft_vote_session_dual` for RequestVote;
  `fan_out_slot_session_dual` for every leader/follower pair of a
  three-follower fan-out slot; `unmergeable_view_is_undefined` as a
  negative example.

`lean/ChorusQuorum.lean`, new section "Whole-system log matching": a state
holds every node's log and a ghost log per term and node (the node's log
right after it last appended an entry of that term as leader). Steps are
`leaderAppend t c` (`c` holds a strict majority in term `t`'s ballots, and
its log still extends its ghost for `t`) and `followerAppend` (the modelled
AppendEntries rule against any node's log).

- `system_log_matching`: in every reachable state, every pair of logs
  satisfies log matching.
- `system_entries_owned`: every entry of term `t` anywhere agrees, up to
  it, with the ghost log of a node holding a majority of `t`, and that node
  is unique (by `election_safety`).

The proof combines `election_safety`, `leader_append_preserves_matching`
and `append_yields_leader_prefix`. Its one modelling assumption is the
`leaderAppend` precondition (a leader of `t` has not lost its own entries
of `t` when it appends again: it only truncates after accepting a higher
term, after which it no longer leads `t`). These are theorems about the
Lean models, not about the Rust code; the Rust side is checked by the
seeded simulations, and `tests/lean.rs` also checks that the Rust projection
of the same two choreographies passes `are_dual`.

## Checks

Commands run from the repository root with an isolated target directory
(`CARGO_TARGET_DIR`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`), 4 CPUs:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -p vesper-chorus --locked --all-targets -- -D warnings` | clean |
| `VESPER_LEAN=…/lean-4.24.0-linux/bin/lean cargo test -p vesper-chorus --locked` | all pass (see below) |
| `lean ChorusQuorum.lean`, `lean ChorusQuorum/SessionDuality.lean` (in `lean/`) | pass, no warnings |
| `cargo clippy --workspace --locked --all-targets -- -D warnings` | WORKSPACE_CLIPPY |
| `cargo test --workspace --locked` (with `VESPER_LEAN`) | WORKSPACE_TEST |

Bazel is not installed in this cloud container; the crate's `BUILD.bazel`
already declares `//:lean` as test data (the glob covers the new
subdirectory) and the new dependencies are first-party path crates, which
the generated targets take from `cargo metadata`. CI runs `bazel test //...`.

New and changed tests (debug build, this container):

- `tests/warp_session.rs` (8 tests, about 9 s): the two-buyer protocol with
  each role on its own thread over a 10 % loss, 5 % reorder, 1 % duplicate
  Warp network, using `SelectN`/`OfferN` for the decision, for three
  seeds; a four-way choice in a loop (60 operations, checked against a
  model); fan-out/fan-in replication of 40 slots to three followers (240
  frames over 240 streams, with datagram loss and stream retransmission
  observed); bind rejection of a missing branch, a wrong continuation, an
  offer for a select, an empty choice, fan-out in the wrong order or to too
  few roles, and a follower type without `persist` (and acceptance of a
  reordered branch list); a dropped client observed as `PeerGone`; an
  unanswered receive timing out; seven malformed or off-protocol injected
  streams, each a typed error with the connection intact, and a bad-magic
  stream counted; configuration validation.
- `tests/replog_warp.rs` (3 tests, about 8 s): six seeded scenarios (3, 5
  and 7 hosts, 5 % loss, 5 % reordering, 1 % duplication, 6 partitioned
  terms): no checker violation, every log equal to the leader's and fully
  committed, no leaked or open sessions, every stream ended; across the
  seeds partitions caused RPC timeouts, logs were repaired through
  rejections, datagrams were lost and stream bytes retransmitted. A rerun
  of one seed reproduces the digest and every counter.
- `tests/raft_depth.rs` (5 tests, about 2 s for 400 + 400 seeds;
  `CHORUS_SIM_RUNS` overrides): with `snapshot_threshold = 6` on 5 nodes,
  400 seeds gave 3,300 compactions, 2,369 snapshots sent, 630 installed and
  6,391 committed entries with no violation; with 7 servers, 5 initial
  members, `membership_per_mille = 200` and snapshots, 400 seeds gave 1,362
  configuration changes and 162 leader step-downs, with node views that
  grew to 6 or 7 members and shrank to 3 or 4, and 7,214 committed entries,
  with no violation. Also the snapshot projection and configuration
  parsing.
- `tests/lean.rs`: now checks both Lean files (18 theorems audited) and the
  Rust `are_dual` on the modelled choreographies.
- `src/session.rs` doctests: two `compile_fail` cases (selecting a message
  that is not a branch; selecting on an offer) and one that compiles.
- The wave-five tests (`raft_sim.rs` with 2,000 + 1,000 seeds, `session.rs`,
  `projection.rs`, `rdma.rs`) pass unchanged.

## Root patch

`Cargo.lock` only: `vesper-chorus` now depends on the path crates
`vesper-warp`, `vesper-warp-crypto`, `vesper-warp-shard` and
`vesper-warp-umem` (read-only use; the last three are needed for the
`SoftNic`, `OneRttKeys` and `SlotHandle` types in `WarpEndpoint`'s API).
No new external dependency, no new workspace member, no `BUILD.bazel`
change. It is in its own commit.

## Decisions

- One unidirectional stream per session direction rather than one stream
  per host pair: closing and abandoning a session map onto FIN and the
  stream's own end, a peer's malformed bytes are confined to one session
  direction, and QUIC stream credit (MAX_STREAMS) is returned as sessions
  finish. Directions beyond the stream limit wait for credit.
- Keys come from fixed per-link secrets through
  `WarpEndpoint::open_connection` rather than Warp's TLS 1.3 handshake:
  certificates and the handshake are lane 5's area, and the session layer
  only needs an established connection. Packet protection is still real
  AES-GCM.
- The fan-out and fan-in combinators are sequential in role order, matching
  how the choreography is written. A quorum-style "first k of n" gather is
  not a session type (the remaining replies would be orphans); the Raft
  driver uses one binary session per follower instead.
- `Choice::split` walks the branch list with a `Case` enum, because Rust has
  no variadic enums; the list is typed, so each `This` arm gets the right
  message and continuation.
- Membership change is single-server, not joint consensus. The deliberately
  unsafe "two servers at once" variant was tried as a negative test, but
  2,000 seeds did not produce a violation, so it was not kept; the
  membership checks rest on the existing election-safety, log-matching and
  committed-prefix checkers.

## Remaining gaps

- No `@choreography` macro, no source typing rules for `Session[P]`: Rust
  ownership plus run-time auditing stands in for them, as before.
- The Warp transport runs only on the software NIC with fixed keys; no
  kernel AF_XDP, no TLS handshake, no 0-RTT. Lane 5's lifecycle work is not
  used.
- The Lean duality model has no recursion and only the plain merge rule
  (identical branch views); the Rust projection handles `rec` and merges
  receive branches. The log-matching model abstracts entries to their terms
  and assumes the leader-prefix precondition.
- The Raft simulation still keeps the full logical log in memory (the
  snapshot image is the compacted prefix); there is no durable storage.
- Performance: only the test run times above were measured; no throughput
  or allocation claims are made for the Warp session transport.
