# Verified bootstrap: Stage 2/3 fixed-point gate, DDC cross-check and the bootstrap lock (wave eight, lane 1)

Claim: #450. Base: `1094e649c95e4d81eef4e705d13882ab1339542d` (`origin/main`, a
descendant of the required `ea1ed29`). Design: §42.2, §42.4 and §42.5 of
`docs/specification.txt`.

## What was delivered

| Item | Where | Status |
| --- | --- | --- |
| §42.2 lock parser and validator | `crates/vesper-codegen-verified/src/lock.rs` | Done: exact §42.2 keys (unknown or missing keys rejected), `blake3:` / `mif:blake3:` lowercase digests, scoped package name, canonical `MAJOR.MINOR.PATCH`, axioms limited to `propext`, `Quot.sound`, `Classical.choice` without duplicates, quorum 2..=64, 64 KiB cap, and the three digests must be one fixed point. Canonical writer round-trips. The verbatim §42.2 listing parses. |
| §42.5 fixed-point / DDC gate | `src/audit.rs` (`audit_ddc_bootstrap_quorum`) | Done: source Merkle root, Lean spec digest, axioms used, Stage 2 = Stage 3, Stage 3 = Path B, artifacts against the lock, builder quorum, TEE binding, rewrite hook. Artifact bytes are hashed by the gate, never taken as digests from the caller. |
| Builder-signature quorum | `src/audit.rs` | Reuses `vesper_loom_trust::verify_attestations` read-only (strict Ed25519, configured operators, validity windows). The signed identity binds package, version, the source root, Stage 2 as the builder digest and Stage 3 as the output object. Required operators = max(lock `lila_builder_signatures`, policy minimum). |
| Rewrite-certificate hook | `verify_rewrite_certificate_chain` | Typed `RewriteCheckerNotWired`. The checker is Codex's (`src/optimize*`, `lean/RewriteWitness*`) and lives in the root package, which a library crate may not depend on. Not reimplemented. |
| TEE binding | `TeeEvidence` | The gate does not verify quotes. A caller that verified a TDX/SNP quote (for example with `vesper-aegis`) passes its report data; the gate requires its first 32 bytes to be the fixed-point digest. A lock with `aegis_tdx_snp_attested = true` and no evidence fails. |
| Seed reproducibility record | `src/seed.rs`, `bootstrap/stage0_seed.lock` | Done (schema `vesper.bootstrap.stage0-seed/1`). |
| Driver | `bootstrap/run.sh`, bin `vesper-bootstrap` | Done, see below. |
| Path B reference | `lean/BootstrapDdc/Reference.lean` | Done for the corpus subset, see below. |
| §42.4 theorems and gate model | `lean/BootstrapDdc.lean` | Done, see below. |

### Public API

`parse_lock`, `BootstrapLock { bootstrap: BootstrapSection, quorum: FixedPointQuorum }`
(`new`, `to_toml`), `audit_ddc_bootstrap_quorum(&lock, &BootstrapEvidence) ->
Result<AuditReport, BootstrapVerifyError>`, `BootstrapEvidence`,
`BuilderEvidence`, `TeeEvidence`, `RewriteEvidence`, `expected_build_identity`,
`source_merkle`, `verify_rewrite_certificate_chain`, `BootstrapVerifyError`.
`seed::{SeedLock, ObjectManifest, ResultTranscript, SeedError}`.

`BootstrapVerifyError` variants: `LockTooLarge`, `LockSyntax`,
`InvalidLockField`, `LockNotAtFixedPoint`, `SourceMerkleMismatch`,
`LeanSpecMismatch`, `DisallowedLeanAxiom`, `TamperedDigest`,
`StageFixedPointDivergence`, `DdcCrossCompilerMismatch`,
`InsufficientLilaBuilderQuorum`, `BuilderSignatureRejected`,
`MissingTeeAttestation`, `TeeReportDataMismatch`, `RewriteCheckerNotWired`.
The §42.5 listing's `InvalidRewriteStep` / `FinalTermMismatch` and its rewrite
rules are not here: they belong to the rewrite-witness checker Codex holds.

## Acceptance (`tests/gate.rs`, 14 tests)

The honest lock passes with three operators. A tampered lock digest fails
with `TamperedDigest`; one tampered lock field with `LockNotAtFixedPoint`; a
missing signature with `InsufficientLilaBuilderQuorum { actual: 2,
min_required: 3 }`; a forged signature or signatures over another Stage 3 with
`BuilderSignatureRejected`; a Stage 2/3 mismatch with
`StageFixedPointDivergence`; a Path B mismatch with `DdcCrossCompilerMismatch`;
changed sources, spec or a disallowed axiom with their own errors; TEE missing
or unbound; supplied rewrite certificates with `RewriteCheckerNotWired`; nine
malformed-lock cases, non-UTF-8 and oversize input.

## Two-build reproducibility (measured)

