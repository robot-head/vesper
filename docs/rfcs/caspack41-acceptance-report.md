# Independent CAS transfer acceptance (#199)

Published contract and shared scaffold aff02c4 were read before writes, including
the pre-writer bounded occupied-object publication amendment. Owned work is new
`tests/transfer_acceptance.rs`, `examples/transfer.rs`, `docs/cas-transfer.md` and
this report. No production/dependency/build/workflow file is edited by this lane.

Independent wire construction uses frozen domain/kind tags, little-endian header
fields, manual VLTR entries and direct BLAKE3 derive-key contexts. It does not use
production transfer encoders or ObjectHasher. Golden bytes cover all three domains
and both root kinds; a shared DAG checks exact name-ordered DFS postorder, count,
payload bytes and deduplication rather than merely accepting some topological
order. Opaque executable/certificate-shaped bytes retain content identity only.

Hostile packs include every truncation of a blob pack, tag/version/hash/length/
root changes, missing/duplicate/extra/reordered/foreign-domain/wrong-kind records,
and correctly rehashed malformed/case-colliding/path trees and shared-edge size
conflicts. Every validation/resource rejection is checked against both an absent
nested destination (no parent creation) and a complete existing store snapshot,
including bytes, modes, directories and derivation records.

Resource cases cover every default/HARD setting plus a sweep through validation
and later-publication work/metadata envelopes. A shared tree first visited
shallow and then reached deeper must not bypass depth limits. Valid transfers
exercise actual reuse, corruption repair, concurrent imports and independent
materialization with portable executable flags. A separate-process exporter writes
an actual pack file; a distinct importer receives that file's bytes through stdin
and materializes verified objects. The helper entrypoint is not an additional
semantic case when run without its explicit worker environment.

## Actual Linux execution

Runtime validation used the actual exporter45f34b4, exporter reexportbae12a9,
importer5b88772 and importer reexport6cd513f. Their dependency-only copies in this
worktree areba53d9e,7ae6020,10d5830,c16817d; these are not acceptance-owned changes
and must not be reimported as owned work. Initial owned commits aref8ae5fa and
837dde3. The shared scaffold alone is not cited as implemented transfer.

With exclusive `/tmp/caspack41-acceptance-target`, one Cargo job, debug0 and
incremental0:

- `cargo test --locked -p vesper-loom-cas --test transfer_acceptance` passes all
  15entries, zero failures/ignores. This includes the explicit child-process
  entrypoint; without its worker environment it is not an additional semantic
  case. All canonical/malformed/resource/snapshot/depth/size/concurrency/repair
  assertions execute against the actual implementation.
- `cargo test --locked -p vesper-loom-cas --all-targets` passes61tests across
  four result suites:12library/shared tests,34existing Store tests and15new
  acceptance entries, plus the zero-test example harness. Export/import lanes'
  separate new test files are not imported here; root validates those together.
- `cargo run --locked -p vesper-loom-cas --example transfer` executes its actual
  disposable round trip and verifies/materializes a229byte pack containing two
  immutable source objects, with explicit content-identity-only output.
- The built actual example was separately exercised with a242byte artifact
  closure through export-to-file/import-from-file and a direct OS stdout/stdin
  pipe between separate exporter/importer processes. Both resulting stores
  reexport identical bytes and pass actual API readback/materialization. A small
  temporary Rust fixture helper links the already-built CAS library only to seed
  and inspect those stores; no production, Cargo or dependency file changes.
- Strict all-target Clippy, Rustfmt and diff checks pass. One example-only error
  conversion was corrected to the existing CasError::InvalidTree wrapper;
  no production behavior, test expectation or resource cap was changed.

These results are actual local Linux evidence for the listed dependency heads.
They do not claim that later source follow-ups, hosted CI or another platform
have already passed. Root performs final combined source/review validation.

No network/build/program/certificate verification is performed, and no timing,
RSS, filesystem-wide rollback, permanent-integrity, hosted/platform or trust
claim is made. Root performs final combined integration/review after all lanes.
