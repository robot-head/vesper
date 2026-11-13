# Protected stack integration after the landed Bazel migration

GitHub #181 was claimed before writers after refreshing open issues. External
#158/#171 is merged and closed; main7a5d917 includes its Bazel migration,
the owner's README and Warp streams, and protected formatter PR115. Preserve
those changes. The existing Codex source claims remain reserved through merge.
The documentation-link writers #177–180 retain their exclusive source paths and
do not edit CI. This separate issue owns the following integration work.

## Ownership and sequence

Root controls main imports, parent/child branch merges, publication and protected
integration. One CI writer owns only necessary BUILD data, workflow/BuildBuddy,
Lean model runner and acceptance wrapper edits below, plus its report. No core,
domain, native, main/lib registration, dependency or README changes. All source
changes imported from main are preserved. Separate worktree, no Cargo cache
writes/installations or remote-credential/admin changes by the CI writer.

Work proceeds in explicit stages; add a command only in a branch where its
actual implementation and harness exist. Do not mask missing gates with file
existence conditionals.

1. Proof142: root imports main into this proof integration branch. Add the
   necessary `//:lean` extra_compile_data to crates/vesper-fmt/BUILD.bazel:
   proof_lean.rs actually includes the new root Lean model. Convert
   scripts/test-format-proof.sh to the landed Bazel helper, fold its actual
   acceptance into buildbuddy.yaml and platforms.yml, add StylusAndLintSafety
   to scripts/ci/check-lean-models.sh, and remove the own format-proof.yml.
2. Preview156: root propagates the updated proof parent. Convert
   scripts/test-macro-preview.sh, add its acceptance to the shared jobs, and
   remove the own macro-preview.yml. Preserve current-source/authority tests.
3. Params164: root propagates the updated preview parent. Add
   scripts/test-source-macro-parameters.sh and its actual acceptance calls.
4. Tuples176: root propagates the updated parameter parent. Add
   scripts/test-source-macro-tuples.sh and its actual acceptance calls.
5. Links successor: root propagates the updated tuple parent. Add
   scripts/test-doc-links.sh and its actual acceptance calls.

Root merges these owned staged commits into the existing corresponding PR
branches, preserving ancestry. No new source implementation PR is necessary
for #181. Keep its claim open until the complete stack is reconciled.

## Fixed execution interfaces

Use the landed scripts/bazel-lib.sh `bazel_executable //:bin/vesper` to obtain
the actual executable. Existing Rust targets are discovered through Cargo
metadata; only the demonstrated fmt compile-data input needs a BUILD change.
No new Cargo dependency or toolchain is needed. Bazel's launcher supplies the
Cargo-compatible test working directory and executable PATH.

Each wrapper preserves its existing actual-binary override and otherwise builds
the Bazel binary. The overrides/harnesses are:

| Stage | Override | Python harness |
| --- | --- | --- |
| proof | VESPER_FORMAT_PROOF_BINARY | check-format-proof.py |
| preview | VESPER_MACRO_PREVIEW_BINARY | check-macro-preview.py |
| params | VESPER_SOURCE_MACRO_PARAMETERS_BINARY | check-source-macro-parameters.py |
| tuples | VESPER_SOURCE_MACRO_TUPLES_BINARY | check-source-macro-tuples.py |
| links | VESPER_DOC_LINKS_BINARY | check-doc-links.py |

Inspect actual existing script options before edits; the frozen interface is
the already-published Python parser. All accept --binary; doc links has no
--work option. Preserve bounded harness assertions and actual Lean audit.

BuildBuddy Test all targets already installs Lean and exports its absolute
path. Native and acceptance scripts runs in a separate VM and must separately
run scripts/ci/install-lean.sh, then use VESPER_LEAN from .ci-lean-path for proof
acceptance. Never assume installation or environment survives between actions.
The existing .bazelrc forwards VESPER_LEAN and keeps rbe_build tests local.

macOS retains the landed Bazel lane and actual absolute elan binary. Windows
retains the landed Cargo implementation: add Python setup and each stage's
actual Python harness against target/debug/vesper.exe after its actual Cargo
build/tests, with its real Lean path for proof. This preserves three-platform
acceptance without pretending Bazel/MSVC linking is supported.

## Validation and protected gates

Validate shell/Python/YAML syntax, BUILD/source-input coverage, locked Cargo
metadata, actual read-only executable overrides and the real Lean model. Run
the targeted actual Rust tests/Clippy appropriate to source changes. No Bazel
binary or RBE credential is currently available in this executor; installation,
full Bazel/RBE and hosted results must not be claimed without execution.

At claim time ruleset27101851 still requires `check`, macOS (AArch64) and
Windows (x86_64 MSVC), while main has moved Linux checks to BuildBuddy's Test all
targets and Native and acceptance scripts. Old pending PR runs were cancelled.
The repository owner/#171 has been notified of this exact setup gap. Root never
fabricates a check, cancels required jobs, changes credentials/admin policy or
uses bypass to merge. Current-head applicable required checks must actually
succeed before each protected merge; missing owner setup does not block useful
local implementation and review.