`bootstrap/run.sh` builds `vesper` twice (release, `rustc 1.100.0`), each time
after deleting the target directory, with `SOURCE_DATE_EPOCH` = HEAD's commit
time, `CARGO_INCREMENTAL=0` and `--remap-path-prefix` for the checkout and
`CARGO_HOME`. Both builds use the same target path, so `RUSTFLAGS` and Cargo
metadata hashes are equal. Each seed compiles the 10 programs in
`bootstrap/corpus/` with `vesper emit --emit object`.

Result on the cloud host (4 vCPU, 15 GiB), 2026-11-14, base `1094e64`:
**the two seed binaries are bit-identical** (47.7 MB, BLAKE3 `b9e8cac3…`) and
**all ten corpus objects are bit-identical** (manifest `99128d51…`). Wall time
11 min 38 s (5 min 46 s and 5 min 31 s per seed build). Recorded in
`bootstrap/stage0_seed.lock`.

In CI: the full run is the ignored test
`model::two_seed_builds_compile_the_corpus_bit_identically`, tagged with the
reason (two clean release builds, tens of minutes). CI checks the committed
seed lock's consistency, the corpus/expected-status agreement, and Path B
(next section) on every run. Not covered: other hosts, other rustc versions,
macOS/Windows seeds, and a different checkout path (path remapping makes that
plausible but it was not tested).

What is reproducible today is the **Rust seed only**. There is no
self-hosted compiler (lane 3), so there are no Stage 1, 2 or 3 artifacts and
no real `bootstrap/stage3_ddc.lock` is committed: a lock with invented digests
would be false. The gate is exercised on fixture artifacts.

## Path B (measured)

`lean/BootstrapDdc/Reference.lean` is an independent lexer, parser and
fuel-bounded evaluator for the corpus subset (i64 functions, `let`, `if`,
recursion, unary minus, checked `+ - *`, truncating `/ %`, comparisons,
`handle Fail[ArithOverflow | RuntimeError]` with a value handler). It imports
only `Init`. `run.sh` compiles it with `lean -c` and compiles and links the C
with the distribution **gcc 13.3.0** against Lean's `libleanshared` (no rustc,
no Cranelift; **CompCert is not packaged here**, gcc is the stand-in). The
Lean runtime library itself was built by Lean's release toolchain (LLVM), so
Path B shares LLVM lineage with rustc at the runtime-library level; this is a
gap against a strict DDC.

Path A results are the exit statuses of the seed-built executables; Path B
prints `main`'s value mod 256 (the same status). All 10 programs agree
(`bootstrap/corpus/expected-status.txt`). This is a semantic cross-check on
program results, not the §42 digest equality Stage 3 = Stage 3′, which needs a
self-hosted compiler and its Lean model.

## Lean (`lean/BootstrapDdc.lean`, Lean 4.24.0)

`end_to_end_verified_codegen_simulation` and
`ddc_eliminates_seed_trojan_on_compiler` as in §42.4. A model `gate` of the
Rust check order over abstract artifacts, digests and verified operators, with
`gate_accepts_only_fixed_point`, `gate_rejects_stage_divergence`,
`gate_rejects_ddc_mismatch`, `gate_rejects_missing_signature` (distinct
operators, so one operator signing twice counts once),
`gate_accepts_identical_artifacts` (under an explicit injectivity hypothesis
for the digest) and `gate_eliminates_seed_trojan`, which discharges the DDC
theorem's hypotheses from an accepted gate. No `sorry`/`admit`/axioms; the
only axiom used is `propext` (two theorems). These are theorems about the Lean
models, not about the Rust crate, BLAKE3 or any compiler.

## Checks run

- `cargo test -p vesper-codegen-verified` with `VESPER_LEAN` set: gate 14,
  model 6 (+1 ignored), seed 3, all pass.
- `cargo +1.100.0 clippy -p vesper-codegen-verified --all-targets -- -D warnings`: clean.
- `cargo fmt --all -- --check`, `vesper fmt --check bootstrap/corpus/*.ves`: clean.
- Workspace-wide `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`
  and `cargo test --workspace --locked`: see the PR; CI's required checks run them.

## Root patch

Own commit: `crates/vesper-codegen-verified` in `members` and
`default-members` before `experiments/native-pulse`, `Cargo.lock`, and the
crate's `BUILD.bazel` (from `tools/bazel/sync_build_files.py`, plus
`extra_test_data = ["//:lean", "//bootstrap:files"]` and `copy_test_cwd` for
the model test). `bootstrap/BUILD.bazel` exports the corpus and locks. No new
external dependencies (`blake3`, `toml 0.8.23`, `serde`, `ed25519-dalek` dev
are already locked). No unsafe code.

## Remaining gaps

- No Stage 1-3: needs lane 3's self-hosted compiler; then the driver's corpus
  becomes the compiler source and the seed lock becomes the real §42.2 lock.
- Rewrite-certificate checking is not wired (Codex's checker is in the root
  package).
- TEE quotes are verified by the caller, not here; the Lila builders are
  configured keys in fixtures, not independent clean-room builders.
- Path B uses gcc, not CompCert, and links Lean's LLVM-built runtime.
- The reference covers only the corpus subset of Vesper.
