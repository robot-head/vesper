# Verified portable CAS export (claim197)

Base aff02c4, branch `caspack41/export`. Production source commit45f34b4 owns only
`crates/vesper-loom-cas/src/transfer/export.rs` and the explicitly approved
`store.rs` helpers/publication-size propagation. Coordinator's actual exporter
reexport bae12a9 was imported as dependency18d9dd2; it is not an owned source
change. No Cargo, build, workflow, compiler, CLI or other reserved paths changed.

`export_pack` returns one complete canonical version1 pack only after verifying
every reachable object from the same opened regular file whose bytes are hashed
and retained. The bounded helper prepays relative path/scratch work and storage,
inspects actual handle length, fallibly reserves exactly that length, and reads
at most that length plus one probe byte with64KiB stack scratch. It refuses
growth/shrink or expected identity mismatch and never uses unbounded
`read_to_end`, a separately reopened verified path, or a maximum-sized payload
allocation for tiny objects. Export performs no source writes or corruption
repair.

Traversal follows strict decoded tree name order and emits each identity once
at its first completed visit. File sizes are checked against retained verified
bytes. Active/completed scans, edges, geometric metadata requests, tree decode,
payload capacities and final pack copying receive finite cumulative charges.
Retained subtree heights check deeper later paths through shared DAG nodes;
deduplication cannot suppress the full depth check. All domains and both object
kinds retain the existing schema and domain-separated BLAKE3 identities.

The pre-writer publication amendment passes the actual staged hashed size to
both ordinary and locked occupied-object checks. Each uses one nofollow-open
regular handle, immediately rejects metadata size mismatch, and hashes at most
staged size plus one byte. Four publication attempts retain the existing
quarantine/repair/recheck discipline. Public read/verify APIs are unchanged.

Validation on Linux, one Cargo job, debug0/incremental0, exclusive cache
`/tmp/caspack41-export-target`:

- `cargo test -p vesper-loom-cas --all-targets`:56tests pass, zero ignored;
  12unit,34existing store integration,10new export integration.
- `cargo clippy -p vesper-loom-cas --all-targets -- -D warnings`:pass.
- `cargo fmt --all --check` and `git diff --check`:pass.

New actual tests cover independent single-blob wire construction in every
domain; empty blobs/trees; deterministic postorder/shared subtree deduplication
and executable tree bytes; deeper later DAG paths; each configured limit and
exact pack-byte boundary; missing/symlink/corrupt sources; unchanged source
quarantine/staging on export rejection; valid-digest malformed trees and wrong
file sizes; oversized sparse source rejection before payload reserve/hash;
and repairing512MiB sparse, shortened and same-length corrupt occupied objects
while retaining valid deduplication. Old store tests also exercise concurrent
publication and repair.

Logs: `/tmp/caspack41-export-tests-final.log` and
`/tmp/caspack41-export-clippy.log`. No hosted CI, Windows/macOS runtime, network
protocol, provenance, trust, Lean validity, GC, lock-wait or whole-process RSS
guarantee is claimed. No push, PR or merge performed by this lane.
