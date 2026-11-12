# Loom content-addressed store (design §8.2.6, §8.3)

Lane: Loom CAS. Branch: `claude/project-thread-w1hcem`, based on `origin/work` at `72493b8`.
Owned paths: `crates/vesper-loom-cas/**` and this file. No root workspace, lockfile, CLI, CI, compiler representation, or other crate is changed.

## What is implemented

`crates/vesper-loom-cas` is a standalone library (temporary `[workspace]` table) for an immutable, BLAKE3-addressed object store.

| Requirement | Implementation |
| --- | --- |
| Explicit domains | `Domain::{Source, Interface, Artifact}` and `ObjectKind::{Blob, Tree}`. Every hash uses BLAKE3 `derive_key` with a frozen per-(domain, kind) context such as `"vesper-loom-cas 2026-11 v1 source blob"`. Identical bytes get different ids in different domains, and a blob never shares an id with a tree. Domain and kind are part of the on-disk path. |
| Text ids | `b3v1:<domain>:<kind>:<64 lowercase hex>`. The parser is strict: no uppercase, no extra fields, no other scheme. Store paths are derived only from parsed digests, never from caller text. |
| Canonical trees | `Tree` encodes one directory (schema v1: magic, version, count, then sorted `(name, kind, digest, size)` entries). The decoder accepts only canonical encodings: strictly ascending names, known kinds, zero-size directories, no trailing bytes, and an entry-count limit. Children must share the tree's domain. |
| Portable naming policy | `validate_name` rejects empty names, `.`/`..`, `/` and `\`, `:<>"|?*`, NUL/control characters, trailing dot or space, Windows device names (`CON`, `nul.txt`, `COM1`, …), and names over 255 bytes. Names that differ only by case are rejected within a directory. The policy is enforced on ingestion, on tree construction and in the decoder, so a crafted tree from elsewhere is rejected before anything is written. |
| Versioned derivation inputs | `DerivationInputs` (schema `InputSchema::V1`): output domain (`Interface` or `Artifact`; `Source` is rejected because source is only ingested), builder, builder version, optional target, option map, and a nonempty named input map of `ObjectId`s. The canonical length-prefixed encoding has a strict decoder. `DerivationKey` = BLAKE3 `derive_key("vesper-loom-cas 2026-11 v1 derivation key", encoding)`. |
| Contract-audit cache keys | Implementation inputs are part of the key alongside the interface. A body-only change with an unchanged interface misses, and so does a different target (`body_change_with_same_interface_misses_the_cache`). Every field changes the key (`every_field_changes_the_key`). |
| Verified ingestion | `put_blob`, `put_blob_reader`, `put_file`, `put_tree`, and `ingest_dir` hash while writing to a private staging file. `put_blob_expected` refuses contents that do not match a caller-supplied id, and in that case nothing is published. `put_tree` requires every child to be present with matching kind and size. |
| Atomic publish | Staging file → `fsync` → read-only mode → no-replace publication → parent directory `fsync`. On Linux publication uses `renameat2(RENAME_NOREPLACE)`. Elsewhere, or on file systems without it, it falls back to `link(2)` then unlink, and only after that to check-then-rename (see limitations). An object therefore appears complete or not at all. |
| Verified readback | `read_blob`, `read_tree` and `verify_object` always rehash. Object files are opened with `O_NOFOLLOW` and must be regular files, so a symlink planted at an object path is refused rather than followed. |
| Corruption rejection | Bit flips, truncation and symlink substitution produce `Corrupt` or `UnsafeStoreLayout` errors. When the true contents are ingested again, the bad file is moved to `quarantine/` and the object is republished. `verify_all` rehashes every object, decodes every tree, revalidates every derivation record, and reports stray files. |
| Concurrent writers | Any number of `Store` handles, threads and processes can share a root. Duplicate writers deduplicate: exactly one publishes and the rest verify the existing object. Derivation records are never replaced: the same mapping is idempotent, and a different output for an existing key returns `DerivationConflict` naming both outputs. |
| Interrupted writes | Reader errors and panics remove the staging file through a drop guard. A writer process killed by SIGKILL leaves only an unaddressable file in `tmp/`. `sweep_temporaries(min_age)` removes such files. |
| Safe materialization | `materialize(tree, dest, options)` requires `dest` not to exist, including as a dangling symlink. It builds the tree in a uniquely named sibling staging directory and publishes it with a no-replace rename, so a corrupt or invalid tree leaves no partial workspace. Files are created with `O_EXCL|O_NOFOLLOW`. Workspace files are never hard links or symlinks into the store, so editing a workspace cannot change a stored object. Modes are 0644, or 0755 for executable entries. |
| Reflink with copy fallback | `CopyStrategy::ReflinkOrCopy` (the default) tries `reflink-copy` (FICLONE / clonefile) and falls back to a streaming copy that is hashed as it is written. `CopyOnly` never clones. `ReflinkOnly` fails cleanly with `ReflinkUnavailable`. By default, cloned files are rehashed (`verify_reflinks`). |
| Ingestion of hostile inputs | `ingest_dir` and `put_file` reject symlinks (to files, to directories, dangling, and a symlinked root), FIFOs, sockets and devices, and non-UTF-8 or non-portable names. A nesting limit (`MAX_TREE_DEPTH` = 128) bounds recursion. Hard-linked inputs are read as ordinary files and their contents are copied, never linked. |
| Store layout safety | `Store::open` requires `objects/`, `derivations/`, `tmp/` and `quarantine/` to be plain directories (not symlinks). It writes or validates a `LAYOUT` marker and refuses unknown layout versions. |

