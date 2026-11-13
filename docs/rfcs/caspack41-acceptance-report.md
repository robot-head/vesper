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

Initial status: Rustfmt and diff checks pass. Actual compilation/runtime remains
pending the real export
and import implementations plus their public reexports. The shared scaffold alone
does not implement transfer and is not cited as runtime success. Source commits
will be imported only as dependency copies, kept distinct from this lane's work.

No network/build/program/certificate verification is performed, and no timing,
RSS, filesystem-wide rollback, permanent-integrity, hosted/platform or trust
claim is made. Root performs final combined integration/review after all lanes.
