# Offline TUF target verifier

`vesper-loom-tuf` verifies a target from local repository directories using maintained `tough` 0.24.0 cryptography and TUF metadata verification. It has no HTTP transport, publishing, package execution, or source/build integration. This is a bounded verifier core, **not a complete stateful TUF updater**.

## Security boundary

The caller supplies an independently trusted bootstrap root and stable configured repository identity. A successful result contains authenticated owned target bytes, their exact signed length and SHA-256, opaque signed custom metadata, verified metadata versions, a verification time, and a checkpoint. Next verification starts from the checkpoint's latest authenticated root. It never silently falls back to bootstrap when previous state is malformed.

**Use `verify_target_attempt` and persist its checkpoint even on error.** It returns `VerificationAttempt { checkpoint: Option<Checkpoint>, result: Result<VerifiedUpdate, TufError> }`. After Tough finishes or fails, authenticated steps in its private datastore are reverified before promotion: consecutive root continuity, timestamp signatures and snapshot-version pins, then snapshot signatures/source hash pins and all snapshot-role floors. A valid newer timestamp establishes its snapshot-version floor even when the snapshot body fails. Root-only checkpoints have absent timestamp/snapshot floors. No failed result exposes target bytes. Invalid request identity/bootstrap/previous state cannot become a new checkpoint.

Progress is returned **only at call completion**, not durably at each internal TUF step. Cancellation, process termination, crash, extraction failure, or a caller failing to persist the returned checkpoint can still lose advancement. A full updater must durably retain each authenticated TUF step before continuing; this library does not implement that transaction state machine. Do not describe an atomic final checkpoint file as a complete crash-safe TUF updater.

The compatibility `verify_target` function returns only a successful update and **discards error progress**. Newer authenticated metadata followed by a later failure can then be replayed at older still-valid versions using the previous checkpoint. This is a rollback/freeze security limitation beyond crash durability. The regression `failed_target_leaves_older_checkpoint_usable_despite_newer_authenticated_metadata` deliberately demonstrates this legacy behavior. Stateful callers should use the attempt API instead.

Checkpoints are caller-protected trusted state. Their checksum detects accidental corruption; it is not a MAC, signature, provenance certificate, or defense against someone rewriting state and recomputing the checksum. Protect state against deletion, tampering, and rewind; serialize operations and persist returned checkpoints on both success and failure before using any target. The core writes only private temporary snapshots/state. The separate Unix `state::CheckpointStore` holds a cooperating-process lock and atomically persists opaque checkpoints; it does not make internal verifier steps durable. Passing `previous: None` explicitly initializes a fresh verifier with no previous version floors and must not be a recovery fallback for lost state.

Checkpoint schema 2 retains a bootstrap fingerprint/repository identity, authenticated root chain, monotonic timestamp version floor, snapshot version floor from authenticated timestamp pins, snapshot-role floors, and observed host time. Zero timestamp/snapshot floors denote root-only state; role floors can remain from an earlier snapshot while a newer timestamp pins a snapshot whose body failed. Complete schema-1 checkpoints remain readable. No partial checkpoint represents target acceptance or package identity authentication.

Root key rotation does not reset authenticated timestamp/snapshot/role floors in this API. This deliberately conservative policy can reject a legitimate repository recovery that requires resetting compromised fast-forwarded versions. Every previous snapshot role must remain and cannot decrease in version, including unrequested delegated roles. Root chain retention is bounded; a long-lived registry can require an explicit trusted bootstrap policy migration after exhausting the configured chain limit.

## Usage

