# Port P3b lane 6: vesper-chorus in Vesper, part 1 (choreography front end)

Claim: #1068 (stays open: this is part 1 of the lane). Base: `main` at
8212ddf. Lane prompt: `docs/agent-prompts/port-p3b/06-chorus.md` (plan PR
#1072).

## Summary

- `@vesper/chorus` (`crates/vesper-chorus/vesper/`, 7 modules, about 2,900
  lines) runs the choreography front end: the lexer, parser and right-fold
  builder of `parse.rs`, the well-formedness checks of `chor.rs`, endpoint
  projection with full branch merging (`project.rs`), binary duality and the
  bounded asynchronous explorer (`check.rs`).
- **The Vesper library is the only build of these parts and their Rust
  code is deleted** (the owner's call once the port beat Rust, as for
  Aura). The port first landed behind a non-default `vesper-port` feature
  and was measured against the Rust build; the last commit with the Rust
  implementation is `a2adca20`, which `scripts/bench.sh` builds as its
  baseline. `build.rs` always links the library, as in `crates/vesper-aura`.
  The crate is outside the compiler's closure, so there is no seed.
- Raft, sessions, the simulated network, durable storage, Warp and RDMA
  stay Rust until part 2. The shared types (`Choreography`, `GlobalChor`,
  `LocalType`, the errors) and the `LocalType`/`GlobalChor` helper methods
  that the session monitors call stay Rust too.
- Every crate test passed on both builds before the deletion (110 tests,
  plus the new ones below), and passes on the Vesper library now. Because
  Raft and the session monitors call `parse_choreography` and `project` for
  their protocols, `raft_sim`, `raft_depth`, `session`, `warp_session`,
  `warp_tls` and `replog_warp` run their protocols through the library.
- Speed (ABBA, release, 12 epochs): the port takes **0.70x** the Rust
  build's time to parse and project 24 choreographies (0.82x the
  instructions), and **0.004x** to explore 6 (0.006x the instructions),
  because the port interns local types and explorer states.

Part 2 (Raft's cluster model, sessions over `Box[dyn Transport]` and the
simulated network) is not in this PR; see "Remaining" below.

## API inventory

| Item | Implementation |
| --- | --- |
| `parse_choreography` | `parse`: lex, parse, build and check in Vesper; the wrapper rebuilds `Choreography` from the result words |
| `Choreography::new` (well-formedness) | `check_chor` |
| `project`, `project_all`, `project_role` | `project` (one call for all roles in `project_all`) |
| `are_dual` | `dual` |
| `check::explore`, `check::explore_locals` | `explore` (projection and exploration in one call for `explore`) |
| `LocalType::{choice, mentions, subst, unfold, alpha_eq, head_summary, render}`, `GlobalChor::{node_count, involves, free_vars_within}`, `Choreography` accessors, the error types and their `Display` | Rust: methods of the shared Rust types. Their logic is also ported (in `local.ves` and `tree.ves`), as part of the entry points above |
| New: `IMPLEMENTATION`, `port_live_allocations` | `"vesper"`; the library's leak counter |

The types (`Choreography`, `GlobalChor`, `LocalType`, the error enums) stay
the crate's own Rust types. `src/port.rs` encodes a
call's inputs as words plus one text buffer and decodes the result words;
the formats are documented there and in `vesper/src/lib.ves`.

## Design

- **Symbols.** Every string (role names, labels, payload types, recursion
  variables, actions, and the messages the port formats) is an interned
  symbol (`util.ves`), so string equality is one word. Byte order of
  symbols is Rust's `String` order, which `LocalType::choice` sorts by.
- **Shared continuations.** The parser's builder gives every arm of a choice
  the same continuation node; the Rust implementation cloned the tree. Each node
  keeps its expanded size, so `MAX_NODES` is charged exactly as in Rust.
- **Hash-consed local types** (`local.ves`). Equal local types are equal
  ids, so `merge`'s `a == b`, the explorer's `*t == End` and its visited
  set compare words. `unfold` is memoized per node.
- **Explorer states** (`explore.ves`) are runs of words (one local-type id
  per role, then the non-empty queues in `(from, to)` order) in one hash
  table. States are visited in the Rust order (a stack, successors in role
  and branch order), so state counts and the first error are identical.
  This, not code generation, is why exploration is two orders of magnitude
  faster: the Rust implementation cloned and hashed whole `LocalType`
  trees for every state.
- **No recursion over untrusted shapes at the boundary.** Trees cross in
  postorder and are rebuilt with a value stack on both sides; the port's
  recursive passes are bounded by `MAX_DEPTH` (validated trees) as in Rust.
- **Errors** come back as typed results with the same positions, texts and
  variants. `ParseErrorKind::UnexpectedToken::expected` is an index into
  the wrapper's table of the parser's `&'static str` texts. The port has
  no `unsafe`; the generated binding is the only unsafe code (export ABI
  calls), as in every port.

## Unit tests

The crate has no `#[cfg(test)]` unit tests, so none needed treatment. New
tests (run against both builds before the Rust code was deleted):

- `tests/differential.rs`: 3,000 seeded random choreography sources (about
  40% malformed: undeclared and unused roles, self-messages, repeated
  labels, unbound `continue`, lexical damage), parsed, projected, checked
  for duality, explored (with and without hand-swapped endpoints), with the
  `Debug` and `Display` text of every result folded into one digest pinned
  from the Rust implementation. It found one bug during the port (a message text).
  `CHORUS_DIFF_DUMP` and `CHORUS_DIFF_SHOW` locate a diverging seed. Also
  hand-built trees through `Choreography::new`.
