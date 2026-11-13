# Aegis vendor collateral (wave six, lane 4): real TDX and SEV-SNP samples

Claim: #246. Base: `07322e3ba47890e939a48967a1ea8f62fdd7aac1` (`origin/main`,
a descendant of the required `cda092f`). Design: §33 of
`docs/specification.txt` (Google Doc revision of 2026-11-13 04:46 UTC).
Earlier report: [aegis.md](aegis.md).

## Summary

The crate now verifies **real, publicly published vendor evidence offline**
against **pinned vendor roots**:

| Evidence | Root | Result |
| --- | --- | --- |
| TDX quote v4 from `dcap-qvl` 0.3.4 (`sample/tdx_quote`) with its PCCS collateral | Intel SGX Root CA | PCK chain, PCK CRL, root CA CRL, QE report, QE identity, TCB info and quote signatures all verify; TCB status `UpToDate` (platform, QE and TDX module `TDX_03` all `UpToDate`, FMSPC `B0C06F000000`, TCB evaluation data number 17) |
| Production SPR E4 TDX quote from `go-tdx-guest` v0.3.1 with PCS responses, PCK CRL and root CA CRL | Intel SGX Root CA | every signature, chain, CRL and collateral check passes; evaluation then reports `TcbLevelNotFound` because the quote (`TEE_TCB_SVN [3,0,4,…]`) and the PCK (SGX components `[3,3,2,2,2,1,0,2,…]`) are below both published levels: Intel's "TCB level not supported" outcome, as a typed result |
| SEV-SNP Milan v2 report and VCEK from `sev` 6.0.0 (`tests/certs_data`) | AMD ARK-Milan | ARK → ASK → VCEK chain (AMD RSA-PSS encoding), VCEK hwID = `CHIP_ID`, VCEK TCB = `REPORTED_TCB` (bl 3, tee 0, snp 8, ucode 115), P-384 signature |
| SEV-SNP Milan v2 report and VCEK from `go-sev-guest` v0.15.0 (`verify/testdata`) | AMD ARK-Milan | same checks (bl 2, tee 0, snp 5, ucode 68) |

Both kinds of real evidence are also shown **not** to be Vesper evidence:
the full Vesper verifiers (`verify_tdx_quote_with_collateral`,
`verify_snp_report_with_collateral`) reject them with `BindingMismatch`,
because their report data is not the Vesper channel/supply-chain binding.

## Pinned roots (item 1)

`src/roots.rs` compiles in the DER files under `crates/vesper-aegis/roots/`,
records each one's canonical vendor URL and SHA-256, and re-checks the
digest whenever the bytes are used (`PinnedCert::der`,
`PinnedCert::trust_root`). Intel's and AMD's endpoints are blocked from the
cloud sandbox, so each certificate was taken from **two independent
published copies with identical bytes**:

