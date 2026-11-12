# Configured-quorum artifact blob admission

Base: `1ffd0aec27dfcc24c5564585ae9068255490bf61`.
Branch: `parallel-next/artifact-admission`.
Exclusive delivery: new `crates/vesper-loom-admission/**` and this report.
No existing CAS/trust/build/TUF/Forge, wave-three, CLI, root manifest/lock or CI
files were edited. No push, package publication or PR was performed.

## Implemented boundary

`admit_blob(store_path, artifact: Vec<u8>, Context, Limits)` accepts a blob only
after reparsing complete bounded expected identity/current policy/bundle JSON,
strictly verifying every real Ed25519 attestation and configured operator quorum
at the explicit clock, rejecting tree/nonartifact output, and recomputing CAS's
actual artifact-blob hash. Store creation and `put_blob_expected` happen only
AFTER these checks. Trust/content rejection never initializes or writes CAS.
Every failed consuming import returns the original artifact vector allocation,
including storage failures after validation. No derivation index, registry
publication or persistent trust bit is changed.

`retrieve_blob(&Store, Context, Limits)` repeats complete validation, bounded
regular-file/no-follow reading and actual owned-memory hashing on each call.
Corruption is rejected without quarantine writes. A public previous report is
never an accepted authorization input. `materialize_blob(&Store, destination,
Context, Limits)` repeats those checks before same-directory temporary staging
and no-clobber publication of an independent regular blob copy. It neither
creates parent directories nor marks output executable.

Blobs have a hard 64 MiB limit; callers may lower it, including zero for empty
blobs. Each JSON input retains trust's 4 MiB hard limit; configured keys and
attestations retain its 64/128 count limits. Bounded reads inspect the opened
regular file and read at most limit+1 bytes, rehashing returned memory rather
than trusting metadata/path checks or CAS presence.

Reports bind complete canonical typed expected/policy/bundle content, evaluation
time and byte limit. Policy builders and attestations are sorted by validated
key ID; no label stands in for policy content. Unused/revoked configured keys
also affect policy identity. Digests are domain-separated/versioned and include
explicit lengths; report identity binds operation and outcome metadata. Public
report values remain diagnostics only. The crate README documents preimages and
all API/failure/ownership details.

Acceptance remains conditional on caller-supplied expected identity, current
policy/operator grouping and clock. It does not authenticate source namespaces,
policy provenance, OIDC, Sigstore or TUF; replay a build; establish hermeticity;
or authorize execution/package installation. Store-root ancestors remain a
trusted filesystem boundary. Tree/streaming admission is explicitly unsupported.
No process-crash/power-loss durability or Windows runtime claim is made. Ordinary
post-validation I/O failure can leave valid CAS/staging changes; validation
rejection cannot. Materialization does not overwrite an existing destination.

## Validation

Environment: Linux x86_64, Rust 1.99.1. Isolated target `target-next-admission`,
`CARGO_BUILD_JOBS=1`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`.

- `cargo test --manifest-path crates/vesper-loom-admission/Cargo.toml`:
  **12 integration tests passed**, zero failed/ignored; real Ed25519 and
  filesystem CAS import/read/materialization. No mock verifier.
- `cargo clippy --manifest-path crates/vesper-loom-admission/Cargo.toml --all-targets -- -D warnings`:
  passed.
- `cargo fmt --manifest-path crates/vesper-loom-admission/Cargo.toml --check`:
  passed; staged diff whitespace check also clean.
- `cargo check --manifest-path crates/vesper-loom-admission/Cargo.toml --all-targets --target x86_64-pc-windows-gnu --features blake3/pure`:
  passed; compilation evidence only, not Windows runtime validation.

Tests cover all eight expected identity fields, signature/blob tampering,
signed-tree rejection before store creation, current policy revocation and
clock/TTL revalidation of all operations, original allocation pointer recovery,
no-write rejection snapshots, full-policy/context identities and order
canonicalization, unused-key revocation, duplicate/rotated operator rules,
corrupt/oversized/symlink CAS objects, safe repair only after fresh validation,
independent copy/no overwrite, staging cleanup and storage failure ownership.
All standalone dependency name/version pairs also occur in root's current lock
except the new admission crate; no dependency version migration is requested.

## Coordinator patch

1. Add `"crates/vesper-loom-admission"` to root workspace `members` AND
   `default-members`.
2. Add root dependency (for the coordinator's CLI integration):
   `vesper-loom-admission = { path = "crates/vesper-loom-admission" }`.
3. Remove this new crate's temporary standalone `[workspace]` section and
   discard its generated standalone `Cargo.lock` (not part of this commit).
   Regenerate root lock to record the new path package while retaining current
   dependency versions. There is no module wiring patch.
4. Coordinator's planned CLI can use `vesper attest import --expected ...
   --policy ... --bundle ... --artifact ... --store DIR [--at SECONDS] [--json]`.
   Existing read-only attest verify behavior belongs to the coordinator.

Concrete types:

- `Context<'a> { expected_json: &'a [u8], policy_json: &'a [u8],
  bundle_json: &'a [u8], evaluated_at: u64 }`.
- `Limits { max_blob_bytes: usize }`; Default and `MAX_BLOB_BYTES` are 64 MiB.
- `admit_blob(...) -> Result<AdmissionReport, Box<AdmissionFailure>>`;
  failure fields `.error`/`.artifact`; `QuorumRejected(Box<VerificationReport>)`
  preserves real verifier diagnostics. `AdmissionReport` is serde serializable.
- `retrieve_blob(...) -> Result<RetrievedBlob, AdmissionError>`;
  `.artifact` and `.report` are independent owned results.
- `materialize_blob(...) -> Result<MaterializedBlob, AdmissionError>`;
  `.path` and `.report`; destination is fresh and nonexecutable.
