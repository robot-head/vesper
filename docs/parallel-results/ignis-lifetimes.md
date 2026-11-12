# Ignis owned command and fence lifecycle

Base: `1ffd0aec27dfcc24c5564585ae9068255490bf61`. Branch:
`parallel-next/ignis-lifetimes`. This is separate from every externally assigned
wave-three lane. No compiler, Pulse, native, FFI, Prism or planner implementation
was changed. No workspace/dependency/lock registration patch is needed.

## Implemented host protocol

`vesper_ignis::lifecycle` adds an executable host ownership layer above the real
render-graph planner:

- `PreparedPlan::from_graph` invokes the existing planner and keeps its `Plan`
  immutable. Public mutable `Plan` fields cannot be injected into this executor.
  Each graph queue binds injectively to a runtime timeline of the same queue kind,
  with exactly one owned allocator per binding. Foreign timelines, queue kinds,
  allocator mismatches and budgets are rejected with all inputs returned.
- `CommandBatch<Recording, ...>` owns its plan, allocators, resource bundle and
  optional scratch leases. `close(self)` produces `CommandBatch<Closed, ...>`;
  submission consumes that owner. In-flight owners are private to `Runtime`.
- Existing barrier/execute/wait/signal commands are translated without changing
  planner APIs. Every queue's local fence values are checked and rebased against
  that runtime queue's prior reservations. A new original terminal signal follows
  each complete queue stream. `u64::MAX` is reserved for device-removal/error
  sentinels and is never issued or accepted as completion. Definite submission
  rejection may consume a reservation, leaving a legal monotonically increasing
  gap; no later work depends on that unused value.
- Backend outcomes distinguish definitely rejected publication from uncertain
  publication. Definite rejection returns the complete closed owner, including
  uninitialized scratch. Accepted or uncertain work remains in `Runtime` until
  every original terminal is reported complete. Uncertainty blocks further
  submissions, preventing later signals from concealing never-submitted work.
- Ownership is installed before invoking the backend, with in-flight capacity
  reserved before device publication. Backend unwinding leaves an uncertain
  flight retained; unwinding cannot drop the submitted resource bundle.
- `complete_original` validates runtime/timeline membership, issued signal points,
  monotonic observations and all earlier cross-queue waits. Invalid, stale,
  foreign, future and conservatively out-of-order reports return the submitted
  `Completion` intact. Valid cumulative reports can retire earlier frames on a
  queue, but a frame returns owners only after all of its queue terminals.
- `poll` preserves already harvested completions even when the backend reports
  an error. Invalid reports and the remaining suffix return in `unprocessed`;
  backend and protocol errors are both retained when both occur. If the backend
  exceeds its declared poll bound, every returned report is retained for retry.
- `drain` and consuming `shutdown` are nonblocking and bounded by poll/event
  budgets. `Shutdown::Incomplete` returns the runtime owning all pending work,
  along with retired outputs and unprocessed reports. No cancellation request or
  missing signal becomes fabricated retirement. Dropping a still-live runtime
  deliberately leaks/quarantines its backend and pending owners; it never frees
  potentially GPU-visible resources in a destructor.

`WorkGraphScratch<Uninitialized, T>` is consumed by a recording initialization
lease, emitted immediately before its selected real plan pass. Initialization
can target only graphics/compute passes. A Ready owner is returned only after
original terminal retirement, never merely when initialization is queued or
submitted. Later `use_scratch` consumes that Ready owner for the whole flight;
there is no API exposing it while in flight. Ready scratch is tied to its runtime
identity, and foreign initialized scratch is returned on error. Compile-fail
doctests cover false Ready construction and closing a batch twice.

## Deterministic backend and evidence

`vesper_ignis::recording::RecordingBackend` follows actual translated queue
streams, simulates queue order and fence waits/signals, and records submitted
metadata. It is CPU-only and does not execute shaders. Bounded scripts model
rejection, accepted-but-uncertain work, missing completion, backend unwind,
partial multi-queue acceptance and poll failure after harvesting.

The partial multi-queue test publishes and executes only the first queue stream.
Its original signals complete; the second stream is never published and its
terminal never occurs. Shutdown stays Incomplete, both queues' allocators and
all frame resources remain owned, and further submissions remain blocked.

Validation on Linux 6.18.57 x86_64, Rust 1.99.1:

```sh
export CARGO_TARGET_DIR=target-ignis
export CARGO_BUILD_JOBS=1
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_INCREMENTAL=0
cargo fmt -p vesper-ignis -- --check
cargo test -p vesper-ignis --locked
cargo clippy -p vesper-ignis --locked --all-targets -- -D warnings
cargo run -p vesper-ignis --locked --example owned_frame
```

All checks passed: **19 new lifecycle tests + 38 existing planner tests + 3
doctests**, zero failures/ignored. The example runs a real prepared plan through
the recording backend, demonstrates zero-budget owned shutdown, then recovers the
allocator/resources and Ready scratch after original modeled completion. It
prints `retired frame 1 after 1 recorded queue signals; allocator and initialized
scratch returned`.

## Trust boundary and remaining scope

The generic `Backend` is trusted to report original queue fence observations and
to declare definite rejection only when no work was accepted. A public
`Completion::reported` value is an assertion from that boundary, not authenticated
hardware evidence. Real adapters must distinguish device removal and cancel
acknowledgements from original completion. This layer supplies no device-loss
quiescence proof allowing partially published/missing-signal work to be reclaimed.

`OwnerViews` and `Submission` references are temporary submit-time borrows.
In-flight Rust wrappers can move during queue growth/retirement; adapters must
copy native handles and independently own/pin actual GPU/DMA-visible host
buffers. These wrapper addresses are not a stable native ABI. Generic allocator,
resource and scratch payloads can wrap aliases of the same native object: callers
and the backend must supply actual nonaliased allocator/scratch owners, validate
resource-handle mappings and device compatibility, and uphold native API rules.
The host wrapper cannot infer those facts from arbitrary Rust payload types.

Lifecycle limits bound retained in-flight frames, queues, translated commands,
barrier entries, scratch leases and nonblocking poll/drain work. Graph construction
and the existing planner run before this layer and retain their existing resource
policy. Caller-retained outputs and backend-internal allocations are separate.

There is **no Windows/D3D12/DirectStorage/IOCP backend, GPU hardware execution,
residency/eviction implementation, shader reflection or Vesper source typing** in
this slice. No latency, barrier-soundness, source linear-exhaustion or native
handle-alias proof follows from CPU recording tests. Rust owners can still be
leaked or dropped; the runtime's conservative destructor intentionally chooses
leak over premature GPU resource release when explicit shutdown is abandoned.