| Certificate | Canonical URL | SHA-256 (DER) | Copies compared |
| --- | --- | --- | --- |
| Intel SGX Root CA | https://certificates.trustedservices.intel.com/Intel_SGX_Provisioning_Certification_RootCA.cer | `44a0196b2b99f889b8e149e95b807a350e7424964399e885a7cbb8ccfab674d3` | `dcap-qvl` 0.3.4 `src/TrustedRootCA.der`; `go-tdx-guest` v0.3.1 `verify/trusted_root.pem` |
| AMD ARK-Milan | https://kdsintf.amd.com/vcek/v1/Milan/cert_chain | `69d063b45344d26a2e94e1f4210de49ef555308287d4c174445c95639a540bcd` | `sev` 6.0.0 `src/certs/snp/builtin/milan/ark.pem` and `tests/certs_data/cert_chain_milan`; `go-sev-guest` v0.15.0 `verify/trust/ask_ark_milan{,_vlek}.pem` |
| AMD SEV-Milan (ASK) | same | `67d303bd3905fd38db8b20e0793699870e7fa612eaad5dec358293fd8c0bac1b` | `sev` builtin and `cert_chain_milan`; `go-sev-guest` `ask_ark_milan.pem` |
| AMD SEV-VLEK-Milan (ASVK) | https://kdsintf.amd.com/vlek/v1/Milan/cert_chain | `c5e081f59b7efab1fe2f8b505e159704e72f29cab7ef7cf628a05a42439082f5` | `go-sev-guest` `verify/trust/ask_ark_milan_vlek.pem` and `verify/testdata/milanvlek.testcer` |
| AMD ARK-Genoa | https://kdsintf.amd.com/vcek/v1/Genoa/cert_chain | `4c6598d19c18719c5dfd4a7d335f674e5bfe1d8f800cea2cf270c10d103db2f1` | `sev` builtin; `go-sev-guest` `ask_ark_genoa{,_vlek}.pem` |
| AMD SEV-Genoa (ASK) | same | `5464738c1546aed5f2cecf1dc98c5c960a92e8913238a61711bc90ec6e828521` | `sev` builtin; `go-sev-guest` `ask_ark_genoa.pem` |
| AMD SEV-VLEK-Genoa (ASVK) | https://kdsintf.amd.com/vlek/v1/Genoa/cert_chain | `197e610743a917d6b9bb982a5a9226ccc0a15b611be0619e626aca9151457372` | `go-sev-guest` `ask_ark_genoa_vlek.pem` (one copy) |
| AMD ARK-Turin | https://kdsintf.amd.com/vcek/v1/Turin/cert_chain | `1f084161a44bb6d93778a904877d4819cafa5d05ef4193b2ded9dd9c73dd3f6a` | `sev` builtin and `cert_chain_turin`; `go-sev-guest` `ask_ark_turin_{vcek,vlek}.pem` |
| AMD SEV-Turin (ASK) | same | `5b77ef5fe7a7a004fd9032668fba9d0fda22f88c4442069a479636a6ae3b3185` | `sev` builtin and `cert_chain_turin`; `go-sev-guest` `ask_ark_turin_vcek.pem` |
| AMD SEV-VLEK-Turin (ASVK) | https://kdsintf.amd.com/vlek/v1/Turin/cert_chain | `104e10a8bd060a3c20a434261a57d0588fd65a88915b4f65b08bdecaf8df1a3c` | `go-sev-guest` `ask_ark_turin_vlek.pem` (one copy) |

Every ASK and ASVK is checked to be signed by its own product's ARK (and by
no other product's), and every ARK to be self-signed
(`amd_intermediates_chain_to_their_ark`).

