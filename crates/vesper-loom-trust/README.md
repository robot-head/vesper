# Offline builder attestations

This bounded milestone verifies **real strict Ed25519 signatures** and requires a quorum of configured operator groups for an exact expected build identity. It does not implement Sigstore, OIDC, TUF, registry lookups, source ownership, real builder execution, or automatic cache/publication approval.

`verify_attestations(&BuildIdentity, &AttestationBundle, &BuilderPolicy, now)` returns a deterministic versioned report. Every supplied envelope must be valid; malformed, duplicate, expired, revoked, unknown-key or mismatching extras reject the entire bundle, even if other envelopes meet quorum. No network or host clock is used by the library.

A trusted caller supplies the expected package/exact SemVer version, source-domain **tree** ID, complete CAS derivation key, target, exact builder implementation BLAKE3 digest, sandbox contract identity, and artifact-domain output ID. All fields are covered by the signature, along with key ID and validity interval. Domain/kind confusion and using an interface MIF in place of a derivation key are rejected. The caller remains responsible for computing the derivation from all actual implementation/toolchain/options inputs and hashing the actual source/output bytes. The library neither reads artifacts nor proves that the signed build assertion is true.

The policy explicitly assigns each trusted public key an `operator_id`. Key rotation or multiple keys for one operator count once. At least two distinct configured operators are required; duplicate public keys cannot masquerade as separate identities. Configuration is an assumption about operator independence, not evidence that infrastructure is independently operated. Policies also bind key validity/revocation, minimum operators and maximum statement TTL.

Key and statement intervals are half-open: `issued_at <= now < expires_at`, `valid_from <= now < valid_until`; the complete statement interval must fit the key interval. TTL checks use checked subtraction. Zero/reversed intervals, future issuance, expiration and revocation fail closed. Public keys/signatures and digest spellings use fixed-size lowercase hexadecimal. Weak keys, noncanonical signatures and wrong-key signatures are rejected through `ed25519_dalek::verify_strict`.

## Signature protocol, schema 1

`canonical_signing_bytes(&Statement)` validates the statement and returns:

1. ASCII `vesper.loom.builder-attestation.v1` followed by one zero byte;
2. payload byte length as a little-endian `u64`;
3. compact `serde_json` encoding of the `Statement`, in the declared struct-field order.

The statement/identity fields contain only bounded canonical ASCII identities and unsigned integers; no floating-point or unordered map encoding participates. JSON input whitespace/key ordering is normalized before signing/verification. Unknown and duplicate struct fields are rejected. This is this protocol's canonical encoding, not a claim of general RFC 8785 JSON canonicalization or DSSE compatibility. Future schema changes require a new domain/version.

The byte-parsing helpers (`parse_identity`, `parse_policy`, `parse_bundle`) reject JSON over 4 MiB and perform strict structural deserialization; semantic/schema/cryptographic validation happens in `verify_attestations`. Direct typed callers must likewise use verification. Limits are 64 trusted keys, 128 attestations, bounded identity labels (256 bytes) and object/key/digest sizes. Error messages do not echo unbounded attacker-controlled fields.

## Reproducible fixture

`tests/fixtures/` contains an actual two-key signed bundle, expected identity, policy and artifact bytes. It is valid at evaluation time `150` and expires at `300`. The tests rebuild its canonical source tree and derivation, check the artifact's exact CAS identity, reproduce the signatures from fixed **test-only** signing seeds, and compare every committed byte. Production exposes no signing/private-key storage path.

The fixture uses the actual current component interface identity `vesper:sandbox/step@0.1.0`. It is a synthetic signed test assertion, not an attestation that production hermetic builders ran a compiler. To intentionally regenerate it after a versioned protocol change, run the fixture test with `VESPER_BLESS_ATTESTATION_FIXTURE=1` and review the resulting changes.

```sh
cargo fmt --manifest-path crates/vesper-loom-trust/Cargo.toml -- --check
cargo test --manifest-path crates/vesper-loom-trust/Cargo.toml --locked
cargo clippy --manifest-path crates/vesper-loom-trust/Cargo.toml --locked --all-targets -- -D warnings
```

The temporary standalone workspace/lockfile are for handoff only. The coordinator must register this crate in root members/default-members and generate the shared lockfile. Acceptance remains conditional on trusted expected inputs, keys, configured independence groups and clock; it is not source provenance, compiler authority, hermeticity, ABI compatibility, CVE safety, or a proof certificate.
