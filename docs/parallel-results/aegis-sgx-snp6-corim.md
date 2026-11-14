# Aegis: SGX DCAP quote v3, SEV-SNP report v6, CoRIM appraisal for CCA (wave eight, lane 24)

Claim: #473. Base: `1094e649c95e4d81eef4e705d13882ab1339542d` (`origin/main`).
Design: §33. Earlier reports: [aegis.md](aegis.md),
[aegis-vendor-collateral.md](aegis-vendor-collateral.md),
[aegis-collateral-rest.md](aegis-collateral-rest.md).

## Summary

| Evidence | Trust | Result |
| --- | --- | --- |
| **SGX DCAP quote v3**, real Intel enclave quote, with PCCS collateral (Phala `dcap-qvl` `sample/sgx_quote`) | pinned Intel SGX Root CA → Processor CA → PCK | PCK chain with PCK and root CA CRLs, QE report signature by the PCK, QE report data = SHA-256(attestation key ‖ auth data), quote signature over header + 384-byte enclave report, SGX TCB info v3 and QE identity v2 signatures and windows, FMSPC binding. Verdict `ConfigurationAndSWHardeningNeeded`, advisories `INTEL-SA-00289`, `INTEL-SA-00615`, platform `ConfigurationAndSWHardeningNeeded`, QE `UpToDate`, data number 17: the same verdict as dcap-qvl's own test |
| SGX quote v3 from an **Occlum** enclave (Trustee `occlum_quote.dat`) | Intel SGX Root CA → **Platform CA** → PCK | chain, QE report and quote signatures, platform-instance PCK data. No collateral is published with it, and the other sample's Processor CA CRL does not cover it, so the collateral path refuses it |
| SEV-SNP **report version 6** | (no real sample exists; see Gaps) | real Oak Genoa and Turin v5 reports relabelled v6 parse with the v6 layout, re-encode byte-for-byte, keep their mitigation vectors, and then fail the real AMD signature (`BadSignature`): the layout is right on real bytes and the signature covers the version. Generated v6 reports (Milan, Genoa, Turin, Venice with extended TCBs) verify end to end against generated chains |
| **Arm CCA token + CoRIM** platform and realm reference values (Veraison `services` arm-cca corpus: `cca-good.cbor` + `corim-cca-platform-valid.cbor` + `corim-cca-realm-valid.cbor`) | CoRIM attest-key triple → endorsed CPAK | platform signature by the CoRIM-endorsed key (implementation ID + UEID instance ID), 4 software components, config, manufacturing config, TBB RoTPK, realm RIM, REM0..3, personalization value: all matched |
| Veraison end-to-end inputs (`end-to-end/input/cca-*.cbor`) | CoRIM-endorsed CPAK | strict appraisal (signer IDs compared) of all 13 software components and the config; the bundled realm CoRIM is for a different RIM, so the realm is `ReferenceValueMissing` (Veraison reports it unmatched too) |
| TF-RMM token (`cca-token-01`, wave seven) against the end-to-end CoRIM | the same TF-A key | the CoRIM's attest key verifies it; its components (BL, M1, …) are not the endorsed ones: `ReferenceValueMismatch` |
| Veraison `corim` profile testcases and `signed-good-corim.cbor` | the published test JWK | valid platform/realm profiles accepted; invalid digests, implementation ID, missing RIM and RIM size refused; COSE_Sign1 CoRIM (ES256, `application/rim+cbor`, corim-meta validity) verifies, and a flipped byte in it fails |

