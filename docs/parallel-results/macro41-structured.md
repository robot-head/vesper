# Structured WASI quotation/cache foundation — issue #107

Branch macro41/structured from fd09dd7; exclusive paths from macro41-contracts.md.
The overall foundation is in progress. This first checkpoint is transport only;
it does not claim runtime, cache, source Tier2 integration or checked compilation.

Added separate self-contained vesper:forge-quote@0.1.0 WIT package, world
sandboxed-quotation, interface quotation, payload1. Existing flat WIT/API unchanged.
The new quote_transport module has public raw invocation/Expr input slot/generated
claim/InputSlot(u32)/packed result DTOs and validated finite Limits. No scopes,
nonces, host handles or approval cross this boundary. Guest spans are display data;
DefIds are claims. Before per-item copies, unpack checks table/list caps, failed
outcome emptiness, all checked slices/UTF8/lexical kind coherence and the sum of
aliased referenced lengths. Repeated slot references have a separate count cap;
actual current-input expansion/copy checks belong to the typed host adapter.

Defaults/HARD mirror the agreed contract (HARD is explicit finite maxima). Raw
record/invocation byte limits are exposed for the upcoming bounded cache codec;
they are not yet advertised as implemented encoders in this checkpoint.

Read runtime package/network guidance and current policy before official
`rustup target add wasm32-wasip2`; installation succeeded without sudo/config
changes. Host rustc1.99.1 (2026-10-13). Actual new fixture build/run is pending.

Lightweight temporary harness /tmp/vesper-quote-transport-harness references the
actual new source and unchanged legacy transport; no production Cargo/lib wiring
was committed. Private target /tmp/vesper-editor41-protocol-target, 1job/debug0/
incremental0. Six new integration tests +5 legacy transport tests pass; strict
all-target Clippy passes. New tests cover actual claims/slot indexes, alias bombs,
failed items, bad slices/UTF8/count/work/occurrence limits, kind/text mismatch,
integer overflow/hex/underscores, Unicode/surrogate/string truncations, invocation
caps and delimiter balance. Final full-runtime checks will supersede this harness.


## Runtime checkpoint

Actual quotation runtime now shares the legacy start_validated/complete_call
helpers (WASI determinism, declared readonly mounts, output-only writable mount,
fuel/memory/table limits, denied sockets/environment, bounded stdout/stderr).
The old flat host methods preserve behavior. Child host::quote_runtime is
reexported as quote_host by coordinator wiring; no broad visibility is added.
New direct methods run_quotation/run_quotation_async accept the separate DTO and
validated Limits. Count/table headers are checked before moving lifted WIT
vectors, and alias/UTF8/kind validation precedes per-item string copies.
Encoded invocation size is now checked even on direct sandbox input validation.

Real new guest built twice from separate private target directories with locked
dependencies and the official installed wasm32-wasip2 target. Current fixture
SHA256 a2e6404be232dfbf2d6216f5cb4e50ea422aac5796a541577ee24cf956c59dda
matched both builds. Binary lives fixtures/quotation/bin/quotation.component.wasm;
new guest lock/build script are committed, legacy fixtures unchanged.
Four actual Wasmtime integration tests plus six transport tests pass. They run
real structured identity/duplicate/claims/hostile variants and check
fuel/memory/stdio denial, ambient file/env denial, deterministic time/randomness,
output detection and old flat-world link rejection. Root acceptance owner also
independently executed the previously built 4+6 test binaries read-only. The
current fixture adds call-connect/read-input cases for upcoming raw-cache tests;
no source Tier2 integration or compiler checked artifact is claimed.

## Raw cache and current-context adapter

The additive quote_key/quote_record/quote_runner modules now encode canonical
VFQI invocation bytes and VFRQ/schema1 raw records, with separate builder/profile,
WIT digest, protocol, schema, observable limits/determinism, measured executable
identity, component and declared-input snapshot bindings. Keys are constructed
from scratch; legacy snapshot preparation supplies verified identities only.
The codec preflights count/minimum lengths, work, UTF8, kinds, slot ordinals,
diagnostics, aliased copy limits and canonical option payloads before allocating
decoded output. Cached bytes contain only raw claims/ordinals/diagnostics/metering
and stdio, never tokens, scopes, nonces, resolution or approval.

The runner bounds reads of the actual opened file and hashes exactly those bytes.
It rejects nonregular objects and, on Unix, opens with O_NOFOLLOW|O_NONBLOCK before
checking the opened descriptor again. Ancestor directory replacement is not a
capability-confinement guarantee. Fresh prepare snapshots bind actual declared
input changes; execution rejects guest errors/output files/input mutation and
publishes only valid raw success records. Verify reruns and compares raw bytes.

The optional quote_session::expand_fragment adapter imports into the current
sealed typed arena, resolves the entire current body and checks the current
trusted registry/graph/policies after every hit or miss. This is conditional
fragment approval, never compiler ExpandedChecked or source Tier2 compilation.
Private fields bind components to the structured profile. The only typed API
amendments are the approved crate-private cumulative metadata charger and public
sealed expansion current-host/liveness validation. Export accounts for staged
DTO storage before allocation; repeated input expansion has token/text/scope caps.

Actual cache testing caught the capture guest's bare let sequence, which the
typed Expr checker correctly rejected. The fixture now emits an explicit block;
locked independent builds again match, with final SHA256
92c31f0407dd6efc5ca44cc05954c8f3e095c5603b530c3c6c686ad9b3bb00ce.
No checker/category relaxation was made.

The full actual Forge/runner/sandbox suite passed 153 tests in19 suites with no
ignores before the final filesystem guard test. It includes unchanged legacy flat
runtime/cache/session behavior, actual Wasmtime structured execution and typed
hygiene. Final raw7cache+4codec tests and strict all-target Clippy are checked
again after the guard addition; results are recorded in the follow-up below.
Final seven raw-cache and four codec tests pass with zero ignores, including
static Unix FIFO/final-symlink rejection without execution and fresh current-host,
stale-expansion, invalid-slot, metadata and repeated-copy guard cleanup. Strict
all-target Clippy passes for Forge, Forge runner and sandbox. Its private verdict
stores the accepted fragment behind a Box, matching the existing flat session.
All heavy verification uses private /tmp/vesper-editor41-protocol-target, one job,
debug0/incremental0. A mistaken lock-update command briefly started an independent
worktree default target and was stopped; no coordinator cache was written.
