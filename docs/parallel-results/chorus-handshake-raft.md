# Wave seven, lane 16: Chorus over Warp TLS, recursive duality and a durable Raft log (§25)

Claim: #339. Branch: `claude/project-thread-70fp6w`. Base: `283c811` (main
with the wave-seven plan #354 merged; a descendant of the required baseline
`c025b9a`).

## What was built

All in `crates/vesper-chorus` (still `#![forbid(unsafe_code)]`, no new
dependencies) and `lean/ChorusQuorum/RecursiveDuality.lean`.

| Item from the prompt | Where | Status |
| --- | --- | --- |
| 1. Chorus sessions over Warp connections with the real TLS handshake and lifecycle (no fixed keys) | `src/warpnet.rs`, `tests/warp_tls.rs`, `tests/fixtures/pki` | Done |
| 2. Lean duality model with recursion and the receive-branch merge rule | `lean/ChorusQuorum/RecursiveDuality.lean`, `tests/lean.rs` | Done (about the model) |
| 3. Durable Raft log with snapshots and log truncation on disk, recovered by a crash test | `src/durable.rs`, `src/raft.rs`, `tests/durable_log.rs` | Done |
| Acceptance: session tests over the software NIC with TLS; the Lean check; crash-recovery tests | as above | Done; RDMA stays hardware-deferred |

### 1. Sessions over Warp TLS

`WarpNet::new(cfg, &WarpTls)` now opens every host pair's QUIC connection
with `WarpEndpoint::connect`/`listen`, that is, the wave-five/six TLS 1.3
handshake (RFC 9001 over rustls 0.23.46 with aws-lc-rs) inside the Warp
endpoint, over the same seeded lossy wire the sessions use. The fixed
per-link secrets and `open_connection` are gone; 1-RTT keys come only from
the handshake.