## Public API

```rust
pub const OBJECT_SCHEMA_VERSION: u32 = 1;
pub const TREE_SCHEMA_VERSION: u32 = 1;
pub const MAX_TREE_ENTRIES: u64; pub const MAX_TREE_DEPTH: usize;

pub enum Domain { Source, Interface, Artifact }
pub enum ObjectKind { Blob, Tree }
pub struct Digest(pub [u8; 32]);           // to_hex, from_hex (strict)
pub struct ObjectId { pub domain, pub kind, pub digest }   // Display/FromStr "b3v1:…"
pub struct ObjectHasher;                    // new(domain, kind), update, len, finish
pub fn blob_id(Domain, &[u8]) -> ObjectId;

pub fn validate_name(&str) -> Result<(), NameError>;
pub enum EntryKind { File { executable: bool }, Directory }
pub struct TreeEntry { pub kind, pub digest, pub size }
pub struct Tree;  // new(domain), insert_file, insert_dir, get, entries, child_id,
                  // encode, decode(domain, &[u8]), id

pub enum InputSchema { V1 }
pub struct DerivationInputs { pub schema, pub output_domain, pub builder, pub builder_version,
                              pub target: Option<String>, pub options: BTreeMap<String, String>,
                              pub inputs: BTreeMap<String, ObjectId> }
  // new, with_target, with_option, with_input, validate, encode, decode, key
pub struct DerivationKey(pub Digest);       // Display "drv:b3v1:…"

pub struct Store;
impl Store {
    fn open(root) -> Result<Store>;  fn root(&self) -> &Path;
    fn object_path(&self, &ObjectId) -> PathBuf;  fn contains(&self, &ObjectId) -> Result<bool>;
    fn put_blob(&self, Domain, &[u8]) -> Result<Ingested>;
    fn put_blob_reader(&self, Domain, impl Read) -> Result<Ingested>;
    fn put_blob_expected(&self, ObjectId, impl Read) -> Result<Ingested>;
    fn put_file(&self, Domain, path) -> Result<Ingested>;
    fn put_tree(&self, &Tree) -> Result<Ingested>;
    fn ingest_dir(&self, Domain, path) -> Result<Ingested>;
    fn read_blob(&self, &ObjectId) -> Result<Vec<u8>>;
    fn read_tree(&self, &ObjectId) -> Result<Tree>;
    fn verify_object(&self, &ObjectId) -> Result<u64>;
    fn publish_derivation(&self, &DerivationInputs, ObjectId) -> Result<DerivationOutcome>;
    fn lookup_derivation(&self, &DerivationInputs) -> Result<Option<ObjectId>>;
    fn materialize(&self, &ObjectId, dest, MaterializeOptions) -> Result<MaterializeReport>;
    fn sweep_temporaries(&self, Duration) -> Result<usize>;
    fn verify_all(&self) -> Result<StoreReport>;
}
pub struct Ingested { pub id, pub size, pub newly_stored }
pub struct DerivationOutcome { pub key, pub output, pub newly_recorded }
pub enum CopyStrategy { ReflinkOrCopy, CopyOnly, ReflinkOnly }
pub struct MaterializeOptions { pub strategy, pub verify_reflinks }   // Default: ReflinkOrCopy, true
pub struct MaterializeReport { pub directories, pub files, pub bytes, pub reflinked, pub copied }
pub struct StoreReport { pub verified_objects, pub corrupt_objects, pub verified_derivations,
                         pub corrupt_derivations, pub stray_paths }   // is_clean()
pub enum CasError { Io, Corrupt, DigestMismatch, SizeMismatch, NotFound, WrongKind, DomainMismatch,
                    InvalidId, InvalidName, InvalidTree(TreeError), InvalidDerivation(DerivationError),
                    DerivationConflict, CorruptDerivationRecord, UnsupportedFileType,
                    DestinationExists, UnsafeStoreLayout, ReflinkUnavailable, PublishContention }
pub enum NameError { … }  pub enum TreeError { … }  pub enum DerivationError { … }
```