```rust,no_run
use std::path::{Path, PathBuf};
use vesper_loom_tuf::{verify_target_attempt, Limits, LocalRequest};

# #[cfg(unix)]
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
use vesper_loom_tuf::state::CheckpointStore;
let limits = Limits::default();
// Explicit one-time initialization uses CheckpointStore::create; open never resets lost state.
let mut store = CheckpointStore::open(Path::new("protected-registry-state"))?;
let previous = store.load_checkpoint(&limits)?;
let attempt = verify_target_attempt(LocalRequest {
    repository_id: "configured-example-registry".into(),
    bootstrap_root: std::fs::read("trusted-bootstrap-root.json")?,
    metadata_dir: PathBuf::from("repository/metadata"),
    targets_dir: PathBuf::from("repository/targets"),
    target_name: "packages/example-1.0.0.tar".into(),
    previous,
    limits: limits.clone(),
}).await;
if let Some(checkpoint) = &attempt.checkpoint {
    store.commit_checkpoint(checkpoint, &limits)?; // Includes failed verification progress.
}
let update = attempt.result?; // Bytes can be used only after successful state persistence.
# let _ = update.bytes;
# Ok(())
# }
```

`Checkpoint::from_json(bytes, limits)` decodes a bounded wire structure; `verify_target` checks checksum, identity, bootstrap binding, root chain, version floors, and time. `root_version()` is an inspection helper and does not certify an unverified decoded checkpoint. `VerifiedUpdate.custom` is signed but application-specific: Loom namespace/package/version policy must validate its meaning separately.

## Verification and bounds

The implementation snapshots both directories into a private temporary directory before handing them to `tough::FilesystemTransport`. Safe expiry enforcement is unconditional. Tough verifies root threshold continuity, top-level and delegated signatures, snapshot version/hash pins, target authorization through delegation paths/termination, and the target SHA-256. Only a completely consumed successful target stream is returned. A separate exact-length comparison rejects a target shorter than its signed length even when its hash matches. Post-read checks sample real host time again, reject detected clock regression, and check root, timestamp, snapshot, top-level targets, and every loaded delegated expiry. No expiration-disable or injected-clock option exists. Before Tough runs, the core reserves worst-case checkpoint wire space for every prospective root and role floor in the bounded immutable snapshot, so a normal accepted step cannot exceed the configured return budget.

Defaults:

| Ceiling | Default |
| --- | ---: |
| Metadata file / total snapshot | 4 MiB / 32 MiB |
| Target file / total target snapshot | 64 MiB / 256 MiB |
| Combined directory entries, including empty directories | 4096 |
| Directory/delegation depth | 32 |
| JSON container depth | 32 |
| Delegation roles / snapshot role entries | 256 |
| Retained root chain / per-load root updates | 64 |
| Serialized checkpoint | 16 MiB |

The entire target directory is copied, so unrelated large files can reject an otherwise valid target. Metadata must be a flat directory containing only bounded JSON metadata files. Target trees can contain nested directories. Extra malformed metadata also rejects the snapshot. Preflight bounds collections and the delegation graph before recursive verification; cycles and excessive graph work are rejected conservatively. These stricter local policies are intentional and can reject repositories accepted by unrestricted TUF clients.

The `files` limit also caps every JSON collection, including signed `targets`, `meta`, and `keys`, per metadata document; each document's JSON traversal also has a `files × roles` node budget. Bootstrap and retained roots receive these checks too. Snapshot role floors and loaded target-role counts have the separate `roles` ceiling. Signed length fields cannot bypass snapshot size limits. Root version overflow is rejected before entering Tough. All root-chain members are reverified with Tough's old/new threshold methods when a checkpoint is used; a newly self-signed root without trusted continuity cannot become an anchor.

## Filesystem and platform assumptions

Tested on Linux. Unix opens regular input files with `O_NOFOLLOW | O_NONBLOCK`; symlinks, special files, and symlink directory ancestors are rejected. Windows code uses `FILE_FLAG_OPEN_REPARSE_POINT` and rejects reparse attributes, but Windows runtime behavior has not been tested in this milestone. Build dependencies include native AWS-LC C code, even with Tough's HTTP feature disabled. No macOS or Windows runtime verification is claimed.

