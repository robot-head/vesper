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
* `4636168`: count Name/Ident, Argument, namespace/module structural levels and
  the import ModuleName node consistently; add an independent actual-AST walker
  and exact-depth success/one-below refusal checks. There are now ten tests.

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
and final-source hashing. Structural depth includes Name-to-Ident and Argument
intermediaries as well as namespace/module levels; the import is counted as a
ModuleName node. Vec/Box indirections and literal/string storage are payload
accounted in bytes. Independent tests walk the public emitted AST and match
the exact reported node count/depth. Consuming render makes exactly one central serializer
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

The ten exporter tests exercise structural namespace/definition/proof sharing,
manifest counts, independently encoded certificate and binding hashes, model
and rendered-byte hashes, Vec-capacity digest independence, irrelevant TypeMap
entries, source-name isolation, malformed/incorrect-span refusal, every limit
field, exact structural bounds versus one-below refusal, genuine producer
exports including U64MAX squared, barriers/unchanged/refusal and all audit roots.
The added independent structural walker also exercises genuine identities,
signed-MIN folds and prior-totality proof applications.

This authoring lane executed only rustfmt on owned Rust files and
`git diff --check`, both passing. **No Cargo build/test, Clippy or Lean process
was run by this lane**, respecting the coordinator-only build lease.

The coordinator reported actual `cargo check --locked -p vesper --lib` success
with the integrated module/model. The final focused run at root head
`ed70d8df037dac917f314bdf69c608d86d400ea5`, including `4636168`, passed
**10 exporter tests** and the **one complete kernel acceptance target**, with
zero failures/ignored in either target. Kernel acceptance took 105.61s. The
actual log `/tmp/replay42-root-final-focused.log` records the independent AST
node/depth walk and exact-limit tests passing. Final workspace format checking
also passed. These are coordinator executions, not source-lane executions.

Separately, the earlier full root-package baseline at `94b704f` passed 884 tests
across 74 targets with zero failures/ignored, including the pre-metric kernel
matrix (105.72s). That baseline precedes the structural-metric fix; its count
is not combined with the final focused run. Final package all-target strict
Clippy passed on `ed70d8d` in 19.82s, recorded in
`/tmp/replay42-root-final-clippy.log`; final formatting/diff checks also passed.
Read-only semantic review by the
model owner found no mapping defect across the 24 cases, kinds or proof arguments;
the coordinator reviewed the subsequent structural-counter fix as correct.

Coordinator/acceptance execution has additionally compiled the four per-kind
aggregate catalogs (96 mappings, all reasons and guard orientations) and actual
producer fold/identity/guard/endpoint artifacts, with actual restricted axiom
audits accepting the full 33 model roots plus final theorem. The final actual
kernel target also passed its complete hostile-artifact and policy matrix.
These observations are attributed to the coordinator/acceptance lanes, with
their separate model-only claim. Earlier fixture diagnostic-matcher corrections
are superseded by this complete executed target, not counted as additional tests.

## Scope

The independent kernel theorem establishes equality in the bundled mathematical
model. It does not prove the Rust binding/checker/translator or the compiler
correct, establish a source-language refinement, or certify backend machine
code. No throughput, linear-time, process-memory, axiom-free or unexecuted
platform claim is made. A consumer must compile the actual bundled model and
generated source and audit the actual theorem closure under explicit policy.