**Which roots are claimed as real.** Following the contract ("only claim
real vendor roots where a test verifies a real vendor sample end to end"):
the **Intel SGX Root CA** and **AMD ARK-Milan / ASK-Milan** are verified end
to end by real samples. Genoa and Turin ARK/ASK and all three ASVKs are
pinned and their signatures checked, but **no real Genoa/Turin report or
real VLEK leaf was available**, so they are not claimed as exercised by
real evidence. **Arm CCA**: there is no single public platform root (each
silicon vendor runs its own endorsement service) and no public sample token
with a vendor chain was found, so nothing is pinned for CCA; CCA stays on
configured roots as before.

Sample provenance (package, version, path, licence, SHA-256 of every file)
is in `crates/vesper-aegis/tests/data/vendor/README.md`. All samples were
fetched from crates.io and the Go module proxy, the only routes open from
the sandbox.

## What was built (items 2–5)

### Chains, CRLs and AMD's RSA-PSS encoding (`src/chain.rs`)

- `verify_chain_with_crls(leaf, intermediates, root, now_ns, curve, crls,
  scope)` and `verify_path`: rustls-webpki path building with its CRL
  support (`RevocationCheckDepth::Chain`, `ExpirationPolicy::Enforce`).
  `RevocationScope::EveryCertificate` (Intel) denies unknown status, so the
  PCK must be covered by the PCK CRL and the PCK/TCB-signing CAs by the
  root CA CRL. `RevocationScope::IssuedByRoot` (AMD) requires a CRL issued
  by the root (the ARK's CRL covers ASK/ASVK; AMD publishes none for
  VCEKs) and allows unknown status for the VCEK.
- `AmdRsaPssSha384`: AMD's certificates and CRLs carry RSASSA-PSS
  parameters with an explicit `trailerField [3] INTEGER 1`. That is the
  ASN.1 DEFAULT, so DER forbids writing it, and webpki (which compares
  algorithm identifiers byte for byte) rejects AMD's chains with
  `UnsupportedSignatureAlgorithmContext`. The adapter accepts exactly that
  one `AlgorithmIdentifier` and calls ring's `RSA_PSS_2048_8192_SHA384`; it
  implements no cryptography. The real Milan chains exercise it.
- `verify_issued_by(child, issuer)` checks a CA certificate's signature by
  its issuer (webpki will not verify a CA as an end entity) with the same
  algorithm list.
- New typed errors: `Revoked`, `MissingCollateral`, `CollateralExpired`,
  `CertificateExpired`, `CertificateNotYetValid`, `Crl`, `Collateral`,
  `CollateralMismatch`, `TcbLevelNotFound`, `PinnedRootDigestMismatch`, and
  `AegisVerifyError::TcbStatusNotAccepted`.

### Intel collateral (`src/intel.rs`)

- `SignedCollateral` (exact signed bytes + ECDSA `r||s`), from the
  `sgx_ql_qve_collateral_t` form (`new`) or a PCS/PCCS response body
  (`from_pcs_response`, which keeps the field's JSON bytes exactly via
  `serde_json::RawValue`).
- `TdxTcbInfo::parse` (TDX TCB info v3: FMSPC, PCE ID, `tdxModule`,
  `tdxModuleIdentities`, levels) and `QeIdentity::parse` (`TD_QE` v2).
  Unknown statuses, duplicated keys, wrong ids/versions, non-16 component
  arrays, bad hex and bad timestamps are typed errors.
- `parse_pck_extensions`: the SGX extension (OID 1.2.840.113741.1.13.1):
  PPID, the 16 TCB components, PCESVN, CPUSVN, PCE-ID, FMSPC, SGX type;
  duplicates, unknown entries and out-of-range values are errors.
- `evaluate_tdx_tcb` implements Intel's published TDX algorithm (see the
  module documentation): component-wise SGX/PCESVN/TDX comparison (TDX from
  index 2 when module identities apply), the component-1 rule, QE identity
  match and level, TDX module identity (`TDX_<major>`) match and level, and
  QVL convergence. `TcbStatus` has all seven Intel statuses;
  `TcbAcceptance` (default: `UpToDate` only) never accepts `Revoked`.
- `verify_tdx_collateral` runs everything end to end (PCK chain with both
  CRLs, quote signatures, collateral chains with the root CA CRL and
  signatures, `issueDate ≤ now < nextUpdate`, evaluation).
  `tdx::verify_tdx_quote_with_collateral` adds the Vesper binding.
- `TdxQuote::parse_padded` accepts zero padding after the declared quote
  length (DCAP quote buffers are fixed size; `dcap-qvl`'s sample has 70
  zero bytes); `parse_prefix` reports the declared length. `parse` stays
  exact.

### AMD collateral (`src/amd.rs`)

- `AmdTcb` decodes `TCB_VERSION` per product (Milan/Genoa layout; Turin
  with FMC), with reserved bytes required to be zero.
- `parse_amd_cert_extensions`: product name, bl/tee/snp/ucode (and Turin
  FMC) SPLs, hwID (raw extension value: 64 bytes, 8 on Turin), CSP ID;
  duplicated or unknown AMD extensions and non-zero reserved SPLs are
  errors.
- `verify_snp_collateral`: `SIGNING_KEY` must match the collateral (VCEK or
  VLEK, both supported; the ASK/ASVK defaults to the pinned one), chain to
  the ARK with an optional ARK CRL, product name match, certificate TCB =
  `REPORTED_TCB`, VCEK hwID = `CHIP_ID` (a `MASK_CHIP_ID` report cannot be
  bound to a VCEK), and the P-384 signature.
  `snp::verify_snp_report_with_collateral` adds the Vesper binding.
