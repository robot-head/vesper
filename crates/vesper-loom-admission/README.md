# Artifact blob admission

This crate connects the configured Ed25519 builder quorum to actual Loom CAS
blob consumption. It adds no permanent trust flag, derivation index entry,
registry publication, network access, build execution or package installation.

`admit_blob(store_path, artifact: Vec<u8>, context, limits)` performs strict
bounded identity/policy/bundle parsing, verifies every supplied attestation and
the current configured operator quorum under the explicit clock, requires an
artifact-domain **blob**, and hashes the actual owned artifact bytes using CAS's
versioned domain-separated blob identity. Only then does it open/create the store
and call `put_blob_expected`. Any failure returns the original artifact allocation
in `AdmissionFailure.artifact`. Rejected trust/content inputs never initialize or
write the store. A storage error after all checks can leave valid CAS/staging
changes; this is not a transaction across all filesystem operations.

```rust,ignore
use vesper_loom_admission::{admit_blob, Context, Limits};
let report = admit_blob(
    store_path,
    artifact_bytes,
    Context {
        expected_json: &expected_identity_json,
        policy_json: &current_builder_policy_json,
        bundle_json: &signed_bundle_json,
        evaluated_at: trusted_unix_seconds,
    },
    Limits::default(),
)?;
```

`Context` is required afresh for each operation. The expected `BuildIdentity`
includes package/version, source tree, implementation derivation, target, exact
builder implementation digest, sandbox contract and output identity. The caller
must obtain that expectation, the current policy, operator grouping and clock
from its trusted orchestration/configuration boundary. Matching signed fields
and bytes do not authenticate a source namespace, establish independent operator
ownership, replay a hermetic build or implement OIDC, Sigstore or TUF.

`retrieve_blob(&store, context, limits)` repeats the full policy/quorum check,
opens a bounded regular object without following a final-component symlink,
reads at most the byte budget plus one detection byte and rehashes the returned
owned bytes. It never accepts an old report as authority and never quarantines
on rejection. Changing the store file later cannot change a returned vector.
The store root/ancestor directories remain a trusted local CAS boundary; final
object checks do not sandbox arbitrary hostile filesystem namespaces.

`materialize_blob(&store, new_destination, context, limits)` repeats the same
checks and bounded actual-byte read, then creates an independent regular-file
copy using same-directory temporary staging and no-clobber publication. It
creates no parent directories, never overwrites an existing destination and
cleans ordinary staging failures. It marks nothing executable and provides no
process-crash/power-loss durability guarantee. Output files are caller-owned;
subsequent caller edits do not change CAS or establish new authorization.

Artifacts have a hard **64 MiB** bound and may use a stricter per-operation
`Limits.max_blob_bytes`; zero permits only empty blobs. Each JSON context input
has trust's independent **4 MiB** bound; policy keys and attestations retain its
64/128-element limits. There is no streaming, tree/recursive-child admission,
remote fetch, executable loading or source-language authority audit in this
slice. Retrieval uses explicit Unix/Windows no-follow opening; other targets
return `UnsupportedPlatform`.

Reports are ordinary public diagnostic values. Context identities hash complete
validated typed expected/policy/bundle content, clock and blob limit. Policy
builders and attestations are sorted by key ID after duplicate rejection, so
JSON field ordering, whitespace and registry/bundle order do not change those
identities. Every policy field, including unused configured keys and revocation,
is included; no version label stands in for policy content. Hashes use a
versioned domain, LE u64 domain length/domain, LE u64 compact JSON length/JSON;
the self-digest field is the empty string in each digest preimage. Report hashes
also bind the operation, verified output/size and fresh verification report;
import's newly-stored outcome deliberately distinguishes first versus repeated
imports. These hashes are content identities, not signatures or capabilities.

Checks use real Ed25519 signatures and on-disk temporary CAS stores. They cover
exact import/retrieval/copy, no overwrite, full-identity mismatch, signature/blob
tampering, tree rejection, revocation and expiry on later operations, canonical
context identities and full-policy changes, operator/key rotation and duplicates,
corrupt/oversized/symlink objects, original allocation recovery and no-write
rejection. See `tests/admission.rs` for executable examples.