On-disk layout (v1): `LAYOUT`, `objects/v1/<domain>/<kind>/<aa>/<hex>`, `derivations/v1/<aa>/<hex>`, `tmp/`, `quarantine/`. A derivation record is `"VLDR" | u32 1 | 34-byte output id | canonical input encoding`. On read, the stored inputs must decode strictly and hash to the record's key.

## Validation

Run from `crates/vesper-loom-cas` with Rust 1.97.0:

```sh
cargo fmt -- --check                              # clean
cargo clippy --all-targets -- -D warnings         # clean
cargo test                                        # 9 unit + 31 integration + 1 doctest passed
cargo doc --no-deps                               # clean
```

The default host temp directory is on ext4, which does not support FICLONE (`EOPNOTSUPP`), so that run exercised the copy fallback and the clean `ReflinkUnavailable` path. To exercise real clones, the suite was also run on an XFS loop mount with `reflink=1`:

```sh
TMPDIR=/mnt/casxfs VESPER_CAS_EXPECT_REFLINK=1 cargo test   # 9 + 31 + 1 passed
```

With `VESPER_CAS_EXPECT_REFLINK` set, the tests require every file in `ReflinkOrCopy` and `ReflinkOnly` to have been cloned. That includes the corruption test: a clone of a corrupted object is caught by the post-clone rehash. The three concurrency tests (threads, processes, and derivation races) were also run 15 times in a row without failure. That is evidence against flakiness on this host, not a proof of race freedom.

The golden vectors in `golden_hashes_are_stable` (one blob per domain, a two-entry tree, and a derivation key) were reproduced independently with the Python `blake3` package from the documented encodings.

| Test group | Tests |
| --- | --- |
| Determinism | golden vectors, domain/kind separation, strict id text form, order-independent derivation keys, every derivation field changes the key |
| Ingestion/readback | round trip, dedupe, read-only objects, expected-digest mismatch, missing children/size mismatch in `put_tree`, cross-domain children refused |
| Interrupted writes | failing reader, panicking reader, simulated crash residue and sweep, **real SIGKILL of a writer process mid-stream** |
| Corruption | bit flip (read, verify, `verify_all`, quarantine + repair), truncation, symlink substitution at an object path, corrupt and dangling derivation records, stray files |
| Concurrency | 16 threads × duplicate and distinct writes; 6 processes × 40 writes; 12 threads racing to record two different outputs for one key (exactly one wins, the rest get `DerivationConflict`) |
| Adversarial paths | `..`, `.`, `../escape`, `/abs`, `a/../../escape`, `..\escape`, `C:escape`, NUL, empty, invalid UTF-8, `CON`, unknown entry kind, duplicate names, case collision, lying size, all in crafted tree objects; symlink/dir-symlink/dangling-symlink/FIFO/symlinked-root ingestion; non-portable names; depth limit; existing/dangling-symlink destinations; symlinked store internals; unknown layout marker |
| Materialization | round trip with exec bits, empty files and directories under both strategies; independence from the store (inode and nlink checks, workspace edits); hard-linked inputs materialize as independent files; corruption leaves no partial workspace; `ReflinkOnly` clones or fails cleanly |

## Integration requirements

