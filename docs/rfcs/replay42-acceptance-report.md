# Independent rewrite replay acceptance (#241)

The implementation follows frozen contract
`63d60cd1b1b60f880f23c75e598e15c2383df23e`. This lane owns only
`tests/replay42_kernel.rs` and this report. Model, exporter, module registration,
Bazel timeout and general model-CI changes belong to other claims.

## Authored acceptance matrix

One Rust test runs the complete matrix serially. It resolves the actual pinned
Lean 4.24.0 binary and fails on missing/wrong default or explicit tooling. The
exact bundled static model is compiled once; ordinary model artifact files are
copied into each distinct private case root. Every generated module uses the
existing structured Lean AST and the fixed ReplayGenerated module identity.

* Four independent aggregate certificates cover all 24 catalog rules for all
  four integer kinds (96 rule/kind mappings), all five equality reasons,
  checked/wrapping congruence, reused proof edges and earlier-totality guards in
  both orientations. Every unused certificate record is verified and emitted.
* Twenty-four parsed and checked constant-fold programs use the actual public
  producer path: six operators for each kind, including full-u64 wrapping
  products. Four checked identities, prior-totality erasure, signed MIN and
  retained original Unicode-source intrinsic provenance provide additional
  genuine producer cases. Checked-overflow erasure is refused for every kind.
* Structural inspection checks named term/proof sharing, fixed manifest scope,
  root IDs/counts and independent canonical certificate/binding hashes. Separate
  prepared exports must serialize deterministically. Actual returned source
  and bundled model bytes are independently hashed.
* Actual proof-audit commands request all 33 model roots plus the generated
  theorem. The explicit genuine policy permits only Quot.sound and propext.
  Audit JSON, theorem sets, actual axiom closures, artifact fingerprints and
  unchanged audited object bytes are checked. A second valid equality changes
  source/binding and actual artifact inventory observations.
* Hostile caller-owned ASTs change the folded output, nominal U64/I64 type,
  proof reference and totality evidence. Actual Lean rejection and absent
  compiled output are required. A truncated actual compiled object must fail
  audit; removing that object while preserving the source must also fail.
  Missing/wrong tool selections and the forbidden sorryAx policy option
  must fail without invented success reports.
* An AST-only Classical.choice theorem separates kernel validity from policy:
  empty and restricted genuine policies reject it; an explicitly broadened
  fixture-only policy accepts it while retaining cache_eligible:false.

Subprocesses drain both streams and cap each capture at 8 MiB. Compilation is
guarded at 300 seconds, audit at 360 seconds, and timeout/output refusal kills
and reaps the child. Every Lean compile uses -j1 -M4096. Actual per-command and
whole-target timings are emitted for coordinator review against the separately
owned all-platform 900-second Bazel timeout. No timing has yet been measured.

## Execution evidence

Writer checks: rustfmt and git diff --check. No Cargo build, Rust test, model
compilation or proof audit has been executed by this lane. Coordinator execution
against the integrated real model and exporter is pending; this checkpoint is
not a claim that the matrix passes. Actual failures must be investigated before
changing expectations, and final timing/platform evidence will be recorded
after execution.

## Scope

Accepted replay establishes selected mathematical-model equalities under the
explicit standard-axiom policy and pinned trusted Lean toolchain. Rust verifies
source/candidate/type/certificate binding, but this chunk does not prove that
translation in Lean, whole-compiler refinement or producer correctness.
Association hashes are diagnostic data, not provenance or approval tokens.
Even accepted local audits must retain cache_eligible:false. No self-hosting,
DDC, reusable proof-cache, native-platform or performance claim is made.
