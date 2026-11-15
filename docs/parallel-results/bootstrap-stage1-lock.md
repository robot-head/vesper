# Stage 1 frontend lock and backend availability (#582)

Base: `40d0b33b8c5fb0e3a38526ec86c9e32e35df6f87`, a descendant of the
wave-nine baseline `37f7b8b`. The issue and open PR board were checked before
editing; there was no competing bootstrap PR. Changes stay in lane 5's paths.

## Result and boundary

The Rust seed builds the actual five-file `@vesper-lang/compiler` frontend
from main. For each of the ten bootstrap corpus programs, the adapter emits a
Cranelift relocatable object and runs `parse_main` through Cranelift's JIT.
Every full 63-bit CST digest equals the digest independently calculated from
the Rust parser's CST. Inputs use the same bounded 64-byte/eight-child byte
adapter as `tests/self_host_front_end.rs`. No frontend source is replaced.

The verified backend is attempted on every checked program. Its typed refusal
on this main is `Unsupported { construct: Effect, function: "parse_main",
detail: "effect handlers" }`. Only `VerifiedError::Unsupported` is recorded as
unavailable; limits, invalid IR, target errors and execution errors fail the
run. No seed digest stands in for a missing verified output.

The supported verified slice is exercised separately: the actual frontend's
six byte-classification functions, called for -1, every byte 0..255, and 256.
Cranelift and the verified executable both return checksum
`7856525685952484840`, also computed independently by the Rust adapter. The
verified executable returns all eight result bytes, rather than truncating
the result to an exit status. Both artifacts are recorded under
`kind = "lexer-byte-classes"`; these are not CST results.