Tough's file transport preserves URL escapes in filesystem paths. Private staging therefore requires an ASCII temporary-directory ancestry without spaces or percent characters. The core checks URL/path round-trip equivalence and rejects an unsupported temporary path with an explicit diagnostic before Tough starts. Configure `TMPDIR` (or the platform temporary directory setting) accordingly. Original source directory paths with spaces/non-ASCII/percent characters are supported through snapshotting; encoded or unusual target/role filenames remain subject to Tough's file-repository conventions.

The caller must prevent concurrent replacement of source directory ancestors during snapshot construction. The implementation checks ancestors, rejects symlinks/reparse points, and opens leaf files without following them, but does not implement directory-handle-relative traversal resistant to an adversary replacing parent directories between checks. Once constructed, Tough reads only the private immutable snapshot, so later input mutations cannot alter accepted target bytes. Mixed source-file versions fail signed hash/version checks; unauthenticated source filenames never become trust anchors.

Target names must be canonical UTF-8 relative paths with no empty, `.` or `..` segments, URI schemes, percent escapes, backslashes, query/fragment syntax, control characters, Windows device names, trailing dots/spaces, or platform-reserved component characters. Copied file components follow the same platform-safe policy (percent-encoded metadata role filenames are allowed). Source directory arguments may be relative but cannot contain parent traversal. Unicode component bytes are limited to 255. Snapshotting can perform bounded blocking filesystem work within the async call; applications needing an interactive executor should isolate the operation on their own worker runtime.

The OS wall clock and its integrity remain trust assumptions. Tough's accepted metadata encodings and canonicalization rules also apply; fixtures use whole-second RFC3339 expiries for portable signature round trips. A detected backward clock fails verification, but a falsified host clock is not a trusted time service.

## Unix checkpoint store

`state::CheckpointStore` provides explicit `create` and `open`, bounded
`load_checkpoint`, and `commit_checkpoint`. Keep its exclusive OS-lock guard
through loading, verification and committing. State directories must belong to
the current user with mode 0700 (or stricter); state/lock files must have no
group/other permissions. Parent directories must be protected against concurrent
replacement, and the persistent lock file must never be deleted. Missing state
in an existing directory, corrupt JSON, symlinks and unsafe permissions fail;
they never trigger implicit bootstrap initialization.

Commits write a private same-directory temporary file, sync it, rename over the
checkpoint, then sync the directory. A storage-write failure poisons the guard:
reopen to inspect recovered state before another attempt. Serialization/size
preflight failures do not start a write. The byte limit bounds checkpoint wire
size, rather than transient serialization memory. Reopening under the lock
removes reserved regular staging files left by dead writers; scanning more than
4096 state-directory entries fails for manual recovery. Store paths are fixed
to an absolute path before acquiring the lock, so later working-directory
changes cannot redirect state access. This requires a filesystem
with working file/directory sync and atomic same-directory rename. Tested on
Linux, including live cross-process lock contention, replacement under an
exclusive lock, and child-process exit without Rust cleanup. It does not protect against a state-directory owner
rewriting or deleting files, and does not make in-flight verification steps
durable. In particular, storing only `verify_target` successes does not fix
the failed-verification limitation above. Release target bytes only after all
required checkpoint persistence succeeds.

## Guarantees outside this crate

Successful TUF verification authenticates bytes under the configured repository root. It does not establish OIDC identity, Sigstore/transparency evidence, namespace ownership outside the configured repository, source-tree correspondence, builder independence, hermeticity, ABI compatibility, vulnerabilities, or cache eligibility. Builder attestation quorum verification remains a separate crate and policy.

## Validation

Tests generate real Ed25519-signed TUF repositories with fixed public test seeds. They exercise delegated targets, distinct-key root rotation, loading checkpoint root 2 after old root files disappear, successful and failed-attempt version floors across metadata roles, expired metadata, signature and snapshot hash mutations, scope rejection, target corruption/truncation/oversize, exact-length mismatch, state corruption/mismatched identities, resource limits, filesystem links, numeric role graph cycles/depth, temporary-path constraints, and the legacy failed-update limitation above. A Unix integration commits failure progress, drops/reopens the store, rejects older metadata, then persists a valid successful target and reopens again.
