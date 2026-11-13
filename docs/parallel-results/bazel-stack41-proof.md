# Proof-stage Bazel stack reconciliation (#181)

This stage follows the published `docs/rfcs/bazel-stack41-contracts.md` from
commit9882a9a. Coordinator importb2deaf6 contains the landed Bazel migration,
protected formatter PR115 and the owner's README/Warp changes. Those imported
changes are preserved. No preview/parameter/tuple/doc-link stage is wired before
its implementation reaches this branch.

The proof wrapper now obtains its default executable through the existing
`scripts/bazel-lib.sh` helper and `//:bin/vesper`; its explicit
`VESPER_FORMAT_PROOF_BINARY` override, executable suffix handling, scratch cleanup
and actual bounded Python/Lean acceptance remain intact. BuildBuddy's native
acceptance action separately installs Lean because it uses a different VM from
the all-target action. Proof acceptance receives the installer's actual absolute
Lean path. macOS adds the same wrapper to the existing Bazel acceptance step.
Windows retains the landed Cargo build/test lane and runs the actual Python
proof harness against `target/debug/vesper.exe`, using `elan which lean`.

The shared Lean runner now includes `StylusAndLintSafety`. The formatter BUILD
adds only the demonstrated `//:lean` compile-data dependency: its real
`proof_lean.rs` integration test includes the root model with `include_str!`.
The root BUILD already covers root Lean/fixtures/scripts, and Cargo metadata
already discovers the new formatter and CLI test targets. The separate own
`format-proof.yml` is removed after folding these gates into the shared jobs.
Existing default-CLI dependency assertions remain in the unchanged source-macro
and Stylus workflows; no Wasmtime, proof policy or source implementation changes
are introduced here.

Validation on this stage:

- `bash -n` passes for both edited shell scripts; whitespace and workspace
  `cargo fmt --all -- --check` pass.
- Both edited YAML files parse with duplicate keys rejected. Direct structural
  assertions confirm separate native-action Lean installation before proof
  acceptance, its absolute Lean environment, macOS wrapper execution and Windows
  Python/actual-executable execution. No existence conditional hides a gate.
- Locked, no-dependency Cargo metadata succeeds and discovers `proof_lean`,
  `proof_model` and `format_proof_cli`. BUILD/source-input assertions and
  `python3 tools/bazel/sync_build_files.py --check` pass.
- The edited wrapper's explicit override passes28actual commands, three kernel
  reports and independent witness replay using the coordinator-supplied read-only
  executable `/workspace/vesper/target/parallel-next/debug/vesper` and actual
  Lean4.24.0. That executable was built from the coordinator's links integration;
  this stage changes only CI/BUILD/scripts, and does not claim a fresh build of
  the imported main code or a Bazel default-path execution.
- Actual `lean --trust=0 lean/StylusAndLintSafety.lean` passes; its nine printed
  axiom reports are empty.

No Cargo cache writes, installation, dependency, domain, README, credential or
administrative setting edits were performed. Bazel is not installed in this
executor; no local Bazel build, remote execution or hosted macOS/Windows result
is claimed. The coordinator will publish and check the current protected heads.
The repository-owner setup for BuildBuddy and required-context migration remains
tracked with #171/#181; no missing check is fabricated or bypassed.
