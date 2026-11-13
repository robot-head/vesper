# Vesper Aegis (wave five, lane 9): attestation policy, page acceptance and single-fetch host values

Claim: #129. Base: `af03122ea74d403f203170926945f9d35786b289` (`origin/main`).
Design: §33 of `docs/specification.txt` (Google Doc revision of
2026-11-12 09:18 UTC).

## What was delivered

A new library crate, `crates/vesper-aegis`, and a Lean model,
`lean/AegisToctou.lean`.

| §33 item | Where | Status |
| --- | --- | --- |
| TDX quote v4 parser (header, TD 1.0 report body, signature data, certification data type 6 → type 5 PCK chain) | `src/tdx.rs` | Done, bounds checked, typed errors |
| SEV-SNP attestation report (v2/v3, 1184 bytes) | `src/snp.rs` | Done |
| Arm CCA token (tag-399 collection of two COSE_Sign1 tokens) | `src/cca.rs` | Done |
| Signature verification and chains to a configured root | `src/chain.rs`, per-format `verify_*` | ECDSA P-256 (TDX QE report and quote), P-384 (SNP VCEK), COSE_Sign1 ES256/ES384 (CCA platform and realm); X.509 paths through `rustls-webpki` |
| Normalized claims and 128-byte `AegisAttestationClaimsWire` | `src/claims.rs` | Done; layout 2+2+4+48+32+32+8 = 128 bytes matches the design |
| §33.5 RA-TLS policy verifier | `src/policy.rs` | Done (`verify_attestation_claims`, `verify_ra_tls`) |
| GPA page acceptance with `UnacceptedGpaRange` owners | `src/gpa.rs` | Done; split/merge, double/overlapping accept, unaccepted reads |
| `UntrustedHostVal<T>`, `HostBounceSlice` vs private memory, Iago validators | `src/host.rs` | Done |
| §33.3 inference gate over a simulated shared ring | `src/gate.rs` | Done, 0 alloc in steady state (measured) |
| Lean: post-snapshot non-interference and policy soundness | `lean/AegisToctou.lean` | Done, no `sorry`/`admit`/axioms |

### Public API (main items)

- `tdx::{TdxQuote, verify_tdx_quote}`, `snp::{SnpReport, verify_snp_report}`,
  `cca::{CcaToken, verify_cca_token, cca_rim_measurement, build_sign1, encode_collection}`.
- `chain::{TrustRoot, verify_chain, EcPublicKey, Curve, pem_certificates}`.
- `claims::{AttestationClaims, AegisAttestationClaimsWire, GuestRuntimeClaims,
  SupplyChainEvidence, SnpTcbVersion, single_extend, supply_chain_digests}`.
- `policy::{RelyingPartyPolicy, TcbFloors, verify_attestation_claims, Evidence,
  TrustRoots, verify_ra_tls, tls_key_channel_binding}`.
- `gpa::{GpaAcceptanceTracker, UnacceptedGpaRange, PrivatePageSlab, PageAcceptor,
  SimulatedGuestMemory}`; the design's raw `accept_gpa_range` is kept.
- `host::{UntrustedHostVal, HostClockValidator, HostMappingTable, BounceAperture,
  HostBounceGrant, HostBounceSlice, HostSide, PrivateRegion, PrivateSlice,
  SealedFrame, CiphertextSlice}`.
- `gate::{EnclaveGate, NonceSequence, NonceTicket, tenant_seal_frame, tenant_open_frame}`.

## How verification works

Each format verifier takes the evidence bytes, the configured root, a
caller-supplied time (`u64` ns since the Unix epoch; no library code reads a
clock) and the guest's untrusted `GuestRuntimeClaims` (channel binding,
Loom.lock digest, `.olean` fingerprint). It returns normalized claims only
after all of these hold:

- **TDX:** PCK chain (PEM in certification data) to the root; PCK signature
  over the 384-byte QE report; `qe_report.report_data = SHA-256(attestation
  key ‖ QE auth data) ‖ 0^32`; attestation-key signature over header and
  body; TD report data equals the Vesper binding (below); RTMR[2] and RTMR[3]
  equal one SHA-384 extend of the claimed Loom and `.olean` digests.
- **SEV-SNP:** `[VCEK, ASK]` chain to the ARK root; VCEK P-384 signature over
  bytes `0..0x2A0` (little-endian 72-byte `r`/`s` converted, high bytes must
  be zero); report data equals the Vesper binding. VLEK-signed reports are
  rejected with a typed error.
- **CCA:** IAK certificate chain to the root; platform COSE_Sign1 signed by
  the IAK; realm COSE_Sign1 (ES384) signed by the RAK in the realm claims
  (a CBOR `COSE_Key`, EC2/P-384, or a legacy raw SEC1 point);
  platform challenge equals the hash of the RAK claim bytes; realm challenge equals the
  Vesper binding; REM[2]/REM[3] equal one extend of the claimed digests
  (extend data zero-padded to 64 bytes).