- PKI: `WarpTls { roots, hosts: Vec<HostCredential { name, chain, key }>,
  time }`. The lower-numbered host of a pair is the QUIC client. The client
  verifies the server's chain against the roots and the server host's name
  (rustls's WebPKI server verifier). Authentication is mutual: the client
  presents its own certificate, and the server checks it with rustls's
  `WebPkiClientVerifier` plus a name check (`HostClientVerifier`: the end
  entity must be valid for the client host's DNS name, via
  `rustls::client::verify_server_name`). ALPN is `vesper-chorus/1`. `time`
  fixes certificate validity checks for reproducible runs.
- A failed or unfinished handshake is `WarpNetError::Handshake { client,
  server, reason }` (bounded by `WarpNetConfig::handshake_steps`); PKI
  problems are `WarpNetError::Tls` or `Config`.
- Lifecycle after the handshake: `key_update(a, b)` runs an RFC 9001 §6 key
  update from host `a` on live session traffic; `close_link(a, b, code)`
  sends an application CONNECTION_CLOSE. A failed link keeps carrying
  datagrams so the close reaches the peer, and sessions on it fail with
  `NetFail::Closed` on both hosts (not a timeout).
- `link_info(a, b) -> LinkInfo`: handshake phase, ALPN, the peer's end
  entity certificate, cipher, key generations, handshake steps and errors,
  as each endpoint reports them. `WarpNetStats` gained `handshakes`,
  `max_handshake_steps`, `key_updates` and `authentication_failures`.
- Test PKI: `tests/fixtures/pki` (`gen.sh`, OpenSSL 3, ECDSA P-256, valid
  2026-11-14 to 2126-10-21): one CA, `node0..node15.chorus.test` with
  serverAuth and clientAuth usage, and a rogue CA with a rogue `node1`.

Breaking-change note: `WarpNet::new` and `replog::run_replicated_log` take a
`&WarpTls`; `WarpNetConfig`, `WarpNetStats` and `WarpNetError` gained fields
or variants. Nothing outside this crate uses them on `main`.

### 2. Lean: recursion and branch merging

`lean/ChorusQuorum/RecursiveDuality.lean` (new, self-contained; the
wave-six `SessionDuality.lean` is unchanged) extends the global types with
`loop x g`/`var x` and the binary session types with `loop`/`var`, and
projects with the rules of `src/project.rs`:

- a loop whose body has no interaction of the pair and whose only free
  variable is its own projects to `stop` (Rust: a role absent from a loop
  that can only repeat or end); a body view that is just `var x` is `stop`;
  an unused binder is dropped; otherwise the view is `loop x t`;
- a choice between other roles merges the branch views with `merge`: two
  receptions from the peer become one `off` with the union of their labels,
  merging the continuations of shared labels (the Rust receive-branch
  merge), and, dually, differing sends become one `sel`. A binary view needs
  the send rule because the peer that learns the choice learns it from a
  third role, whose message the pair view erases. Identical `loop`, `var`
  and `stop` heads merge; everything else is undefined.

Proved (no `sorry`, `admit`, `native_decide` or `axiom`; `#print axioms`
shows `propext` at most):

- `dual_dual`, `loopView_dual`;
- `merge_dual`: `merge (dual x) (dual y) = (merge x y).map dual`;
- `proj_dual`: for distinct `a`, `b`, `proj b a g = (proj a b g).map dual`
  for every global type, so each view is defined exactly when the other is
  and they are duals (`proj_defined_symm`, `sessionDual_of_proj`);
- by `decide`: `raft_heartbeat_loop_projection` and
  `raft_heartbeat_loop_session_dual` (a recursive AppendEntries/Stop
  loop), `commit_notify_client_view` and `commit_notify_session_dual` (the
  client's view of the replica is the receive merge `off {committed,
  aborted}`, the replica's is the dual `sel`), `streaming_loop_session_dual`
  (the same inside a loop), `absent_loop_projects_to_stop`, and
  `send_receive_conflict_is_undefined`.

These are theorems about the Lean model, not about the Rust code.
`tests/lean.rs` runs Lean on the file when `VESPER_LEAN` is set, audits the
axioms of all 13 theorems, and checks the Rust projection on the same
choreographies: the heartbeat loop projects to dual `Rec` types, the
client's type is an `Offer` from the replica with both labels (in a loop
too), a loop the client is absent from adds nothing to its type, and the
send/receive conflict is `ProjectionError::Unmergeable`. Remaining model
differences: binders compare by name (Rust also merges loops up to
renaming), payload types are not modelled, merged labels keep first-seen
order (Rust sorts them), and the model folds merges from the right where
Rust folds from the left.

### 3. The durable Raft log

`durable::DurableLog<D: Disk>` keeps one server's `PersistentState` (term,
vote, snapshot index/term/image, and the log after the snapshot) in three
files:

- `meta` (term, vote, CRC-32C) and `snapshot` (point, image, CRC-32C) are
  replaced atomically: write `*.tmp`, sync it, rename, sync the directory.
- `log` is a header (base index, CRC) and one record per entry (length,
  CRC-32C, index, term, command). Appends go to the end; a conflicting
  suffix is removed by truncating the file at its first record; compaction
  and InstallSnapshot write the snapshot first and then a new log that is
  renamed over the old one. `sync()` makes appends and truncations durable.
- Recovery (`open`) drops a torn tail (a partial record or a bad CRC at the
  end), keeps the log suffix only if it agrees with the snapshot at its
  point (by term, as Log Matching allows), and rewrites the log when it
  dropped anything. A damaged record followed by a valid one, a bad header,
  meta or snapshot, or a record with the wrong index is
  `StorageError::Corrupt`; calls that would break invariants (a term going
  down, a second vote in a term, truncating inside the snapshot, moving the
  snapshot back) are `StorageError::Invalid`.
- `persist(&target)` brings the disk to a target state with the fewest
  writes (meta, snapshot, truncation, appends) and syncs;
  `persist_unsynced` stops before the log sync.
- Disks: `FsDisk` (real files, `sync_all`, directory sync on Unix; Windows
  has no directory handle in std) and `SimDisk`, an in-memory model whose
  `crash()` keeps each file's synced bytes plus a random prefix of later
  appends and up to 15 garbage bytes, keeps or drops a truncation since the
  last sync, and keeps a prefix of the directory operations since the last
  directory sync; `power_fail_after(n)` makes every mutating operation fail
  after `n` more.

The SimPulse Raft cluster uses it when `RaftConfig::durable` is set: after
every node step the node's persistent state is persisted and synced before
the step's messages are routed, a crashed node restarts from what its disk
recovers (checked equal to the last synced state, else
`Violation::RecoveryLostState`), and `torn_crash_per_mille` makes the power
fail during a persist before the log sync: the step's messages are dropped,
the node recovers at once, and the recovered state must lie between the
last synced and the target state (else `Violation::RecoveryInconsistent`).
The disk model has its own random stream, so a durable run without torn
crashes schedules exactly like an in-memory one (tested by digest).
`RaftConfig::reply_before_sync` is a deliberately unsafe variant for a
negative test. `RaftStats` gained `persists`, `disk_syncs`, `recoveries`,
`torn_crashes`, `torn_entries_lost`, `torn_bytes_dropped`,
`disk_truncations` and `disk_snapshots`.

The image is still the compacted prefix itself (the modelled state machine
is the command history), so compaction bounds the log file and what is
replicated incrementally, not the total bytes on disk.

## Checks

Commands from the repository root, `CARGO_TARGET_DIR` outside the checkout,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`,
`CARGO_INCREMENTAL=0`, 4 CPUs, Lean 4.24.0 from the GitHub release zip:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo +1.100.0 clippy -p vesper-chorus --locked --all-targets -- -D warnings` | clean |
| `VESPER_LEAN=… cargo test -p vesper-chorus --locked` | all pass (88 tests plus doctests, 1 ignored child) |
| `lean ChorusQuorum/RecursiveDuality.lean` (in `lean/`) | passes, no warnings |
| `VESPER_LEAN=… cargo test --workspace --locked --no-fail-fast` | 4,014 passed, 17 failed in 544 binaries; every failure is environmental and outside this crate: `vesper-loom-build` `tests/build.rs` and `tests/cli.rs` (no build identity in this container) and `vesper` `tests/lean4lean42_acceptance.rs` (needs `VESPER_LEAN4LEAN42_ROOT`) |
| `python3 tools/bazel/sync_build_files.py` | no changes |

Bazel is not installed in this cloud container. The crate's `BUILD.bazel`
already declares `//:lean` (which globs `lean/**`) as test data; the PKI
fixtures are package files; the crash test re-runs its own test binary and
uses `TEST_TMPDIR` when set.

New tests (debug build, this container):

- `tests/warp_tls.rs` (4 tests, under 1 s): every link of a 4-host network
  over a 10 % loss, 5 % reorder, 1 % duplicate wire is a confirmed TLS
  connection with ALPN `vesper-chorus/1` and the expected certificates on
  both sides, for three seeds, and sessions run over it in both directions;
  a rogue-CA host fails as a server and as a client, a valid certificate
  for the wrong host fails as a server (name) and as a client (client name
  check), an untrusted root, expired certificates and a wire that drops
  everything fail the handshake, and PKI misconfiguration is a typed error;
  key updates from both hosts during 80 session messages leave both
  directions on key generation 2 with no authentication failure, and a
  second update before the first is acknowledged is refused (RFC 9001 §6.5)
  as a typed error; CONNECTION_CLOSE fails a blocked receive with
  `NetFail::Closed` within a few steps, the peer records `Peer { error_code
  }`, the producer's next send fails, and other links keep working.
- `tests/durable_log.rs` (10 tests and one child, about 2 s; the review fixes added checks for two adjacent damaged records and an oversized snapshot): CRC-32C check
  value; a real-file round trip with truncation and compaction and typed
  invalid calls; 300 seeds × 120 random persists on `SimDisk` with power
  failures before the log sync (8,942 torn persists, 3,449 recovering a
  partial append, 252,587 unsynced bytes lost, 20,361 garbage bytes),
  always recovering a state between the synced and the target state; 300
  seeds × 60 persists with the power failing between operations inside the
  atomic replacements (6,412 failures: 3,942 recovered the old state, 1,301
  the new one, 1,169 a state in between); damaged files as typed errors and
  3,000 randomly damaged disks without a panic; a real killed process
  (`std::process::abort` between a write and its sync) recovering the last
  synced or the in-flight state, 12 seeds × 3 lives; durable storage
  transparent to the Raft schedule (40 seeds, equal digests); 300 seeds of a
  5-node cluster with snapshots and torn persists (610 crashes, 1,173
  recoveries, 569 torn persists losing 353 entries, 70 disk truncations,
  2,805 snapshot writes, 470 InstallSnapshots) with no checker violation;
  the unsafe reply-before-sync variant produces a `CommittedDivergence`
  (seed 13) while the safe server on the same seeds does not.
- `tests/lean.rs`: now also checks `RecursiveDuality.lean` (13 theorems
  audited) and the Rust projection of its choreographies.
- `tests/warp_session.rs` and `tests/replog_warp.rs` run unchanged over the
  TLS links.

## Root patch

None. The crate's dependencies are unchanged (the TLS types come through
`vesper-warp`'s re-export of `vesper-warp-tls` and `rustls`), so
`Cargo.lock`, the workspace manifest and `BUILD.bazel` are unchanged.

## Decisions

- Mutual TLS rather than server-only authentication: a session placement
  names the host of each role, and the receiver trusts frames by the link
  they arrive on, so both ends of a link must be authenticated. Client
  certificates are checked against the PKI and the expected host name, so a
  host with a valid certificate cannot stand in for another.
- The handshake runs inside `WarpNet::new` over the same lossy wire, with a
  step budget, so a network either has every link confirmed or fails with
  the failing pair named.
- A closed link keeps carrying datagrams (without servicing its streams) so
  that the CONNECTION_CLOSE reaches the peer.
- One durable store per server with three files, rather than a WAL of
  mixed records: term/vote and snapshot changes are rare and atomic
  replacement keeps them simple; only the log needs append, truncate and
  torn-tail handling.
- Recovery keeps the log suffix after a snapshot by comparing the term at
  the snapshot point, the InstallSnapshot rule; this relies on Log Matching,
  as Raft does.
- Persisting at the end of every simulated step, before routing its
  messages, models "persist before you answer" for every RPC (votes,
  appends, snapshots) without touching each handler; the torn-crash mode and
  the unsafe variant show the ordering matters.

## Remaining gaps

- RDMA stays hardware-deferred (no NIC with RDMA in the cloud host); the
  software RDMA model is unchanged.
- `WarpNet` still runs on the software NIC only; no AF_XDP, Retry, 0-RTT or
  migration is used by Chorus. Both TLS peers are Vesper Warp with rustls
  0.23.46; no third-party QUIC peer was tested in this lane.
- The `replog` driver over Warp sessions keeps its logs in memory; the
  durable log is used by the SimPulse cluster (`raft::run_cluster`) and is
  available as a library for other drivers.
- `FsDisk` relies on the platform's `fsync`/`FlushFileBuffers`; the power
  failure tests use `SimDisk`, and the real-file test kills a process, which
  keeps the page cache. No physical power loss was tested.
- No `@choreography` macro and no source typing rules for `Session[P]`:
  Rust ownership plus run-time auditing stands in for them, as before. Rust
  ownership is not evidence that the Vesper source typing rules are
  implemented.
- Performance: only test run times were measured; no throughput, latency or
  allocation claims are made.