1. Register `crates/vesper-loom-cas` in the root `members` and `default-members`. Remove its temporary `[workspace]` table and its `.gitignore` (which ignores the nested `target/` and `Cargo.lock`), then regenerate the root lockfile.
2. New dependencies (versions resolved in the standalone lock): `blake3 = "1.8"` (1.8.7), `reflink-copy = "0.1"` (0.1.32), `libc = "0.2"` on unix (0.2.191, the same as the root lock), and dev-only `tempfile = "3"` (3.27.0). The illustrative `blake3 = "1.6"` scaffold pin should not be used. `redb` is not used (see the decisions below).
3. Producers of `Interface`/`Artifact` objects (SemVer `PublicInterfaceV1`, native `AbiLayoutV1`, Lean `ProofArtifactV1`) should put their versioned schema identities into `DerivationInputs.options` (for example `abi = "AbiLayoutV1"`) and their canonical bytes into the store. This crate does not define those encodings.
4. The `vesper` CLI, `Loom.lock` writer, and `vesper-loom-trust` are separate milestones. `Loom.lock`'s `source_blake3` can be the `Source` tree id from `ingest_dir`.

## Ambiguities and proposed decisions

| Topic | Design text | Decision taken here |
| --- | --- | --- |
| Metadata index | §8.4 and §10 line 1156 name `redb` tables (`DERIVATIONS`, `MIF_CACHE`, `OLEAN_CERTS`, `LILA_QUORUM`). | Records are individual immutable files published with no-replace renames. This gives multi-process safety without a single-writer database lock. `redb` can be added later as a rebuildable index over these files. `LILA_QUORUM` belongs to the trust verifier, not here. |
| `MIF_CACHE` mapping interface MIF → object | §10 line 1158 maps an interface hash directly to a `.o`. | Not implemented as stated. The contract audit shows that this mapping returns stale code after body-only fixes. Artifact lookups use full derivation keys that include implementation inputs. Interface MIF stays an input used for early-cutoff decisions by the caller. |
| `BLAKE3(derivation_inputs)` | §8.2.6 gives no encoding. | Explicit schema-v1 binary encoding with sorted maps, a strict decoder, and a dedicated derive-key context. Unknown schemas are rejected rather than guessed. |
| Domains | The design lists source trees, `.vhir`, egglog rules, `.o` and `.olean`. | Three hash domains: source, interface (`.vhir`, MIF payloads), and artifact (everything derived, including `.olean` and rule sets). Finer artifact types belong in derivation options and builder ids, not in new hash domains. |
| Symlinks in packages | Not specified. | Tree schema v1 has no symlink, hardlink or special-file entries, and ingestion rejects them. A future schema could add relative, in-tree symlinks with an explicit policy. |
| Portable names | Not specified. | Names are restricted to the intersection of Linux, macOS and Windows rules, with case-collision rejection. This can reject some Linux-legal package files (for example `a:b`). |
| Reflink | "instantaneous" materialization via reflinks. | Reflink is an optimization only. Copies are always verified. Cloned files are rehashed by default, so materialization is O(bytes) unless `verify_reflinks` is disabled. Nothing here claims the "instantaneous" performance target. |

## Limitations

- **Trust:** A matching digest proves content identity only. No Sigstore, Rekor, TUF, OIDC namespace, Lila quorum, or `.olean` validity check exists here, and a derivation record is a local builder's claim, not an attestation.
- **Scope:** There is no registry, substituter, HTTP/3 or zstd client, and no build or sandbox execution. No garbage collection or reference tracking beyond `sweep_temporaries`, and no `redb` index.
- **Threat model:** The store root is assumed to be writable only by its owner. The store defends against corruption, crashes, hostile input trees and paths, and pre-existing destinations. It does not defend against a concurrent local attacker who can write inside the store or inside a materialization parent directory while an operation runs (for example, swapping a just-created staging subdirectory for a symlink). Defending against that would need `openat`/`O_NOFOLLOW` traversal (`cap-std`-style).
- **Unicode:** Case folding uses `to_lowercase`. Unicode normalization collisions (for example NFC vs NFD on APFS) are not detected.
- **Platforms:** Only Linux (ext4, plus XFS with reflink) was tested. On non-Linux unix, publication uses `link`+`unlink`. On Windows, and on file systems with neither `RENAME_NOREPLACE` nor hard links, it falls back to check-then-rename, which can replace an identical concurrently published object. Workspace publication on Windows and macOS is untested, and Windows executable bits are not represented.
- **Durability:** Objects and records are fsynced before publish and parent directories after. Materialized workspace files are not fsynced individually.
- **Memory:** `read_blob` and `read_tree` load the whole object into memory. Streaming verification is used for ingestion, copying and `verify_object`.
- **Evidence:** The concurrency claims rest on the tests above, not on a formal argument about every interleaving.
