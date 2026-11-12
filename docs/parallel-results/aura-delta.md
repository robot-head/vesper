# Aura bounded view-delta milestone

Base: `1ffd0aec27dfcc24c5564585ae9068255490bf61`.
Branch: `parallel-next/aura-delta`.
Owned paths: new `crates/vesper-aura/**` and this report only.

## Delivered behavior

A std-only host library now validates flat keyed node topology, typed property
values and supported events against explicit resource budgets; plans sparse
retained-widget/child/property-slot/subscription changes; and commits through an
explicit atomic backend contract. Same-key/same-kind widgets, property slots
and unchanged registrations survive reconciliation, including reparent/reorder.
Kind replacement replaces the widget while compatible keyed descendants can
survive. Removed registrations disconnect before destruction; removed IDs never
become reusable capabilities.

Preparation and backend rejection retain the proposed input/plan and all live
backend owners. Session/recording revision checks reject stale and foreign
transactions. Every accepted tree remains teardown-budgeted, including when the
recording backend uses independently stricter limits than the session. Close
requests yield consuming veto/allow owners; rejected allow retains the decision
and failed plan, and incomplete shutdown returns its still-live session.

The recording backend executes the patch contract against retained maps,
preserves native-like user state, delivers only changed-slot invalidations,
resolves live registrations to host callback identities, and records only the
latest successful bounded event batch. It stages all mutations and rejects at
any patch prefix without publishing tentative notifications or disconnects.

Thread confinement includes UiThread, owning sessions/backends, prepared plans,
close decisions and Transaction itself/shared borrows. A review found and fixed
an initial shared-transaction thread escape and two independent-backend budget
gaps (text/depth and final teardown capacity); dedicated regressions cover all.
`UiThread::claim_current` proves local-thread identity only; a future toolkit
initialization boundary must separately attest its OS main thread.

Full typed data grammar, API flow and retained/Swift slot contracts are in
[`crates/vesper-aura/README.md`](../../crates/vesper-aura/README.md).

## Validation

All commands used an isolated worktree target with:

```
CARGO_TARGET_DIR=$PWD/target
CARGO_BUILD_JOBS=1
CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0
CARGO_INCREMENTAL=0
```

A temporary standalone `[workspace]` in the new crate enabled checking before
coordinator registration. It and its generated crate-only lockfile are removed
from delivery; no root manifest/lock edits are included.

```
cargo fmt --manifest-path crates/vesper-aura/Cargo.toml -- --check
cargo test --manifest-path crates/vesper-aura/Cargo.toml
cargo clippy --manifest-path crates/vesper-aura/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path crates/vesper-aura/Cargo.toml --example recorded_counter
```

Final results: **1 unit test, 21 integration tests, 8 compile-fail doctests; zero
failures/ignores**. Integration includes 4,225 independently expected keyed-list
transitions; state retention during reparent/root replacement; sparse slots and
unchanged subscriptions; rejected initial mounts, reconciles and unmounts at
every patch prefix; malformed topology/type/event/budget input; stale/foreign
IDs/plans; independent backend budget and closeability checks; owned close and
shutdown recovery. Strict all-target Clippy and formatting pass. The actual
recording example emitted one SlotChanged event and closed with all live
subscriptions disconnected. Logs: `/tmp/vesper-aura-tests.log`,
`/tmp/vesper-aura-clippy.log`, `/tmp/vesper-aura-example.log`.

No native toolkit execution or platform hardware evidence is claimed.

## Coordinator patch

Add `"crates/vesper-aura"` to both root workspace `members` and `default-members`.
Regenerate the root lockfile (new local package only; no external dependencies):

```toml
[[package]]
name = "vesper-aura"
version = "0.1.0"
```

No root compiler dependency, CLI/module registration or schema change is needed.
After registration the same focused checks can use `cargo test -p vesper-aura`
and `cargo clippy -p vesper-aura --all-targets -- -D warnings`.

## Limits and remaining connections

This is the Rust host reconciliation/recording core, not source Pact View/lens
elaboration, source linear teardown, UiThread effect checking, zero-copy FFI,
VirtualArena lowering, a Swift host or GTK/WinUI/SwiftUI integration. The backend
trait requires transactional implementations; arbitrary sequential native calls
are not automatically atomic. Rust permits dropping owners and must-use warnings
do not prove total linear exhaustion. No runtime claim identifies the OS main
thread. Planning traverses full trees and can search child vectors, so no
O(delta), zero-heap, latency, frame-rate or large-table performance claim is made.
Snapshot copies and multiple caller-retained plans consume caller-controlled
memory. Opaque IDs are Rust/process-local; a future compact Swift/C wire mapping
requires its own validated ABI. Callback keys are metadata, not actual delegates.