- `tests/implementation.rs`: repeated parse/project/explore calls leave no
  live allocation in the library, and a 20,000-arm choice with descending
  labels projects in O(n log n) (a review found the first version's
  insertion sort quadratic).

## Divergence checks

One edit per ported module (`DIVERGENT` in `build.rs`, mirrored as
`chorus_vesper_divergent_<module>` objects in `BUILD.bazel`).
`port/test-port.sh` (run by `scripts/test-port-harness.sh`) builds each
alone and asserts that its guarding test fails while
`binary_projections_are_dual` still passes.

| Module | Edit | Caught by |
| --- | --- | --- |
| util | symbols compare in reverse byte order | `informed_third_party_merges_into_an_offer` |
| tree | unused roles are never reported | `well_formedness_errors_are_typed` |
| local | one-branch choices stay Select/Offer | `two_buyer_projects_to_the_textbook_local_types` |
| project | the absent-role loop rule is inverted | `role_outside_a_loop_projects_through_it` |
| explore | the queue bound is never reported | `recursion_projects_and_runs` |
| parse | brace nesting is unbounded | `hostile_inputs_are_bounded` |

## ABBA

`examples/abba.rs`, driven by `scripts/bench.sh` (release; the Rust arm is
built from `a2adca20` in a git worktree; both arms are worker processes, `vesper-bench`'s `run_abba`, 12 measured epochs of
A/B/B/A, digests compared on every batch). Cloud host, 4 vCPUs, Linux
x86_64. Instruction counts: `valgrind --tool=callgrind` on `abba once`
(2.5M startup instructions in both, included).

| Workload | Rust | Vesper | Vesper/Rust wall (95% CI) | Instructions Rust / Vesper (ratio) |
| --- | --- | --- | --- | --- |
| project: parse + `project_all` + `render`, 24 choreographies (textbook, Raft, generated 4-12 roles) | 2.29 ms / choreography | 1.59 ms | 0.70x (0.68x-0.75x) | 2,455M / 2,025M for 5 batches (0.82x) |
| explore: parse + `explore`, 6 choreographies | 26.1 ms / choreography | 0.092 ms | 0.004x (0.003x-0.004x) | 979M / 6.1M for 1 batch (0.006x) |

The project numbers include the boundary: encoding the `Choreography` as
words, and decoding the `LocalType` trees into Rust `String`s and boxes,
which is a large part of the port's time. `render` runs in Rust in both.

## Compiler gaps (no blockers)

1. **Reads of a struct inside an `inout` call's arguments** (E0301):
   `f(inout c, g(in c, x))` and `c.out.push(ga(in c, n))` are refused, so
   every argument that reads the handle is hoisted into a `let` first (about
   140 places). Repro:
   `fn g(in c: C) -> u64 { c.x } fn f(inout c: C, v: u64) { c.x = v; } fn h(inout c: C) { f(inout c, g(in c)); }`.
   A two-phase borrow of the inout argument (evaluate the other arguments
   first) would remove the hoisting; #986 did this for method receivers.
2. **String views cannot be bound** (E1531): `let b = s.as_bytes();` on an
   `in Str` is refused, so the bytes go straight into a helper call.
3. **No module constants**: tags and limits are functions or literals.
4. **Integer literals default to `i64`** in `let t = if c { 1 } else { 2 };`
   and `for k in 0..3`, which then fail against `u64` parameters.
5. **`with` is a keyword**, which the diagnostic names only as "expected a
   lower-case identifier".

Std candidates (kept private in the package): `intern_tb` (a byte-string
interner), `sat_add`/`sat_mul`, and a growable open-addressing table over
word runs (`intern_state`).

## Root patch and shared files

- `Cargo.lock`: `vesper-port-abi`, `vesper-port-harness` (build) and
  `vesper-bench` (dev) for `vesper-chorus`. No root `Cargo.toml` change.
- One line in the `fixtures()` skip predicate of
  `tests/fixtures/self_host/driver.rs` (the package uses `use`, #880).
- One line appended to `scripts/test-port-harness.sh`.

## Checks run

- Before the deletion: `cargo test -p vesper-chorus` with and without
  `--features vesper-port`, all pass (Linux x86_64). After: `cargo test -p
  vesper-chorus` (via `port/test-port.sh`), all pass.
- `port/test-port.sh` divergence loop: all six modules detected.
- `cargo +1.100.0 clippy -p vesper-chorus --all-targets -- -D warnings`,
  with and without `--features vesper-port-divergent`; `cargo fmt --check`;
  `vesper fmt --check` and `vesper check` on the package.
- Bazel targets are run by CI (BuildBuddy), not locally.

## Remaining (part 2, keeps #1068 open)

- **Raft cluster model** (`raft.rs` `run_cluster`, `replog.rs`). Every Raft
  RPC is a `RawSession` whose monitor steps the projected local type, the
  run is scheduled by SimPulse with its `Journal`, and durable mode
  persists through `DurableLog<SimDisk>`, which stays Rust. Porting it means
  porting the `RawSession` monitor and `LeakAuditor` from `session.rs` and
  driving the SimPulse port (`@vesper/simpulse`) Vesper to Vesper, with the
  durable mode either kept Rust or reached through a `.vsys` adapter.
  The wire codecs (`RaftAppendWireHeader`, `encode_batch`, `AppendAck`,
  `VoteReq`) and the `Checker` are pure and can go first.
- **Sessions** over `Box[dyn Transport]` and `simnet.rs` over
  `Box[dyn SimRole]`: after lanes 1 and 2 (#1063, #1064).
- `durable.rs`, `warpnet.rs` and `rdma.rs` stay Rust (plan).
- Raft and sessions stay Rust until they are ported, then follow the same
  rule (the owner's call on deletion).