The Vesper binding is
`report_data = SHA-384("vesper-aegis/report-data/v1\0" ‖ channel_binding ‖ loom ‖ olean) ‖ 0^16`,
so report data commits to the channel and both supply-chain digests on all
three platforms.

Not checked (explicit gaps): Intel TCB-info and QE-identity collateral (TCB
status, QE MRSIGNER/ISVSVN), AMD KDS TCB collateral, CRLs/revocation, CCA
software-component endorsements (CoRIM reference values).

## Corrections to the design (recorded per the contracts)

1. **TCB floor per architecture.** The normalized `tcb_svn` values are not
   comparable across vendors, so `RelyingPartyPolicy` has `TcbFloors { tdx,
   snp, cca }` instead of one `min_tcb_svn`. TDX: `TEE_TCB_SVN[1] << 8 |
   TEE_TCB_SVN[0]`. SNP: `snp_fw << 8 | tee` of the reported TCB, plus optional
   per-component floors on the raw TCB. CCA: 0, because the token carries no
   single security version.
2. **Full channel binding.** The design compares an 8-byte prefix, which is a
   64-bit binding. The verifier compares the full 48-byte SHA-384 binding in
   constant time; the wire struct still carries the 8-byte prefix.
3. **RTMRs are extend registers.** A 32-byte digest cannot be "carried" in an
   RTMR. The verifier replays one extend of the claimed digest (TDX:
   `SHA-384(0^48 ‖ d ‖ 0^16)`; CCA: `H(0^n ‖ d ‖ 0^32)`). SEV-SNP has no
   runtime registers, so its digests are bound by report data only; claims
   record this as `SupplyChainEvidence::ReportDataOnly`, and
   `require_register_replay` rejects it.
4. **Measurement allowlist.** One `expected_mr_enclave` becomes
   `allowed_measurements: Vec<(TeeArch, [u8; 48])>`.
5. **CCA RIM size.** RIMs are 32 or 64 bytes (SHA-256/512). The normalized
   48-byte value is `SHA-384("vesper-aegis/cca-rim/v1\0" ‖ algo ‖ 0 ‖ RIM)`,
   exposed as `cca_rim_measurement`.
6. **GPA tracker structure.** The design's per-page `FxHashSet` is replaced by
   interval sets of accepted and pending page frames (no `rustc-hash`
   dependency, memory proportional to intervals). Announcements that repeat
   accepted pages are `HypervisorGpaDoubleAccept`; partial overlaps with
   accepted or pending pages are `OverlappingGpaRange`.
7. **Expansion-37 review (§33):** "signed hardware attestation and trusted
   page acceptance need real platforms". Accordingly nothing here claims
   hardware validation (see below).

## Linear obligations

`UnacceptedGpaRange`, `HostBounceGrant` and `NonceTicket` are non-Clone,
non-Copy, `#[must_use]` owners. Their only exits are the documented consuming
calls (`accept`/`relinquish`/`split`/`merge`; `flush_ciphertext_and_release`/
`abort_and_scrub_bounce`; sealing). Failed consuming calls return the owner. A
dropped owner increments a ledger that the issuer reports
(`leaked_ranges`, `leaked_grants`, `leaked_tickets`); tests assert both the
count on deliberate drops and zero on every normal and error path. **This is
Rust host-library discipline, not an implementation of the Vesper source
typing rules for `linear struct` or `linear effect TeeHardware`.**

Misrouting is a type error, shown by four `compile_fail` doctests in
`src/host.rs`: private plaintext cannot be passed where `CiphertextSlice` is
required, `CiphertextSlice` has no constructor from bytes, a grant cannot be
used after it is consumed, and the raw bits of `UntrustedHostVal` are private.

## Lean model (`lean/AegisToctou.lean`)

Proved, about the model only (not about the Rust code):

- `snapshot_blocks_hypervisor_toctou`: for any capacity, any authentication
  and evaluation functions, two host timelines that agree on the reported
  length and on the snapshotted bytes at `t0` give equal ingress outputs.
- `mutation_after_snapshot_is_invisible`: replacing the host timeline after
  `t0` with arbitrary memory and lengths leaves the output unchanged.
- `double_fetch_interferes`: a double-fetch ingress has two timelines equal
  at `t0` with different outputs (why the single fetch matters).
- `verify_quote_enforces_supply_chain_and_tcb`: an accepted claim set has an
  allowed architecture, debug disabled, TCB at or above the floor, an
  allowlisted measurement, the expected Loom and `.olean` digests and the
  expected channel binding. `checkPolicy_complete` is the converse;
  `debug_reported_first` fixes the reporting order.

