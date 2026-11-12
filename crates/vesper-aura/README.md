# Aura reconciliation core

This dependency-free host library implements a bounded keyed view runtime and
an executable recording backend. It is an initial library milestone for design
§18.2, not a Vesper compiler feature or a native UI toolkit.

`examples/recorded_counter.rs` mounts a tree, dispatches a registered event to
its trusted handler identity, applies one fine-grained slot change, and consumes
a close decision to unmount it.

## Accepted declarative data

The API consumes `ViewTree { root: Key, nodes: Vec<Node> }`. Keys are unique
throughout the entire tree. Each non-root has exactly one parent, every child
exists, every node is reachable, and cycles are rejected. Only `Container` has
children. Node vector storage order is immaterial; child order is meaningful.

| Kind | Text property | Value property | Events |
| --- | --- | --- | --- |
| Container | Unsupported | Unsupported | None |
| Text | String | Unsupported | None |
| Button | String | Unsupported | Click, Focused |
| Input | String | String | Changed, Focused |

All kinds accept `Enabled`/`Visible` Boolean properties and nonnegative `Width`
integers. Properties may be absent; the core prescribes no native toolkit
defaults. Strings are UTF-8 Rust strings; this milestone imposes no semantics on
NUL, Unicode normalization, rich text, pixel units, or native string conversion.
Handler keys are trusted embedding-host callback identities, not function
pointers, captured closures, compiler messages or registered OS delegates.

`Limits` bounds nodes, children per node, depth, properties/subscriptions per
node, bytes per string, aggregate text and patches per transaction. Accepted
states also fit their entire teardown within the patch budget. Invalid input is
returned intact without changing mounted/backend state. Caller-allocated input,
copied output snapshots and application-retained plans remain caller-owned
memory; the core does not meter their combined external lifetime.

## Preparation and commit

1. Claim `UiThread::claim_current` on the host's required UI thread. This only
   proves current-thread confinement; it does **not** identify the OS main
   thread. A real toolkit adapter must establish that at platform initialization.
2. `Session::new` takes the backend owner and limits. `prepare(Some(tree))`
   validates the complete tree, reserves opaque identities, then produces a
   `PreparedUpdate`. `prepare(None)` produces full unmount.
3. `commit` invokes the backend transaction. On success it advances the mounted
   revision. On failure it returns the same plan/tree and retains the old
   backend/widget/subscription owners for retry. Preparing/discarding a plan
   never touches mounted state; reserved identities are never reused.
4. Only the same session and base revision can commit a plan. Multiple prepared
   alternatives may exist; committing one makes the others stale. A stale or
   foreign rejection returns the plan's owner.

Widget identity is retained for the same key **and kind**, including across
reparenting. Changing kind replaces that widget while compatible keyed children
may survive. Property slots survive value changes; removal/readdition mints a
fresh slot. Event registrations survive only when event and handler key match.
Removed/changed registrations disconnect once before their widget is destroyed.

Planning traverses both trees and searches bounded child vectors. It allocates
metadata and is not O(delta). Patches themselves are sparse: unchanged values
and subscriptions emit nothing; reorders use child moves, and changed values
emit exactly one `SetSlot`. No frame-rate or allocation-performance claim is made.

## Backend contract and future Swift host

`Backend::apply` must atomically apply the complete immutable `Transaction`.
An error must leave retained state, registrations, revision and notifications
unchanged. A real adapter needs staging or a genuine rollback protocol;
sequentially making native calls and returning after partial effects is
unsupported. Rust's type system does not prove this semantic trait obligation.

Patches detach moved children, disconnect removed subscriptions, unbind removed
slots and destroy removed widgets before attaching/reordering the new topology.
`BindSlot` establishes an initial value. `SetSlot` changes one property and
requires exactly that slot's notification only after successful commit. A
future Swift adapter can bind its leaf observations to these slots and map
opaque `SlotId` values to its own bounded compact IDs. There is no C wire format,
Swift host, ObservationRegistrar integration or actual native notification call
in this milestone.

`RecordingBackend` actually executes these operations against retained widget,
slot and live-subscription maps. It preserves independently mutated widget
`user_state`, rejects stale/foreign IDs and revisions, and stages all work before
publishing it. It independently enforces its own configured budgets, including
future teardown capacity. It keeps only the latest successful event batch and
checked aggregate registration counters; history storage belongs to the caller.
Failure injection can reject before any patch or after all patches to verify
that no tentative event, disconnection or mutation becomes visible.

`Session`, `RecordingBackend`, `PreparedUpdate`, `Transaction` (including shared
borrows), `UiThread` and `CloseDecision` are thread-confined. Widget/slot IDs and
snapshot copies are metadata and may be copied; they are not native owners.

## Close and shutdown ownership

`request_close` yields one non-Clone `CloseDecision` and blocks competing close
requests. The application may retain it while doing asynchronous work, then
consume it with `resolve_close(Veto)` or `resolve_close(Allow)`. Veto keeps the
mounted state. Allow atomically unmounts, disconnects subscriptions and closes
the **session**; it does not itself close an OS window. A rejected close returns
both the close decision and failed plan, preserving live owners for retry.
Ordinary reconciliation may continue while a close decision is pending.

`shutdown` consumes the session, unmounts, and returns the backend owner. Failure
returns the still-live session and failed plan. A pending close must first be
resolved. Dropping a close decision leaves the session pending rather than
silently deciding; Rust permits Drop, so this API is not source-language linear
exhaustion or an exact-once guarantee for arbitrary application code that drops
its owners. There is no Pulse co-scheduling, compiler `UiThread` effect, lens
elaboration, virtualized table, accessibility proof or native platform binding.
