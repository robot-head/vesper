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
