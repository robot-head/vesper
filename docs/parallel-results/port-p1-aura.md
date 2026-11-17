# Port wave P1 lane 13: vesper-aura

Claim: [#910](https://github.com/robot-head/vesper/issues/910). The keyed view
runtime of `crates/vesper-aura` (validation, planning, the session state
machine, the recording backend and the SimAura queries) now runs as the Vesper
Loom package `@vesper/aura` in `crates/vesper-aura/vesper/`, linked through
the export ABI behind the non-default `vesper-port` feature. The Rust
implementation stays the default build: by the user's rule (2026-11-17), a
port stays non-default until it reaches reasonable performance parity, and
the benchmark below measures the port at 3.8x to 5.2x the Rust build's time.
`vesper-aura-gtk` is unchanged and passes against both builds.

## API inventory (re-measured before porting)

Public types, all kept with the same names, fields and derives:

- view.rs: `Key`, `Kind`, `Property`, `Value`, `Event`, `HandlerKey`, `Node`
  (`new`, `property`, `on`, `children`), `ViewTree::new`, `Limits` (+
  `Default`), `ViewError`.
- identity.rs: `UiThread::claim_current`, `WrongThread`, `WidgetId`,
  `SlotId`, `SubscriptionId`.
- plan.rs: `Patch`, `Transaction` (`patches`, `base_revision`,
  `next_revision`).
- recording.rs: `BackendError`, `RecordedEvent`, `WidgetSnapshot`,
  `RecordingSnapshot`, `RecordingBackend` (`new`, `snapshot`,
  `reject_next_at`, `set_user_state`, `widget`, `slot`, `dispatch`, `Backend`
  impl).
- session.rs: `Backend` (trait), `Session<B>` (`new`, `revision`, `limits`,
  `widget`, `slot`, `subscription`, `backend`, `backend_mut`, `prepare`,
  `commit`, `request_close`, `resolve_close`, `shutdown`), `PreparedUpdate`
  (`transaction`, `tree`, `into_tree`), `CloseDecision`, and the error and
  report types (`InitError`/`InitFailure`, `PrepareError`/`PrepareFailure`,
  `CommitError`/`CommitFailure`/`CommitReport`, `CloseAction`,
  `CloseOutcome`, `CloseRequestError`, `CloseError`/`CloseFailure`,
  `ShutdownError`/`ShutdownFailure`).
- sim_aura.rs: `NativeUiBackend`, `layout_flags`, `NodeOp`
  (`property_slot`), `SlotValue`, `AuraSlotTable`, `AuditTarget`,
  `WcagViolation`, `WcagReport::is_compliant`, `SimAuraError`,
  `SimulatedEvent`, `SimAura` (`new`, `with_wcag_gate`, `backend_kind`,
  `snapshot`, `focused`, `dispatched_events`, `audit_wcag`,
  `focusable_order`, `focus_next`, `focus_prev`, `trigger_event`, `Backend`
  impl), `encode_view_tree`, `audit_view_tree`.

Tests: 27 integration tests (`tests/reconcile.rs` 11, `tests/validation_close.rs`
10, `tests/sim_aura.rs` 6), one `#[cfg(test)]` unit test in session.rs, and 9
doctests (the `compile_fail` `Send`/`Sync` checks and the examples).

Must stay Rust: the `Backend` trait and every generic over it, the `UiThread`
token and its `ThreadId` check, the `!Send`/`!Sync` markers, and owned public
values (`ViewTree`, `Value`, `String`s) handed back to callers.

## What was ported

| Rust module | Vesper module(s) | What moved |
| --- | --- | --- |
| view.rs | `view.ves`, `util.ves` | Tree validation: keys, parents, reachability, cycles, per-kind property/event rules, every `Limits` budget, teardown fit. |
| identity.rs | `session.ves` | Widget/slot/subscription serial allocation and exhaustion. The process-wide session counter stays in Rust (atomic). |
| plan.rs | `plan.ves` | Keyed diff: create, destroy, reorder (`MoveChild`), property and handler changes, disconnect-before-reconnect order, patch budget. |
| session.rs | `session.ves` | Reconciler state: revision, reservations, commit checks (foreign/stale/closed), adopt, close state machine. |
| recording.rs | `recording.ves`, `apply.ves`, `widgets.ves`, `slots.ves` | Staged apply with all-or-nothing publish, every `BackendError::Invalid` check, injected rejection, user state, dispatch, last-batch recording. |
| sim_aura.rs | `sim.ves` | Focus order, wrap-around, event dispatch decisions, WCAG 4.1.2 audit of trees and mounted state, `NodeOp` topology encoding. |

`lib.ves` holds every type and the 40 exported functions (the export ABI
exports only root-module functions, and types defined in submodules get
mangled binding names). `codes.ves` lists the numeric codes shared with
`src/ported/codec.rs`. 3,966 lines of Vesper mirror 1,814 lines of Rust;
the Rust side keeps 1,357 lines of wrapper and codec plus the 1,770-line
generated binding (`port/binding.rs`, 129 `unsafe` occurrences, all in
generated code; the crate's own code has none).

## What stays Rust and why

- `src/ported/session.rs`: `Session<B>` is generic over `Backend`, and
  `dyn Backend` inside Vesper waits for F7b, so Vesper returns a recorded plan
  (`Prepared`) and the wrapper calls `backend.apply` and then `adopt`.
- `src/ported/recording.rs`: keeps the thread token and a copied
  `RecordingSnapshot`, refreshed after each change, so `snapshot`, `widget`
  and `slot` can still lend references.
- `src/ported/sim_aura.rs`: the focused widget and the dispatched-event log
  are plain owned state; every decision comes from Vesper.
- `src/ported/codec.rs`: flattens `ViewTree` and `Transaction` into words plus
  one text buffer, and rebuilds `Patch`, `Value` and snapshots. Slot values in
  a plan point at the input node and property; Rust clones the value from the
  caller's tree, so strings are never copied through the port twice.
- `identity.rs` keeps `UiThread` (a `ThreadId`) and the atomic session
  counter.

Language constraints that shaped the port: `Fail` cannot be raised while an
`inout` parameter is live (E0402), so every error is a value
(`Option[ViewFault]`, `Option[BackendFault]`); native code does not yet free a
dropped local `ChunkSeq`, so every allocation lives in a handle that is
returned or passed `inout` (a counting test checks this, below); `vesper fmt`
caps a file at 8,192 tokens, so the port is a package of eleven modules.

## Unit tests

The one unit test, `exhausted_ids_and_revision_fail_without_mounted_changes`,
forced private counters. It is translated into `src/ported/session.rs` and
forces them through an exported test hook (`reconciler_force_counters`). A new
unit test, `port_frees_every_allocation_it_makes`, runs mount, update,
dispatch, rejection, close and shutdown cycles on `RecordingBackend` and on a
WCAG-gated `SimAura` and checks that the port's live-allocation count returns
to its start. The two unit tests share a lock because the count is
process-wide.

All 28 tests and the 9 doctests pass against both builds, as does a 29th
integration test added in review (`maximum_key_is_an_ordinary_key_when_parent_and_child_swap`):
the port had used `u64::MAX` as its "no parent" sentinel in one comparison,
so a parent keyed `Key(u64::MAX)` was mistaken for a missing one.

## Divergence checks

`build.rs` applies five edits under the `vesper-port-divergent` feature, one
per ported module, and `BUILD.bazel` builds each as its own
`vesper_port_object`. `scripts/test-port-harness.sh` asserts that each one
fails a named test while another test in the same binary still passes. Each
edit was also built alone to check that it is the one the named test catches.

| Module | Edit | Caught by |
| --- | --- | --- |
| view.rs | one child over the limit is accepted | `budgets_fail_before_backend_and_accepted_tree_remains_unmountable` |
| plan.rs | `MoveChild` reports its positions swapped | `keyed_insert_remove_reorder_and_stale_handle_rejection` |
| session.rs | a stale plan passes the revision check | `stale_and_foreign_plans_retain_input_and_do_not_call_backend` |
| recording.rs | an injected rejection fires one patch late | `initial_mount_rejection_never_publishes_widgets_slots_or_registrations` |
| sim_aura.rs | `Input` drops out of the focus order | `sim_aura_focus_events_atomic_wcag_gate_and_close_decision` |

## Benchmarks

`examples/abba.rs`, run by `crates/vesper-aura/bench-abba.sh`. The two builds
cannot share one binary (the public failure types name a single `Session`),
so the example is built twice and its `compare` mode runs the executables as
separate processes in Rust, port, port, Rust order per epoch. Each run warms
up for 50 operations and then times 200; the pair totals are analyzed with
`vesper_bench::analyze_pairs`. Trees are built before the clock starts.

Workloads, over a 64-row tree (257 nodes: per row a Container holding a
Text, a Button and an Input):

- `one-text`: `prepare` plus `commit` of a tree that differs in one Text.
- `reorder`: `prepare` plus `commit` of a tree with its rows rotated by one
  and every row Text changed.
- `encode-audit`: `encode_view_tree` plus `audit_view_tree`.

Setup: 4-vCPU Intel Xeon @ 2.10 GHz cloud container, Linux, Rust 1.97
`--release`, the port compiled by the `vesper` executable from this branch
through `vesper-port-harness`; 15 epochs.

| Workload | Rust ns/op | Port ns/op | Port/Rust median | Range over epochs |
| --- | ---: | ---: | ---: | --- |
| one-text | 475,575 | 1,898,130 | 3.84x | 3.12x to 5.68x |
| reorder | 555,501 | 2,780,751 | 5.24x | 4.68x to 5.93x |
| encode-audit | 166,098 | 664,907 | 4.00x | 3.42x to 5.61x |

The port is slower than 2x on every workload, so the Rust code is not deleted
(lane rules, "Performance"). Not yet profiled. Candidate costs: every call
flattens the tree into words plus a text buffer and decodes it again in
Vesper, the recorder's published state is copied into a Rust snapshot after
every change, and the Vesper side keeps its maps as sorted index arrays
searched by binary search, with a bounds-checked call per element access.

## Platforms run

- Linux (cloud container), Cargo, Rust 1.97: `cargo test -p vesper-aura`
  with and without `--features vesper-port`, the vesper-aura section of `scripts/test-port-harness.sh` (five divergent
  builds), and `cargo +1.100.0 clippy --all-targets -p vesper-aura -p
  vesper-aura-gtk -- -D warnings`, with and without `vesper-port-divergent`.
- Linux, `vesper-aura-gtk` with `--features gtk` under
  `dbus-run-session -- xvfb-run -a` (`GSK_RENDERER=cairo`), against both
  builds.
- Bazel (BuildBuddy), macOS AArch64 and Windows x86_64 MSVC, and the
  `aura-gtk.yml` workflow: through CI on the PR.

## Rust code deleted

No. The Rust implementation (`src/reference/`) is the default build and
takes bug fixes only; the port is built with `--features vesper-port`, and
`scripts/test-port-harness.sh` runs the crate's tests against both builds
plus the five divergent builds. Deletion waits for performance parity.