- **TCB status for SNP.** AMD publishes no signed TCB-status collateral
  like Intel's TCB info: a VCEK is issued per chip and per TCB. The
  binding above makes the certificate prove the reported TCB; whether that
  TCB is current is the relying party's decision, expressed as
  `SnpTcbBaseline` → `TcbStatus::UpToDate` / `OutOfDate`. A revoked
  ASK/ASVK is `AttestError::Revoked`. This is a correction to the prompt's
  wording, which assumed Intel-style statuses for both vendors.

### Vault key slots (item 4, `src/gate.rs`, `src/host.rs`)

`VaultEnclaveGate<A: vesper_vault::AeadAlgorithm>` holds an `AeadOpener`
and an `AeadSealer` whose keys are Vault `SecretSlot<32>`s (the Rust form of
§23.2's `AeadKeySlot`: locked, `MADV_DONTDUMP`, shredded by `retire`, leak
counted on drop). Requests and replies use Vault's §23.3 frame
(`epoch u32be || seq u64be || tag[16] || ciphertext`) with the transcript
hash as AAD; reply nonces are Vault `NonceTicket`s; received sequence
numbers must strictly increase (only an authenticated frame advances the
window, so a forged far-future frame cannot lock the channel). Egress uses
a new `CiphertextSlice::from_vault_sealed`, constructible only from a
`SealedVaultFrame` that the gate builds after a successful seal. The old
`EnclaveGate` (gate-held ChaCha20-Poly1305 keys) is `#[deprecated]` and
kept for API stability until the wave ends, as the contracts require.

### `attest_channel_binding` (item 5)

No TEE exists in the cloud or CI, so requesting a quote from hardware
remains unavailable: `host::attest_channel_binding(dest, transcript_hash)`
is the §33.2 entry point and always returns the typed
`TeeError::HardwareUnavailable` (the variant is new; wave five had no
entry point at all). Nothing in this lane claims hardware
validation; the real samples were produced by vendor hardware elsewhere and
are verified here as data.

## Tests

| File | What |
| --- | --- |
| `tests/vendor_samples.rs` (13 tests) | pinned digests; ASK/ASVK → ARK signatures; both real TDX quotes end to end; real TDX collateral outside its window; tampered quote, TCB info, QE identity and CRL; missing PCK CRL; mismatched collateral; wrong root; TCB status from the real TCB info (OutOfDate with Intel advisories, module `TDX_01` OutOfDate convergence, unknown module, no level, QE mismatch); all seven statuses on edited copies; real PCK extension; both real SNP reports with explicit and pinned ASK, baselines, Vesper-binding rejection; tampered SNP report, another chip's VCEK, rewritten reported TCB, wrong product, wrong root, VLEK/VCEK mismatch, expired VCEK; 400-round mutation fuzz of every real input (no panics; a mutation is accepted only if it decodes to the same PCK chain) |
| `tests/revocation.rs` (3 tests, generated chains/CRLs with rcgen) | revoked PCK, revoked PCK CA, stale CRL, missing CRL coverage, forged CRL (same name, other key), garbage CRL; revoked PCK through `verify_tdx_collateral`; AMD: valid ARK CRL, revoked ASK, stale CRL, CRL not from the ARK, TCB downgrade (report claims an older TCB than its VCEK), OutOfDate baseline, another chip, malformed extensions, tampered signature |
| `tests/vault_gate.rs` (7 tests) | Vault-slot round trips (ChaCha20-Poly1305 and AES-256-GCM), unique strictly increasing reply nonces, no plaintext in shared memory, replay and reordering rejected, forged frames do not move the window, tampered/wrong-epoch/short frames, rekey at the record limit, Iago lengths, `attest_channel_binding` unavailable; no `AeadKey`/`NonceTicket` leaks after `retire` |
| `tests/zero_alloc.rs` | new `vault_gate_steady_state_does_not_allocate`: 1000 requests after 16 warm-up, counting global allocator, 0 allocations |
| `src/time.rs` unit test | strict UTC timestamp parsing |

