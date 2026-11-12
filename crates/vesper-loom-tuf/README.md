# Offline TUF target verifier

`vesper-loom-tuf` verifies a target from local repository directories using maintained `tough` 0.24.0 cryptography and TUF metadata verification. It has no HTTP transport, publishing, package execution, or source/build integration. This is a bounded verifier core, **not a complete stateful TUF updater**.

## Security boundary

The caller supplies an independently trusted bootstrap root and stable configured repository identity. A successful result contains authenticated owned target bytes, their exact signed length and SHA-256, opaque signed custom metadata, verified metadata versions, a verification time, and a checkpoint. The checkpoint carries the original-root binding, authenticated consecutive root chain, last successful timestamp/snapshot/role version floors, and last successful time sample. Next verification starts from its latest root. It never silently falls back to bootstrap when previous state is malformed.

**Failed verification does not return checkpoint advancement.** If newer authenticated root/timestamp/snapshot/targets metadata is followed by a failed later metadata load or target download, the caller retains the previous successful checkpoint. Older still-valid metadata can then be accepted again up to that checkpoint's version floors. This is a rollback/freeze security limitation even with perfect atomic checkpoint persistence, not merely a crash-durability limitation. The regression test `failed_target_leaves_older_checkpoint_usable_despite_newer_authenticated_metadata` demonstrates it. A full updater must durably retain each authenticated TUF step, including advancement on later failure; that state machine is not implemented here.

Checkpoints are caller-protected trusted state. Their checksum detects accidental corruption; it is not a MAC, signature, provenance certificate, or defense against someone rewriting state and recomputing the checksum. Protect state against deletion, tampering, and rewind; serialize operations and persist a successful checkpoint before using its target. This library writes only private temporary snapshots/state, not persistent checkpoints. It does not guarantee crash-safe client updates. Passing `previous: None` explicitly initializes a fresh verifier with no previous version floors and must not be a recovery fallback for lost state.

Root key rotation does not reset successful timestamp/snapshot/role floors in this API. This deliberately conservative policy can reject a legitimate repository recovery that requires resetting compromised fast-forwarded versions. Every previous snapshot role must remain and cannot decrease in version, including unrequested delegated roles. Root chain retention is bounded; a long-lived registry can require an explicit trusted bootstrap policy migration after exhausting the configured chain limit.

## Usage

```rust,no_run
use std::path::PathBuf;
use vesper_loom_tuf::{verify_target, Checkpoint, Limits, LocalRequest};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let limits = Limits::default();
// Read any existing checkpoint with an independently bounded, protected file reader.
let previous: Option<Checkpoint> = None; // Only an explicitly new state starts with None.
let update = verify_target(LocalRequest {
    repository_id: "configured-example-registry".into(),
    bootstrap_root: std::fs::read("trusted-bootstrap-root.json")?,
    metadata_dir: PathBuf::from("repository/metadata"),
    targets_dir: PathBuf::from("repository/targets"),
    target_name: "packages/example-1.0.0.tar".into(),
    previous,
    limits,
}).await?;
let next_checkpoint = update.checkpoint.to_json()?;
// Caller must persist next_checkpoint safely before consuming update.bytes.
// It must separately implement per-step advancement on failure for complete updater guarantees.
# let _ = next_checkpoint;
# Ok(())
# }
```

`Checkpoint::from_json(bytes, limits)` decodes a bounded wire structure; `verify_target` checks checksum, identity, bootstrap binding, root chain, version floors, and time. `root_version()` is an inspection helper and does not certify an unverified decoded checkpoint. `VerifiedUpdate.custom` is signed but application-specific: Loom namespace/package/version policy must validate its meaning separately.

## Verification and bounds

The implementation snapshots both directories into a private temporary directory before handing them to `tough::FilesystemTransport`. Safe expiry enforcement is unconditional. Tough verifies root threshold continuity, top-level and delegated signatures, snapshot version/hash pins, target authorization through delegation paths/termination, and the target SHA-256. Only a completely consumed successful target stream is returned. A separate exact-length comparison rejects a target shorter than its signed length even when its hash matches. Post-read checks sample real host time again, reject detected clock regression, and check root, timestamp, snapshot, top-level targets, and every loaded delegated expiry. No expiration-disable or injected-clock option exists.

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

Tests generate real Ed25519-signed TUF repositories with fixed public test seeds. They exercise delegated targets, distinct-key root rotation, loading checkpoint root 2 after old root files disappear, successful-operation version floors across all metadata roles, expired metadata, signature and snapshot hash mutations, scope rejection, target corruption/truncation/oversize, exact-length mismatch, state corruption/mismatched identities, resource limits, filesystem links, graph cycles, and the failed-update security limitation above.