Axioms used: `propext`, `Quot.sound` only. `tests/lean_model.rs` audits the
source (no `sorry`/`admit`/`native_decide`/`axiom`), and with `VESPER_LEAN`
compiles the file, checks the axiom report, and compares `checkPolicy` with
the Rust `verify_attestation_claims` on all 128 combinations of the seven
conditions (all agree). The SNP-only checks (VMPL, component floors, register
replay requirement) are not in the model.

## Tests

| File | What it shows |
| --- | --- |
| `tests/attestation.rs` (7) | Each format verifies end to end; every protected byte region flipped → rejected; forged keys, wrong root, expired leaf, wrong curve, missing ASK, QE binding, RTMR/REM replay, realm/platform pairing, wrong channel binding, missing root |
| `tests/parser_fuzz.rs` (4) | 3000 mutations per format (truncate, bit flips, splices, extreme length words, random bytes) never panic; accepted parses re-encode byte-identically; only the unauthenticated PEM tail may differ in an accepted TDX quote; 100 000-deep CBOR nesting and duplicate claim keys rejected |
| `tests/policy.rs` (3) | Every policy rule rejects, ordering, SNP VMPL/component floors, register-replay requirement, exhaustive small-domain reference |
| `tests/gpa_props.rs` (5) | Split/merge/accept/relinquish, all typed errors, backend failure mid-range, leak accounting, and 200 random 120-operation sequences against a reference page bitmap |
| `tests/host.rs` (5) | Length, byte-count, clock (backwards/jump) and mmap (overlap, unaligned, outside window, overflow) validators; grant linearity and foreign grants |
| `tests/toctou.rs` (4) | Gate round trip with only ciphertext in shared memory; every error path scrubs and releases; deterministic post-snapshot mutation does not change output (and a double fetch would); 2000 runs against a concurrent mutating "hypervisor" thread, each output equal to the reference over the actual snapshot (one local run: 484 authenticated, 1516 torn frames rejected) |
| `tests/zero_alloc.rs` (1) | 1000 warmed-up gate requests perform 0 heap allocations (thread-local counting global allocator) |
| `tests/lean_model.rs` (2) | Source audit; Lean compile + axiom audit + 128-case differential |
| doctests (4) | `compile_fail` partition and linearity checks |

Commands run (Linux x86_64 cloud container, Rust 1.97.0, Lean 4.24.0):

```sh
cargo fmt --all -- --check
cargo clippy -p vesper-aegis --all-targets -- -D warnings
VESPER_LEAN=lean cargo test -p vesper-aegis        # 31 tests + 4 doctests pass
cargo clippy --workspace --locked --all-targets -- -D warnings
VESPER_LEAN=lean cargo test --workspace --locked
lean lean/AegisToctou.lean
```

## Hardware

Real Intel TDX, AMD SEV-SNP and Arm CCA hardware is **unavailable** and was
not exercised. All evidence is produced by test code with generated keys and
generated certificate chains; vendor roots (Intel SGX Root CA, AMD ARK/ASK,
Arm/CCA endorsement roots) are untested, and no real vendor sample is
included. RSA-PSS/SHA-384 is enabled on certificate paths because AMD's ARK
and ASK use it, but no test covers it. `SimulatedGuestMemory`,
`BounceAperture` and the gate model the protocol in ordinary process memory:
nothing is encrypted by hardware, no `TDG.MEM.PAGE.ACCEPT`/`PVALIDATE` is
executed, and shared memory is an atomic byte array.

## Dependencies and root patch

New crate dependencies, all mature and pinned to versions compatible with the
lockfile: `ring 0.17.14` and `rustls-webpki 0.103.16` (already in the
lockfile), `rustls-pki-types 1.16`, `spki 0.7.3`, `coset 0.3` (with its
`ciborium`), workspace `chacha20poly1305`, `subtle`, `zeroize`, and
`vesper-loom-cas` (read-only, for Loom object digests). Dev-only: `rcgen
0.14` for generating test chains. No unsafe code in the library
(`#![forbid(unsafe_code)]`); the only `unsafe` is the counting allocator in
`tests/zero_alloc.rs`, as in other crates.

Root patch (separate commit): register `crates/vesper-aegis` in `members` and
`default-members` before `experiments/native-pulse`, and the regenerated
`Cargo.lock`. No new `[workspace.dependencies]`.

## Remaining gaps

- Vendor collateral (TCB info, QE identity, KDS), revocation, and real vendor
  roots and samples.
- `attest_channel_binding` (requesting a quote from hardware) has no backend;
  evidence generation exists only in tests.
- `TeeHardware` is a set of concrete methods, not an effect; source-language
  `linear struct`/`linear effect` typing is not implemented here.
- Sealing keys as Vault `AeadKeySlot` (§33.1 item 3) wait for lane 1; the gate
  uses ChaCha20-Poly1305 keys held in the gate and a counter nonce sequence.
- `vesper build --tee` RTMR extension and CLI wiring belong to integration B.