All earlier tests still pass; one assertion changed: an expired PCK leaf is
now `AttestError::CertificateExpired` instead of the generic
`CertificateChain`.

## Checks

Commands run from the repository root with an isolated target directory
(`CARGO_TARGET_DIR=/home/claude/target`, `CARGO_BUILD_JOBS=4`,
`CARGO_INCREMENTAL=0`, debug info off):

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -p vesper-aegis --all-targets --locked -- -D warnings`: clean.
- `cargo test -p vesper-aegis`: all pass.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: clean
  (exit 0).
- `cargo test --workspace --locked`: 926 passed, 1 failed on the first run;
  the failure was the new mutation fuzz finding the embedded-root issue in
  decision 6. After the fix `cargo test -p vesper-aegis` passes in full;
  the workspace rerun is recorded in the PR.

Bazel is not installed in the sandbox; CI runs `bazel test //...` on
BuildBuddy. The new data files are inside the crate directory, which
`vesper_crate` globs by default, so `BUILD.bazel` is unchanged.

## Root patch

`Cargo.lock` only (its own commit): `vesper-aegis` gains `serde`,
`serde_json` (with the `raw_value` feature), `vesper-vault` and
`x509-parser =0.18.1`, all already in the lockfile. No new
`[workspace.dependencies]`, no new crate.

## Decisions

1. **Layering.** Vendor verification (`intel::verify_tdx_collateral`,
   `amd::verify_snp_collateral`) is separate from the Vesper binding
   (`tdx_vesper_claims`, `snp_vesper_claims`), so real non-Vesper samples
   can be verified at all, and tests show the binding rejecting them.
2. **TCB status is returned, not judged.** Verification returns the
   evaluation; `TcbAcceptance::check` applies the relying party's policy.
   `RelyingPartyPolicy` is unchanged (API additive).
3. **Strict windows.** Collateral must satisfy `issueDate ≤ now <
   nextUpdate`; `dcap-qvl` checks only `nextUpdate`.
4. **Component-wise comparison.** `dcap-qvl` 0.3.4 compares SVN arrays
   lexicographically; this crate compares every component, as Intel's
   algorithm specifies.
5. **MISCSELECT** in QE identity is read as a big-endian `u32` value and
   ATTRIBUTES as bytes in report order. The real samples have MISCSELECT 0
   with mask `FFFFFFFF`, so they do not distinguish byte orders for
   MISCSELECT.
6. **Embedded root copies.** Intel's PCK and issuer chains end with a copy
   of the root, which path building ignores (the pinned anchor is used).
   The mutation fuzz found that a corrupted copy therefore passed;
   `verify_tdx_collateral` now requires that last certificate to be
   byte-identical to the configured root.
7. **New dependency `x509-parser`** (already in the lockfile through rcgen)
   for certificate extensions and DER values; crypto stays in ring and
   webpki.

## Linear obligations

`VaultEnclaveGate` inherits Vault's discipline: non-Clone, `#[must_use]`,
exits via `retire`, dropped keys counted as `AeadKey` leaks; `NonceTicket`s
are spent by sealing or burned. **This is Rust host-library discipline, not
the Vesper source typing rules for `linear struct AeadKeySlot`.**

## Remaining gaps

- No real AMD KDS CRL, real VLEK-signed report, real Genoa/Turin report, or
  public CCA sample with a vendor chain was available; those paths are
  covered by generated chains only.
- SEV-SNP report versions above 3 (newer firmware) are rejected by the
  parser; Turin TCB layout is implemented but untested on real data.
- SGX (non-TDX) quotes, TDX quote v5 / TD report 1.5, and Intel's
  platform-instance (multi-package) PCK certificates are not supported.
- `attest_channel_binding` against hardware stays `Unavailable`.
- Lean: `lean/AegisToctou.lean` is unchanged; nothing new is proved about
  collateral evaluation.
