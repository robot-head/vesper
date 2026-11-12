# Offline builder-attestation checks

`vesper attest verify` checks an actual artifact blob against signed build identities and a quorum of configured Ed25519 keys/operator groups. It performs no network requests, builds, signing, publication or cache-trust mutation.

The repository contains a reproducible synthetic fixture with public test keys and real signatures:

```sh
cargo run -- attest verify \
  --expected crates/vesper-loom-trust/tests/fixtures/identity.json \
  --policy crates/vesper-loom-trust/tests/fixtures/policy.json \
  --bundle crates/vesper-loom-trust/tests/fixtures/bundle.json \
  --artifact crates/vesper-loom-trust/tests/fixtures/artifact.bin \
  --at 150 --json
```

`--at` selects explicit Unix seconds; otherwise the command evaluates validity at the host's current time. The fixture is valid at 150 and expires at 300, so it is not evidence of a current production build. Its fixed signing seeds are test data, not production keys.

## What is checked

The expected identity binds a qualified package/version, source-domain tree ID, implementation derivation key, target, exact builder digest, versioned sandbox contract and artifact-domain output ID. Every field, the key ID, issuance and expiry are signed with a versioned domain-separated canonical message. An interface MIF alone cannot identify a build.

The library uses strict Ed25519 verification. Policies reject malformed/weak/duplicate keys, require at least two configured operator groups, and check revocation, key validity, statement validity and maximum TTL. Key rotation within one operator does not create a second independent builder. Invalid, unknown, duplicate or mismatching extra attestations reject the entire bundle. JSON schemas reject unknown/duplicate fields and unsupported protocol versions.

The CLI additionally requires `--artifact`, streams its bytes through the CAS's artifact-blob hash, and compares the result to the signed expected ID. Valid signatures cannot approve a different file. Only artifact blobs are supported by this CLI milestone; the library can represent tree outputs, but the CLI does not pretend a directory is a blob. JSON inputs are bounded to 4 MiB each and artifact files to 4 GiB.

## What acceptance means

Acceptance is conditional on the supplied expected identity, trusted key registry, configured operator independence and evaluation clock. Different keys do not cryptographically prove different operators; the policy maintainer provides that grouping. The source tree, derivation and sandbox fields are signed assertions checked against trusted expected inputs, not a replay of the build or independent proof of hermetic execution.

This is not Sigstore/OIDC/TUF verification, source-namespace authentication, a compiler authority audit, ABI/CVE analysis or a complete Lila deployment. It does not authenticate how the local policy was obtained. All CLI reports retain `cache_eligible: false`; future registry/build integration must supply the missing provenance and policy gates before changing cache trust. Use immutable CAS objects for subsequent consumption rather than assuming a local path cannot change after verification.

JSON mode emits a schema-versioned `verified` or `rejected` report, the actual/expected blob IDs, the full signature/quorum diagnostics, explicit trust basis and evaluation time. Unsupported output kinds and resource-limit failures produce structured errors. Argument, file and JSON parsing failures use stderr. No input files or cache records are rewritten.

## Importing verified blob bytes

`vesper attest import` repeats the complete current policy, signature, clock and
content checks, then imports the exact owned artifact bytes into the CAS:

```sh
cargo run -- attest import \
  --expected crates/vesper-loom-trust/tests/fixtures/identity.json \
  --policy crates/vesper-loom-trust/tests/fixtures/policy.json \
  --bundle crates/vesper-loom-trust/tests/fixtures/bundle.json \
  --artifact crates/vesper-loom-trust/tests/fixtures/artifact.bin \
  --store /tmp/vesper-admitted-artifacts --at 150 --json
```

Import supports regular artifact files up to 64 MiB, rejects final-component
symlinks/special files, and detects files growing beyond the limit. The Unix
open also uses nonblocking/no-follow flags so a raced FIFO cannot block the
artifact read. It creates no store directory until the library has accepted
both the fresh configured quorum and actual content hash. Expired, revoked,
malformed, mismatching or unsupported inputs leave the store untouched.

This writes immutable CAS bytes; it creates no derivation index, permanent trust
flag, executable permission or installation/publication record. An I/O failure
after validation may leave valid CAS/staging changes. Repeating import always
rechecks current policy, even when the blob is already present. Successful
reports bind full evaluated context and operation/outcome metadata; reports
are diagnostic values, never reusable authorization handles.

The [admission library](../crates/vesper-loom-admission/README.md) additionally
supports freshly validated bounded retrieval and no-clobber materialization.
The supplied expected identity, key policy, operator grouping and clock retain
the same explicit trust boundary as `attest verify`.