Every vendor file's source, commit, licence and SHA-256 are in
`crates/vesper-aegis/tests/data/vendor/README.md` (section "Wave eight
additions"): Phala dcap-qvl (MIT), Confidential Containers Trustee,
Veraison `services` and `corim` (Apache-2.0).

## What was built

### `sgx` (new, `src/sgx.rs`)

- `SgxQuote::parse` / `parse_padded` / `parse_prefix`: header (version 3,
  attestation key type 2 = ECDSA P-256, reserved field zero, Intel QE
  vendor ID), `SgxReportBody` (CPUSVN, MISCSELECT, attributes, MRENCLAVE,
  MRSIGNER, ISV product ID and SVN, report data, all reserved ranges
  checked zero), signature data with the PEM certification data (type 5)
  directly, no type-6 wrapper as in TDX.
- `SgxTcbInfo::parse`: SGX TCB info v3 (`id` `SGX`, `tcbType` 0, unknown
  JSON fields refused). QE identity v2 with `id` `QE` through the existing
  `QeIdentity` parser (`parse_with_id`).
- `evaluate_sgx_tcb`: Intel's component-wise SGX SVN and PCESVN rule, then
  convergence with the QE status (`TcbStatus::converge`), as for TDX
  without the TDX components and module identity.
- `verify_sgx_quote_signatures_only` and `verify_sgx_collateral`: the
  same PCK, CRL, QE and collateral checks as TDX (shared code in
  `intel.rs`, now `pub(crate)`), returning `VerifiedSgxQuote`.
- `SgxEnclavePolicy`: MRENCLAVE, MRSIGNER, product ID, minimum ISV SVN,
  report data; requires INIT and rejects debug enclaves unless allowed.

### `snp` version 6

- `SNP_REPORT_MAX_VERSION` = 6. `SnpReport` gains `current_etcb`,
  `launch_etcb`, `committed_etcb` (`Option<SnpEtcb>`, at 0x220, 0x240,
  0x260). They are read only on Venice (family 1Ah, model 50h–5Fh or
  C0h–CFh, `SnpReport::is_venice`) with version ≥ 6; everywhere else those
  bytes must be zero. The 24 bytes before and 32 after are reserved.
- `SnpEtcb`: 14 SVN bytes, bits 255:112 reserved (non-zero is
  `NonZeroReserved { field: "ETCB bits 255:112" }`), `at_least` is
  component-wise.
- `verify_snp_collateral` refuses Venice with
  `MissingCollateral { what: "pinned AMD Venice ARK and VCEK TCB layout (not published)" }`.

### `corim` (new, `src/corim.rs`)

- Parsing: unsigned CoRIM (tag 501) with CoMIDs (tag 506), and signed
  CoRIM (COSE_Sign1, tag 18, content type `application/rim+cbor`,
  corim-meta at header label 8 with signer and validity checked against
  `now`) via `Corim::verify_signed`. Environments (class with bytes, UUID
  or OID ID, vendor, model; UEID or bytes instance; group), measurements
  (mkey, version, digests with named-info algorithms 1/7/8, raw value,
  masked raw value, name, cryptokeys), reference-value triples and
  attest-key triples. Bounded: 256 KiB, 1024 items per collection;
  every failure is a typed `ParseError` or `AttestError`.
- `CcaCorimStore::add`: validates a CoRIM against the Arm CCA endorsement
  profiles (`tag:arm.com,2025:cca_platform#1.0.0`,
  `tag:arm.com,2025:cca_realm#1.0.0`) and adds platform references
  (software components, config, manufacturing config, RoTPKs by
  `cca.rotpk.{CM,DM}.n`), realm references (RIM, REM0..3, RPV) and
  endorsements (implementation ID, UEID instance ID, PEM SPKI P-256/P-384
  key) atomically: a CoRIM that fails anywhere adds nothing.
- `appraise_cca` / `verify_cca_with_corim`: verify the token with the
  endorsed key and appraise it. Results are `CcaAppraisal` (which
  references matched, which REMs were constrained).
- `cca.rs`: platform claims now include software components (2399),
  config (2401), manufacturing config (2403) and TBB RoTPKs (2405), with
  round-trip encoding.
- New typed errors: `AttestError::ReferenceValueMismatch`,
  `ReferenceValueMissing`, `UnsupportedReferenceValue`;
  `Format::SgxQuoteV3`, `Format::Corim`.

### Lean (`lean/AegisCollateral.lean`)

Three new theorems about the reference-value appraisal model:
`appraise_sound` (an accepted measurement is literally a reference value),
`appraise_refs_mono` (removing references never turns a rejection into an
acceptance) and `appraise_rejects_unknown`. The SGX evaluation is the
existing evaluation model without TDX components and module identity; the
module doc says so. `tests/lean_model.rs` now audits 15 theorems. These
are theorems about the model, not the Rust.

## Checks

| Command | Result |
| --- | --- |
| `cargo test -p vesper-aegis --locked` with `VESPER_LEAN` set to Lean 4.24.0 (release zip) | all pass, including `sgx_quote_v3` (5), `snp_v6` (3), `corim_cca` (7) and the Lean differential |
| `lean lean/AegisCollateral.lean` (4.24.0) | compiles; axioms only `propext`, `Quot.sound` |
| `cargo +1.100.0 clippy -p vesper-aegis --all-targets --locked -- -D warnings` | clean |
| `cargo +1.100.0 clippy --workspace --all-targets --locked -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `python3 tools/bazel/sync_build_files.py --check` | in sync (no BUILD change: all data is inside the crate) |

Negative tests (all typed errors, no panics):

- SGX: flipped bytes anywhere in the signed header and body, the
  attestation-key signature and the QE report; wrong QE vendor ID,
  version, key type and a TDX TEE type in the reserved field; truncation
  anywhere and non-zero trailing bytes; a TDX quote; collateral outside
  its windows, tampered TCB info and QE identity bodies, correctly signed
  TDX collateral in place of SGX collateral, and a PCESVN bump that leaves
  no level matching; 600 random mutations and truncations over both
  quotes.
- SNP v6: ETCB bytes on non-Venice, ETCB reserved bits, reserved ranges,
  versions 7, 8, 0x100 and `u32::MAX`, flipped ETCB SVN, Venice v5 with
  ETCB bytes, 500 random mutations.
- CoRIM: every Veraison "bad" arm-cca CoRIM (no class, bad instance,
  cryptokeys, mkey, no digests, no raw value, bad trust-anchor cert,
  realm without RIM or REMs, …) refused at `add`; per-field tweaks of the
  stored references (digest, algorithm, name, version, signer ID, config,
  manufacturing config, RoTPK hash and index, REM, RPV) give
  `ReferenceValueMismatch`; unknown implementation ID or RIM give
  `ReferenceValueMissing`; flipped token bytes fail; random CBOR and
  truncations never panic.

## Decisions

- **SGX reuses the TDX collateral types** (`SgxCollateral = TdxCollateral`)
  and the shared PCK, CRL and QE code, so SGX and TDX cannot drift.
- **Debug enclaves are refused by default** in `SgxEnclavePolicy`.
- **Venice is refused for collateral**, not guessed: the ETCBs parse and
  are covered by the report signature, but without a published ARK and
  VCEK TCB extension layout nothing can bind them.
- **Masks never loosen a comparison.** A masked raw value's mask is
  validated (same length as the value, not empty) but the raw bytes are
  compared in full, as Veraison does: a published all-zero mask would
  otherwise match every configuration. Config and manufacturing config
  must match exactly.
- **The digest algorithm must agree.** When the token names the algorithm
  (measurement description), it must equal the reference's; the
  algorithm and digest length are checked again at appraisal because the
  store's fields are public.
- **RoTPKs**: when references list RoTPKs, the token must report them and
  every one must be listed.
- **Signer IDs are compared by default.** Veraison's arm-cca corpus uses
  CoRIM signer IDs that differ from its own evidence, and Veraison does
  not compare them; `CcaAppraisalOptions { compare_signer_id: false }`
  reproduces Veraison's behaviour and the test uses it for that corpus
  only. The strict default rejects it, and the test asserts that too.
- **Unknown measurement-value keys and `authorized-by`** give
  `UnsupportedReferenceValue` rather than being skipped.
- **A missing REM constraint is unconstrained**, as in the profile.
- The RPV may be 1..=64 bytes (the Veraison `corim` valid realm uses 44).
- No new dependencies: `coset` (and its `ciborium`), `ring`,
  `rustls-webpki`, `rustls-pki-types`, `spki`, `x509-parser`,
  `serde_json`. No root patch.

## Gaps

- **No real SEV-SNP version-6 report** is published anywhere reachable
  from the sandbox (Project Oak, Trustee, go-sev-guest, VirTEE `sev` and
  `snpguest`, Contrast, the Veraison repositories and a web search). v6 is
  tested with relabelled real v5 reports and generated chains.
- **No AMD Venice ARK or VCEK TCB layout is published**, so Venice
  reports cannot be verified against AMD.
- **The CCA tokens and CoRIMs are not from silicon**: Veraison and TF-A
  reference keys only. No silicon vendor publishes CCA endorsements.
- **Endorsed-value triples (CoRIM key 1) and conditional endorsements are
  parsed past but not interpreted**; only reference-value and attest-key
  triples are used. CoSWID and CoTL tags inside a CoRIM are not
  appraised.
- Only the CCA profiles are appraised; CoRIMs for other environments parse
  but are not used.
- SGX collateral is checked for the Processor CA sample only; the Occlum
  quote has no published collateral.
- No TEE hardware was used anywhere.
