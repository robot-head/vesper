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
No target authentication bypass was found in that review. The integrated phase-two
failure-progress API is described below; the legacy successful-operation wrapper
retains its stated limitation.

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

## Phase 2: retain completed-call authentication progress

New primary API: `verify_target_attempt(request) -> VerificationAttempt { checkpoint: Option<Checkpoint>, result: Result<VerifiedUpdate, TufError> }`. Stateful callers must commit a returned checkpoint even when `result` is an error. `verify_target` remains a compatibility wrapper that drops error progress; its original failed-update security regression intentionally remains to demonstrate the limitation.

Tough writes each accepted root/timestamp/snapshot into its private datastore before attempting later work. On load success or failure, the core reads only these private accepted-step files, reauthenticates consecutive root continuity with Tough old/new threshold verification, rechecks timestamp and snapshot signatures/pins, and promotes monotonic floors incrementally. Root advancement survives a bad later timestamp. A valid timestamp immediately establishes the authenticated snapshot version floor even if its body fails. A valid snapshot establishes every declared role version floor even if a delegated metadata or target download later fails. No error result exposes target bytes.

Checkpoint schema 2 permits root-only zero floors and partial progress. Complete schema-1 checkpoints remain accepted. Existing identity/bootstrap/root-chain validation is preserved, no previous floor resets on root key rotation, and invalid previous state produces no replacement checkpoint. Source snapshot hashes are checked against the original bounded private input bytes rather than reserialized datastore bytes. Private generated metadata reads allow bounded serialization overhead while input-file ceilings remain strict. Monotonic host clock observations are retained too; clock errors do not discard independently authenticated root progress.

Before Tough runs, a conservative upper-bound checkpoint is sized from every prospective root and snapshot-role name in the immutable source snapshot. This reserves room for any normal accepted prefix and rejects an insufficient return budget before remote root authentication. Existing bounded graph and input snapshot checks still apply. This policy can reject extra unused archive metadata conservatively.

This improves failure/retry rollback protection **when the complete call returns and its checkpoint is persisted**. It is not a durable per-step TUF updater. Cancellation, process termination, crash, extraction/IO failure, or ignored checkpoint output can still lose advancement; an atomic checkpoint committed only at the end does not close those gaps. The coordinator owns further persistence/callback integration. No package/namespace/provenance authentication claim is added.

The reviewed temporary-path issue is now an explicit supported-environment check: Tough FilesystemTransport preserves URL escapes, so private temporary ancestry must round-trip without escaping. Spaces, percent signs, and non-ASCII private temp paths return a clear unsupported-temporary-path diagnostic before Tough starts. Regression covers these paths; original source directory paths containing the same characters still verify through the ASCII snapshot. No HTTP/custom transport was introduced.

Validation with coordinator `src/state.rs` copied locally from main `ecfb8d2` (not committed by this lane) and its existing Unix module declaration temporarily enabled: **30 tests** passed (23 core/integration tests owned here plus 7 coordinator store tests), plus **1 compiled README example**; formatting and strict all-target Clippy passed. New signed-fixture tests cover root-before-invalid-timestamp, timestamp/pinned-snapshot-before-invalid-snapshot, snapshot-role floors before invalid delegation/target, invalid-state nonpromotion, legacy schema migration, preauthentication checkpoint-budget reservation, original source paths with unusual characters, and OS-backed store commit-on-error/reopen/rollback followed by successful target persistence and reopen.

Only `src/lib.rs`, `tests/offline.rs`, `README.md`, and this report belong to the phase-2 commit. `src/state.rs`, root manifests/lockfiles, and the coordinator's Unix module declaration remain coordinator-owned. Integration tests require that already-merged coordinator module.

Coordinator reran phase two against the hardened store: 30 unit/integration tests
and the README example passed, including commit-on-error/reopen/rollback.
Strict workspace Clippy also passed with Forge registered. The exact metadata pin
length and file-transport target-name follow-up is recorded below.

## Exact metadata lengths and supported target names

Confirmed with independent real signed fixtures that Tough 0.24 accepts a declared metadata length larger than its actual bytes when the actual hash/signatures match. Added independent exact checks for timestamp→snapshot, snapshot→top-level targets, and snapshot→every loaded delegated targets role. They use original immutable staged bytes and Tough's filename escaping/version-prefix conventions, before opening the verified target stream or returning bytes. Optional absent metadata lengths remain optional.