**The full parser cross-backend acceptance in #582 remains blocked.** A
Cranelift-only frontend record is not a completed DDC check. The strict gate
returns `Stage1Error::BackendUnavailable`. Work in lane 6 (#583) may extend
the backend; this lane does not edit it or depend on unmerged work.

The `.o` artifacts are preserved build outputs. Their semantic comparison
uses a separately emitted JIT instance of the same checked frontend. They
are not standalone frontend executables: the parser's regions require the
host runtime. This does not implement a compiler capable of Stage 2, Lean
soundness certificates for the source compiler, CompCert DDC, independent
builders or the Stage 3 fixed point.

## Driver and lock

```
bootstrap/run.sh stage1 --record
bootstrap/run.sh stage1
bootstrap/run.sh stage1 --require-cross-backend
```

The first command intentionally records a new baseline. The second rebuilds
and requires the existing record to match before writing canonical bytes.
The third also requires every verified frontend build and fails on this
baseline. `OUT`, `WORK`, `PROFILE=release|dev`, and `CARGO_TARGET_DIR` override
the output, scratch directory, Cargo profile and build directory. Stage 0's
existing no-argument driver remains available and still requires Lean.

`bootstrap/stage1.lock` uses schema `vesper.bootstrap.stage1/1`. It binds:

- The exact historical Stage 0 parent lock bytes.
- The executable containing the Rust compiler facade that actually performs
  this frontend build (`stage1-runner`), hashed via its own executable path.
- The package manifest, five frontend sources, and the adapter source.
- The explicit target triple, each corpus/probe source, emitted artifact
  digests, full result words, and exact typed unsupported-backend reasons.

The historical parent is a lineage anchor. Its earlier seed-binary digest
does not certify the newer adapter executable. The new executable has its own
locked digest. This distinction is intentional; there is no new assertion
of two-clean-build Rust seed reproducibility or independent seed trust.

Retained artifacts are checked before regeneration can overwrite them.
Rebuilt evidence is checked again against the record. A changed executable
fails with `DigestMismatch { field: seed_binary }`; a retained object or
verified executable with changed bytes fails with its artifact field. A
clean scratch directory is supported. Failed strict checks do not publish a
replacement lock.

Release dependency paths are remapped to `/vesper` and `/cargo`, incremental
compilation is disabled, and the epoch defaults to the historical seed's
epoch. Cargo's artifact layout is discovered rather than assuming a `deps/`
directory (Cargo 1.100 can use `build/<crate>/<hash>/out`). The target triple
is explicit for object emission. No timestamps, absolute paths, current HEAD
or current date enter the lock. This does not imply universal host equality:
the Rust toolchain, target, profile, dependency feature resolution, host
linker, standard library and executable container affect the seed digest;
backend/toolchain changes can affect generated objects. Only the local
Linux x86-64 run is measured. Other hosts must deliberately record their own
seed baseline; the gate rejects silent seed substitution.

## Public API and rewrite checker

`vesper_codegen_verified::stage1` exports `Stage1Lock`, `Stage1Evidence`,
`ProgramEvidence`, `FrontendOutput`, `ResultKind`, `VerifiedRecord` and
`Stage1Error`. `record` measures artifact/input bytes and compares full
results, `verify` rehashes actual evidence against a lock, and
`require_cross_backend` rejects any unavailable build. Parsing rejects unknown
fields, malformed digests, duplicate/unsorted programs, negative CST digests,
empty evidence and locks over 256 KiB. The CLI's
`vesper-bootstrap check-stage1-lock FILE` invokes the strict availability
gate; it does not re-execute the builds.

The adapter calls the root's public `optimize::optimize` pipeline, which
installs pure integer rewrites only after the actual bounded rewrite witness
checker accepts them, then rechecks the module before code generation. No
checker is duplicated. The auditor crate's legacy
`verify_rewrite_certificate_chain` hook still returns `RewriteCheckerNotWired`:
it cannot depend on the root package and there is no certificate interchange
adapter in that crate. No claim is made that all IR optimization is verified;
this driver disables the additional Place-IR optimizer for both backends.

## Validation

- Focused auditor tests cover accepted cross-backend evidence, canonical
  round trips, changed seed/parent/source/corpus/artifacts, mismatched full
  results, unsupported-backend refusals, malformed/empty/duplicate/oversize
  inputs and failure to certify unavailable artifacts.
- The root adapter test measures all ten real CST cases and the supported
  lexer checksum on both backends. A direct Rust test run passed in 156.29 s.
- Cargo focused tests (32 passed, one historical two-seed-build test ignored),
  strict focused Clippy, workspace formatting, explicit
  adapter Rustfmt, shell syntax, Python compilation, Vesper fixture formatting,
  BUILD-file synchronization and whitespace checks pass.
- `bootstrap/run.sh stage1 --record`, followed by `bootstrap/run.sh stage1`:
  pass; lock bytes are identical. SHA-256 of the committed lock:
  `b38437b85d500888c18f0ff79fd40d3c7fdb6e608c5977187ee12492608e8917`.
- Actual retained Cranelift object and verified executable tampering, and an
  executable copy with appended seed bytes: rejected with the corresponding
  `DigestMismatch` field. The strict cross-backend command rejects the parser
  with `BackendUnavailable`; the lock remains byte-identical on every refusal.
- `bazel test //bootstrap:stage1_test --jobs=2 --local_test_jobs=1`:
  pass under pinned Bazel 9.3.1 after a cold toolchain build. This exercises
  the real frontend and verified lexer probe inside the Bazel sandbox.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: pass
  with Rust 1.100.0 (Cargo separately warns about an existing unused
  `serde_json` manifest dependency in `vesper-pact`). Direct `clippy-driver`
  metadata checks of the adapter, with and without `--test`, also pass with
  `-D warnings`.
- `cargo test --workspace --locked`: stops in
  `lean4lean42_acceptance::genuine_three_model_replay_and_independent_refusal_matrix`
  because `VESPER_LEAN4LEAN42_ROOT` must name a genuinely prepared candidate.
  The required environment is absent; that test intentionally hard-fails.
  Its test binary reports 15 passed and one failed. No proof test, candidate,
  validator or environment requirement was changed.

The new Bazel `//bootstrap:stage1_test` runs this real cross-backend slice on
Linux with the existing medium timeout (300 s), one compiler worker and an
estimated 1 GiB memory scheduling reservation. It needs the root compiler
facade but introduces no Cargo workspace member or dependency. Portable
auditor tests continue through the existing crate target on all CI platforms.
There is no root manifest/lockfile patch, no unsafe code and no new Lean model
or proof claim. `//bootstrap:clippy` and `//bootstrap:rustfmt` keep the standalone
adapter in CI's normal lint checks.
