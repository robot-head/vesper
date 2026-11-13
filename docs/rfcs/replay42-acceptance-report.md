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
owned all-platform 900-second Bazel timeout. The final coordinator execution
measured 105.61 seconds for the Rust target (105.60 seconds in the matrix's
own wall-clock measurement). This is one Linux-host observation, not a universal
elaboration-time bound or a Bazel/platform execution claim.

## Execution evidence

The writer authored the tests, ran rustfmt and git diff --check, and reviewed
the coordinator's actual logs. The writer did not execute Cargo builds, Rust
tests, Lean compilation or proof audits. The following executions were performed
by the coordinator against the integrated real model and exporter:

| Source and evidence | Actual result |
|---|---|
| Final root `ed70d8df037dac917f314bdf69c608d86d400ea5`, `/tmp/replay42-root-final-focused.log` | 10 exporter tests and the complete kernel matrix passed; 11 passed, 0 failed, 0 ignored |
| Final kernel matrix in that focused run | 43 real Lean compilation calls, including four expected hostile-AST rejections; nine real-tool audit calls, including corrupt and missing object refusal; two missing/wrong-tool CLI calls and one forbidden-policy-option call passed their assertions |
| Earlier full root `94b704f`, `/tmp/replay42-root-regression.log` | 884 passed across 74 result-bearing targets, 0 failed, 0 ignored; includes the original complete kernel matrix at 105.72 seconds, before the additional missing-object case and final exporter metric fix |

The final focused rerun validates the combined final source and the added
missing-object negative. The earlier broad regression is evidence for its
stated source revision; it is not relabeled as a full-package rerun of the
later revision. The coordinator also reported the final formatting check
passed. Final strict Clippy was still running when this report was updated,
so no final Clippy result is claimed here.

Two fixture corrections preceded the passing runs: an unsupported Unicode
identifier was replaced with an admitted ASCII identifier while retaining a
Unicode source comment and original byte-span checks; the rejection helper
was corrected to recognize Lean's tagged `error(lean.unknownIdentifier):`
diagnostic as well as `error:`. Actual nonzero status, absent compiled output,
and all semantic rejection assertions remain required. No production semantics,
limits or axiom policy were weakened.

No macOS, Windows, Bazel or BuildBuddy execution was performed by this lane or
established by these logs. All-platform target configuration is distinct from
actual platform coverage.

## Scope

Accepted replay establishes selected mathematical-model equalities under the
explicit standard-axiom policy and pinned trusted Lean toolchain. Rust verifies
source/candidate/type/certificate binding, but this chunk does not prove that
translation in Lean, whole-compiler refinement or producer correctness.
Association hashes are diagnostic data, not provenance or approval tokens.
Even accepted local audits must retain cache_eligible:false. No self-hosting,
DDC, reusable proof-cache, native-platform or performance claim is made.
