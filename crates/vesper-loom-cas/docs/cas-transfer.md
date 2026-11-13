# Portable verified CAS closure packs

`transfer::export_pack` verifies the reachable immutable closure of an explicit
ObjectId and returns its deterministic bytes. `transfer::import_pack` validates
the entire pack against a caller-provided expected root before opening or
initializing its explicit destination. No network, build or program executes.

The frozen version1 wire format is little endian:

```
VSPACK01 | u32 version1 | u32 object-schema1 | root ObjectId34 | u32 count
count * (ObjectId34 | u64 payload-byte-length | exact payload)
```

An ObjectId is its domain byte, kind byte and32digest bytes, using the existing
schema1 domain/kind-separated BLAKE3 contexts. Source, interface and artifact
domains, and blob/tree kinds, remain distinct even for identical payload bytes.
Trees retain their existing strict portable names, sorting, flags and sizes.

Records follow deterministic DFS postorder in canonical tree-name order. Shared
objects appear once at their first completed visit, and the root is last. This
is stricter than arbitrary topological order. Missing, duplicate, extra, reordered
or trailing records are invalid. Every object is actually rehashed; filesystem
presence, recorded sizes and pack-header claims are insufficient. Every file
edge's declared size is checked, including repeated references to a shared blob.

```rust
use vesper_loom_cas::{ObjectId, Store};
use vesper_loom_cas::transfer::{TransferLimits, export_pack, import_pack};

fn move_closure(source: &Store, expected: ObjectId, destination: &std::path::Path)
    -> Result<(), Box<dyn std::error::Error>>
{
    let bytes = export_pack(source, expected, TransferLimits::default())?;
    let report = import_pack(destination, expected, &bytes, TransferLimits::default())?;
    assert_eq!(report.root, expected);
    assert_eq!(report.inserted + report.reused, report.objects);
    Ok(())
}
```

Keep the expected root independently of the incoming header. A digest identifies
bytes; it is not a signature, provenance, builder-quorum, TUF or Lean certificate
validation result. Certificate-shaped artifact payloads remain opaque. Derivation
records and mutable trust/root references are not transferred.

## Actual file and pipe example

Run the disposable self-check:

```sh
cargo run -p vesper-loom-cas --example transfer
```

The same example accepts explicit paths and caller-known ObjectIds. Build it,
then use the actual executable for file or pipe transport:

```sh
cargo build -p vesper-loom-cas --example transfer
target/debug/examples/transfer export SOURCE_STORE EXPECTED_ROOT > closure.pack
target/debug/examples/transfer import DESTINATION_STORE EXPECTED_ROOT < closure.pack
```

Here EXPECTED_ROOT is an existing strict `b3v1:domain:kind:hex` identifier, not a
literal placeholder the program resolves. Windows uses `transfer.exe`. Export
writes only verified pack bytes to stdout; import reads a bounded stdin buffer
and writes a diagnostic to stderr. The library itself is transport-neutral.

Imported roots are available through existing `read_blob`, `read_tree`,
`verify_object` and `materialize`. Materialization into a new directory uses the
existing verified-copy/reflink discipline and preserves executable flags where
supported. Workspace copies are independent of stored bytes and each other;
none of these APIs automatically executes an imported artifact.

## Resource and failure boundaries

TransferLimits defaults/HARD are pack bytes64/256MiB, objects4096/16384,
depth64/128, aggregate tree entries16384/65536, metadata16/64MiB and work512Mi/2Gi.
Every value must be positive and within its hard ceiling. Limits are logical
allocation/copy/hash/graph envelopes, not timing or process-RSS guarantees.

All validation/resource errors precede destination initialization/publication.
The importer prepays later tree encoding, staging, path and bounded occupied-
object verification resources before opening the store. Existing fixed Store
buffers are charged cumulatively (up to nine64KiB buffer envelopes per record),
so many tiny objects can exhaust metadata below the independent object cap.
Individual maxima do not guarantee that every combination fits all other caps.

After validation, children are published before parents. Valid existing objects
can be reused; corrupt ones can be repaired through the existing quarantine and
atomic no-replace discipline. Reported insertion/reuse counts describe actual
publication, not validation of a trust claim. OS/store errors may leave verified
immutable objects; no filesystem-wide rollback, lock-wait deadline or immunity
to later out-of-band mutation is promised.

This is a prerequisite for distribution, not HTTP3/zstd transport, registry
authentication, signing, admission, garbage collection, source effect confinement
or formal proof of the Rust implementation.
