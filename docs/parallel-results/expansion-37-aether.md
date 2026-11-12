# Expansion 37: Aether host memory and snapshot pages

Implemented `crates/vesper-aether` and `lean/AetherMemory.lean` after rereading
`expansion-37-contracts.md` and the refreshed full sections 35–37.

## Delivered behavior

`SnapshotMap::validate` builds immutable, bounded host metadata. Each slot has an
explicit VM identity, slot identity, GPA interval, HVA interval and permission.
Both intervals use checked u64 half-open endpoints, require nonempty aligned
ranges, and reject endpoint overflow. Duplicate slots and overlapping GPAs are
rejected within each VM; numerical GPAs may legitimately repeat in different
VMs. Overlapping HVAs are rejected across all VMs unless both entire intervals
and their explicit readonly CAS identities (domain/kind/digest, object length,
offset) match exactly. Partial readonly overlaps are conservatively rejected.

Every slot supplies exactly one explicit `Zero`, `ReadOnlyCas` or `PrivateCow`
entry per 4KiB page. Readonly pages must match the slot's complete sequential
identity. Private slots allow explicit zero/COW entries only. No optional bitmap
or absent mapping defaults to zero. Unknown VMs, GPA gaps and exclusive-end
faults fail. Fault byte addresses resolve their enclosing aligned page.

Construction checks slot/page/guest-byte/blob-byte/pair-work limits before
copying metadata. Hard ceilings are 4096 slots, 1,048,576 pages, 4GiB declared
guest bytes, 16MiB per backing blob, and 8,388,608 slot comparisons. Default
limits are smaller. Resolution scans at most the admitted slot count and reads
and hashes at most one admitted blob; output is exactly 4096 owned bytes.
There is no mutable map accessor, page handle import, mutation or page-application
API, so stale/foreign page handles cannot be applied against another map.

Actual CAS resolution accepts only artifact-domain blobs, validates aligned
page ranges and exact declared object length, opens a regular readonly object,
reads into one bounded owned buffer, recomputes the existing CAS's domain/kind
separated BLAKE3 identity from that same buffer, and copies the requested page
from that verified buffer. It never verifies one open and returns bytes from a
second open. Corrupt/missing/writable/nonregular/wrong-length objects fail.
Changing or replacing the file after resolution cannot change owned output.
Unix opens use `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`, and additionally reject
multiple hard links. Windows opens use `FILE_FLAG_OPEN_REPARSE_POINT`, reject
reparse-point metadata and permit only read sharing, excluding concurrent
writable/deletion handles. Other targets explicitly return UnsupportedPlatform
for CAS-backed resolution. Only Linux execution was available locally; Windows
cross-compilation is reported separately below.

`UffdPageAction` describes proposed zero/readonly-continue/private-copy actions.
A continue plan still needs future immutable memfd backing and UFFD registration;
a returned byte array is not a registered shared page. HVA checks establish no
host-physical, EPT or Stage-2 isolation. The crate performs no KVM/UFFD ioctls,
memfd mapping, native VM boot, compiler integration, cryptography, nonce/VMGENID
handling or boot/fork timing measurement. Readonly filesystem permissions do not
stop privileged mutation; same-buffer hashing plus owned output supplies the
content-integrity boundary, not a claim that the filesystem cannot change.

## Specification findings

Section 37.5's saturated endpoint arithmetic can hide overflow, does not audit
HVA aliases, and lets missing dirty bitmap words silently become zero pages.
The new API intentionally replaces that scaffold with validated complete
metadata. A sharing boolean is not immutable identity. Section 37.4's first
numeric disjointness result is conditional on supplied separation; it cannot
establish actual hardware isolation. Its second theorem assumes arbitrary HKDF
injectivity, which is not a valid general cryptographic theorem. No such axiom
or clone-safe nonce implementation is introduced here.

Sections 35–36 were reviewed, with their implementation paths left untouched.
Section 35's packet decoder has unchecked shift/input/range arithmetic; its
BBR/UMEM saturated counters do not establish exact arithmetic or hardware
conservation. Section 35.3's shown ingress path does not execute the claimed
cipher authentication. Section 36's same-color proof assumes its conclusion,
Morton masking aliases out-of-range coordinates, and its platform floating-point
and solver performance claims need execution evidence beyond the shown audit
and helper scaffolds. These remain outside this lane.

## Model proof scope

Pinned Lean 4.24.0 compiles seven kernel-checked theorems: accepted interval
facts, guarded UInt64 addition exactness, numerically separated intervals,
adjacent endpoints, complete containment of a fault's enclosing page, page-index
bounds for complete metadata, and page-slice containment in a blob. This is a
handwritten host numeric model, not an extraction/equivalence proof of Rust,
filesystem/hash correctness, physical memory isolation or crypto.

All theorem axiom reports contain only `propext` and `Quot.sound`, except
`fault_page_contained`, which also contains `Classical.choice`. No new axioms,
`sorry`, `native_decide` or `bv_decide` are used.

## Validation and integration

Local Linux checks passed:

- `cargo fmt --manifest-path crates/vesper-aether/Cargo.toml --check`
- `cargo test --manifest-path crates/vesper-aether/Cargo.toml --all-targets --offline`
  (14 integration tests, including real temporary CAS tests and small interval
  model enumeration; overflow, alias/sharing, missing maps, holes, corruption,
  wrong domain/kind/length, writable files, symlinks, hardlinks, directories/FIFO)
- `cargo clippy --manifest-path crates/vesper-aether/Cargo.toml --all-targets --offline -- -D warnings`
- `cargo run --manifest-path crates/vesper-aether/Cargo.toml --example snapshot_pages --offline`
  (self-checking readonly/COW CAS byte identity, explicit zero and invalid fault)
- `lean lean/AetherMemory.lean` under `leanprover/lean4:v4.24.0`
- `cargo check` and strict `cargo clippy`, both with `--all-targets --offline
  --target x86_64-pc-windows-gnu` (cross-compilation only, no Windows execution)

An attempted `--target aarch64-apple-darwin --all-targets` check was blocked in
existing blake3's C build before checking Aether: the host `cc` lacks Darwin
`-arch`/`-mmacosx-version-min` support. Native macOS/Windows tests and examples
remain coordinator CI evidence; no local native execution is claimed.

Builds use `/tmp/vesper-target-expansion37-aether`, one job, no development/test
debug info and disabled incremental caches. No leaf lockfile is committed.

Coordinator integration: add `crates/vesper-aether` to workspace members and
remove its temporary empty `[workspace]` table. Dependency on existing
`vesper-loom-cas` is path-only and read-only; Unix adds libc, examples/tests add
tempfile. No other new lane dependency is required. Stable example name is
`snapshot_pages`; invoke `cargo run -p vesper-aether --example snapshot_pages`
after workspace registration. Add `lean/AetherMemory.lean` to pinned proof
validation. Root manifests/lock/CI/CLI remain coordinator-owned.
