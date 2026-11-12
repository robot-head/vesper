# Bounded offline TUF target verifier

Implemented in `crates/vesper-loom-tuf`; isolated branch `coordinator/tuf-targets`. Only the new crate and this report are owned by this lane. Root workspace registration, lockfile, CLI, persistence, namespace policy, and secure per-step state advancement remain coordinator work.

## API

`async verify_target(LocalRequest) -> Result<VerifiedUpdate, TufError>`.

`LocalRequest` contains configured `repository_id: String`, independently trusted `bootstrap_root: Vec<u8>`, `metadata_dir`, `targets_dir`, canonical relative `target_name`, `previous: Option<Checkpoint>`, and `Limits`.

Successful `VerifiedUpdate` owns the fully verified `bytes`, exact `length`, `sha256`, target name, signed but semantically opaque `custom` metadata, `MetadataVersions { root, timestamp, snapshot, roles }`, `verified_at`, and next `Checkpoint`.

`Checkpoint` has private fields and no public Serde deserialization; use bounded `from_json(bytes, &limits)` and `to_json()`. `root_version()` is an inspection method, not verification of a decoded checkpoint. The checkpoint retains a bootstrap fingerprint/repository identity, entire consecutive authenticated root chain, last successful timestamp/snapshot/all snapshot-role floors, verification time, and an unkeyed corruption checksum. The checksum uses canonical JSON independent of Serde map-order feature unification. Protect this state externally against tampering/deletion/rewind.

## Actual verification

Uses maintained Tough 0.24.0 with `default-features = false`, `FilesystemTransport`, and unconditional `ExpirationEnforcement::Safe`. Private bounded directory snapshots isolate metadata and target reading. No HTTP capability is enabled. Tough performs real root old/new signature-threshold continuity, version/metadata/hash verification, delegated role/path/termination checks, and SHA-256 target verification. Complete target stream success and exact signed target length are required before returning bytes. Post-read checks reject detected time regression and expiry across every top-level and loaded delegated role.

Additional bounds cover perfile/total bytes, combined entries including empty directories, directory/delegation/JSON depths, metadata collection/node counts, delegation graph work/cycles, root update/retention count, and serialized checkpoints. Symlinks/reparse points, nonregular files, noncanonical names, and Windows reserved/device components are rejected. Leaf nofollow opens are platform-specific; source ancestor replacement during snapshot construction remains an explicit caller-controlled filesystem assumption.

## Security limitations and separate milestones

**This is not a complete stateful TUF updater.** Failed verification returns no checkpoint advancement. If newer authenticated metadata is followed by any later failure, older still-valid metadata may be accepted again using the previous successful checkpoint. This is a rollback/freeze security limitation independent of crash durability. `failed_target_leaves_older_checkpoint_usable_despite_newer_authenticated_metadata` demonstrates distinct-key root rotation and all metadata advancing, then a target failure, then acceptance of old metadata using the old state. Coordinator follow-up must capture and persist each authenticated step even on later failure. An atomic successful-checkpoint file alone does not fix it.

The crate writes no persistent state; checkpoints bind only successful operations and require caller serialization and persistence before byte use. Stock Tough datastore is temporary here; its non-atomic writes, ignored malformed cached metadata, and lack of automatic latest-root selection are not relied on for persistent rollback protection. Latest root is selected from the authenticated checkpoint chain instead.

This milestone does not authenticate a Loom package identity or registry namespace, OIDC/Sigstore/transparency evidence, a source tree, builder trust, hermeticity, cache eligibility, ABI contracts, or vulnerabilities. Configured root bytes and repository identity are caller trust inputs. Signed custom metadata requires a separate typed application policy. README details conservative compatibility limits, root recovery/floor policy, supported platform assumptions, and real-host-clock dependence.

## Validation

On Linux, with Rust 1.99.1, jobs=2, dev debug info disabled, and the existing scratch target cache:

```
cargo fmt --manifest-path crates/vesper-loom-tuf/Cargo.toml -- --check
CARGO_TARGET_DIR=/tmp/vesper-tuf-feasibility/target cargo test --manifest-path crates/vesper-loom-tuf/Cargo.toml --locked --jobs 2
CARGO_TARGET_DIR=/tmp/vesper-tuf-feasibility/target cargo clippy --manifest-path crates/vesper-loom-tuf/Cargo.toml --all-targets --locked --jobs 2 -- -D warnings
```

Passed: **14 tests** (13 integration, 1 focused post-load delegation expiry regression), plus **1 compiled README example**, no failures/ignores; strict Clippy and formatting. Fixtures generate actual Ed25519-signed metadata with fixed public test seeds, including distinct old/new root keys; no copied private key artifacts.

Coverage includes target ownership/corruption/truncation/oversize/exact-length mismatch, every metadata-role expiry, signature/snapshot hash failures, delegation scope/graph/limits, root rotation surviving absent old root files, successful-operation timestamp/snapshot/targets/delegated rollback floors, corrupt/mismatched/forged root-chain checkpoint rejection, future clock sample, path/device/symlink/FIFO rejection, and the failed-update security limitation.

No Windows/macOS runtime tests, hosted CI success, production registry integration, or full updater/provenance guarantee is claimed. AWS-LC/rustls dependencies remain unconditional even without HTTP and require native crypto build support. Temporary nested workspace/profile exist only to permit isolated validation; coordinator should remove them when registering the crate, then resolve its own root lockfile. The scratch-generated nested lockfile is intentionally not part of the commit.

## Coordinator integration

Registered the crate in the shared workspace and removed its standalone workspace
and profile. Independent review found a numeric delegated-role preflight bypass;
the integrated fix preserves both raw role names and possible versioned aliases.
No target authentication bypass was found in that review. A separate failure-progress
API is still being implemented; the successful-operation API retains its stated limitation.

The Unix `state::CheckpointStore` now provides explicit initialization, bounded
checkpoint reads, OS locking across live processes, same-directory atomic replacement
with file/directory sync, failed-write poisoning and bounded dead-writer staging cleanup.
State paths bind to an absolute location before locking; a subprocess regression
changes working directory while two stores are locked and verifies correct isolation.
This does not make the verifier's internal steps durable or protect against rewriting
by the directory owner. Store serialization size limits are wire limits, not strict
transient memory limits.

Integrated validation with Prism: **801 workspace tests/doctests across 81 suites**,
zero failures or ignores, with the pinned Lean toolchain available. Formatting,
strict workspace Clippy and focused post-review Clippy passed. Prism separately
passed Windows GNU compilation; TUF's native crypto has not been cross-validated here.

Review follow-up: preflight graph edges now retain both raw decoded role names and possible version-stripped aliases. This closes a resource-bound bypass for non-consistent delegated role names such as `1.a`. Added regression for `targets -> 1.a -> 2.b -> 3.c` under depth 1 and a numeric-role self cycle; both reject in preflight.
