# Structured Lean rewrite replay exporter (#242)

## Ownership and source

This lane owns only `src/optimize/witness_lean.rs`,
`tests/replay42_export.rs`, and this report. The full published contract
`63d60cd` was read before implementation. Worktree alignment used dependency
merge `9ce68ce`; its expected add/add contract conflict was resolved to the
coordinator's published frozen contents. That alignment is not owned source.
No Cargo/core/CLI/backend/AST-library, model or module-registration edit was
made by this lane.

Owned source/test commits:

* `6ae8a34`: complete exporter, all 24 catalog mappings, bounded structured
  planning/building, both public entrypoints, canonical digests and rendering.
* `9d67b6c`: remove an unused import, prepay the certificate counting pass, and
  consume already prepaid final-source hashing credit rather than double charge.
* `ac17735`: nine public exporter tests, including actual parsed/checked input
  through the genuine producer, cumulative Rust gate and replay preparation.

## Implemented boundary

Public standalone export verifies actual source/candidate/type/certificate
binding with the existing independent Rust checker before Lean AST allocation.
The source-based helper invokes the actual private producer and verifies with
its returned cumulative budget before entering a separate explicit export
budget. Barrier/unchanged returns no artifact; failure never returns a partial
fold, AST, source prefix or file. The default optimizer does not invoke Lean
or change its behavior through these artifact APIs.

One two-pass lowering implementation first computes allocation-free structured
Piece statistics and then builds the same declaration templates. It emits one
named typed definition per term and one theorem per proof record, followed by
the fixed final theorem. Each definition uses its own declared Kind. All DAG
edges remain named references; there is no proof/term flattening. The source
uses vesper-lean-syntax exclusively, with no raw Lean node, formatted Lean
fragment, source reparse or Lean process. Source identifiers are numeric model
variable IDs rather than generated Lean names.

All 24 fixed model lemmas are mapped. Guards use an actual leaf Total proof or
the earlier equality in its correct orientation plus total_of_eq. Congruence,
symmetry, transitivity and reflexivity retain exact endpoints. Six fold lemmas
receive actual signed Int constants, three Fits proofs and the independently
decided closed Value equality; no function equality or Rust-computed boolean
is substituted for that proof. Named implicit kind arguments and the nominal
Value wrapper keep U64 and I64 distinct despite equal widths.

Before constructing the AST, the export plan bounds declarations, AST objects,
depth, generated names, serialized bytes, logical metadata and work. It prepays
cumulative printer subtree/copy costs across all declarations, output growth
and final-source hashing. Consuming render makes exactly one central serializer
call and checks actual output length against its structural bound. Original
caller AST storage, physical allocation failure, allocator/RSS and external
Lean runtime retain the contract's explicit limitations.

Certificate and source/candidate binding hashes follow the frozen domain,
tag, role, length and little-endian encoding. Hashing is streamed and prepaid;
no Debug formatting, unordered map traversal or whole-TypeMap scan is used.
Only actual relevant typing lookups and original spans enter the binding hash.
The returned manifest is diagnostic association metadata with MathModelEq
scope, not KernelAccepted or cache approval. The static model source and all
33 exact audit-root names are inspectable without spawning Lean.

## Authored tests and actual coordinator evidence

The nine exporter tests exercise structural namespace/definition/proof sharing,
manifest counts, independently encoded certificate and binding hashes, model
and rendered-byte hashes, Vec-capacity digest independence, irrelevant TypeMap
entries, source-name isolation, malformed/incorrect-span refusal, every limit
field, exact structural bounds versus one-below refusal, genuine producer
exports including U64MAX squared, barriers/unchanged/refusal and all audit roots.

This authoring lane executed only rustfmt on owned Rust files and
`git diff --check`, both passing. **No Cargo build/test, Clippy or Lean process
was run by this lane**, respecting the coordinator-only build lease.

The coordinator reported actual `cargo check --locked -p vesper --lib` success
with the integrated module/model. At root head `94b704f` (source integration
`36b567e`), actual `tests/replay42_export.rs` execution passed **9 tests, zero
failures/ignored**, recorded in `/tmp/replay42-root-export.log`. Workspace
format checking also passed. Read-only semantic review by the model owner
found no mapping defect across the 24 cases, kind handling or proof arguments.

Coordinator/acceptance execution has additionally compiled the four per-kind
aggregate catalogs (96 mappings, all reasons and guard orientations) and actual
producer fold/identity/guard/endpoint artifacts, with actual restricted axiom
audits accepting the full 33 model roots plus final theorem. These observations
are attributed to those lanes. The full hostile-artifact/policy matrix and
broader final checks remain pending their final coordinator report at this
checkpoint; fixture diagnostic-matcher corrections are not kernel acceptance.

## Scope

The independent kernel theorem establishes equality in the bundled mathematical
model. It does not prove the Rust binding/checker/translator or the compiler
correct, establish a source-language refinement, or certify backend machine
code. No throughput, linear-time, process-memory, axiom-free or unexecuted
platform claim is made. A consumer must compile the actual bundled model and
generated source and audit the actual theorem closure under explicit policy.