Timestamp→snapshot mismatch is rejected during harvest after retaining the authenticated timestamp and snapshot-version pin; prior snapshot-role floors remain. Snapshot→top/delegated mismatch is rejected after accepted snapshot-role floors have been retained. Thus error checkpoints still reject later older metadata, and correcting the lengths at the same authenticated versions permits successful verification. Tests prove this behavior for both non-consistent and consistent snapshots, including successful target bytes from raw Tough followed by rejection from the stricter core.

Also confirmed the Unicode target leaf limitation directly: raw Tough authenticates `pkg/é.bin` metadata but fails to open the ordinary Unicode leaf because its file transport preserves `%C3%A9`. Requested target paths now explicitly support only ASCII letters/digits and `/._~+@-`, retaining canonical-component/device-name rules. Unicode, interior spaces, and other escaped target characters fail with a clear supported-ASCII diagnostic. A signed `pkg/@example+v1~.bin` fixture succeeds. Unicode/space/percent source directory paths remain supported through private snapshotting.

Focused validation with the existing coordinator-state copy: **32 tests** (25 owned core/integration plus 7 coordinator store tests), one compiled README example, formatting and strict all-target Clippy. No transport expansion, package authentication claim, or per-step durable-updater expansion is included; state.rs/module registration remain coordinator-owned.

## Optional Unix synced-fetch gate and owned worker

Added `updater::start_update(CheckpointStore, UpdateRequest) -> Result<UpdateHandle, StartError>`. `UpdateRequest` excludes previous checkpoint overrides; the worker loads protected state from its owned locked store. `UpdateHandle::wait(self)` blocks and returns bytes only after final state commit; dropping detaches rather than cancels the worker. Admission is capped at two workers per process and two filesystem blocking threads per worker runtime. Rejected admission/OS thread start returns the still-owned store in `StartError`, including a fresh store without its first checkpoint.

The private gated FilesystemTransport harvests and reauthenticates accepted prefix state and commits it synchronously before every subsequent Tough backend fetch. A harvest may promote a valid root/timestamp then fail later; the valid prefix is still committed before latching the failure. Latches block all later fetches and force outer failure, including Tough's root-loader path that swallows arbitrary fetch errors as a missing next root. Normal success/error finalization commits again; no in-memory checkpoint is reported as persisted after a failed sync.

Security review identified a stale cloned-progress hazard; it is fixed before handoff. Gate progress is shared and merged into the outer verifier before final harvest and target-read error paths. Final target-read completion reharvests metadata. A separate sink defense compares candidates with the last actually committed checkpoint (seeded from the owned store) and refuses any decrease in root chain, identity, version/role floors, or host time. Remembered committed state changes only after a successful store commit. Deterministic faults replace private root with a directory or replace/remove the private datastore after root 2 has synced; error finalization retains durable root 2 and its time floor. Direct stale final root/time candidates cannot rewind disk.

This provides synced checkpoint gates at fetch boundaries and handle-drop-safe ordinary completion, not a complete crash-safe TUF updater. Process termination, panic, allocation/IO failure before recovery/commit, full root recovery/version reset, and hostile same-user state replacement remain outside the claim. Snapshot preparation precedes gated Tough reads and is bounded local input capture independent of authenticated metadata. Invalid input before any checkpoint can leave explicitly created state incomplete; missing state is never silently reinitialized. A final persistence failure yields no bytes and poisons further commits for the operation.

Tests generate actual signed repositories and inspect the actual synced store immediately before backend file opens to prove root/timestamp/snapshot progress precedes later reads. Commit failure after root 2 and harvest failure inside a root-fetch callback demonstrate the latch defeats Tough error swallowing and refuses all later fetches. Other tests cover valid-prefix persistence on later harvest error, no bytes on final commit failure after target verification, deterministic detached-worker completion/lock release, startup rejection preserving fresh-store ownership, panic/admission release, and OS-backed owned-worker failure/replay/success checkpoint roundtrip.

Focused validation with coordinator state.rs copied from main for compilation (not committed here): **43 tests** (36 owned core/updater/integration plus 7 coordinator store tests), plus compiled README examples; fmt and strict all-target Clippy pass. No root manifests/lockfiles, coordinator state.rs, or state module declaration are part of this lane's commit. The new updater module uses Tough's reexported async-trait, so no new manifest dependency is needed.

Validation note: after a successful parallel 43-test run, a subsequent parallel run intermittently failed the coordinator-owned `state::tests::exclusive_lock_survives_replacement_and_releases_on_drop` at immediate reopen after drop (`would block`). Its isolated rerun passed. This may be a transient inherited flock descriptor during another test's subprocess creation; coordinator was notified, and state.rs was not changed here. Both README examples compile. A serial full run also verifies the combined suite without concurrent subprocess lock inheritance.
