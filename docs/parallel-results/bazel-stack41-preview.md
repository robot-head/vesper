# Current-source preview CI reconciliation (#181, stage2)

The published `docs/rfcs/bazel-stack41-contracts.md` governs this stage.
Coordinator importeb14e71 brings the reviewed proof-stage Bazel integration into
the existing preview branch. This stage changes only the preview wrapper/shared
acceptance calls and removes the redundant own `macro-preview.yml`; all proof
gates and imported main/source changes remain intact.

`scripts/test-macro-preview.sh` now uses the landed Bazel helper to obtain its
default `//:bin/vesper` executable. Its explicit `VESPER_MACRO_PREVIEW_BINARY`,
executable suffix handling, real Python harness and temporary-directory cleanup
are preserved. BuildBuddy's native action and the existing macOS acceptance step
invoke that wrapper. Windows retains Cargo and runs the same Python harness
against its actual `target/debug/vesper.exe`. No later-stage harness is called
before its implementation reaches this branch, and no existence conditional
masks a gate.

Validation passes:

- Actual wrapper override against the coordinator-supplied read-only executable
  `/workspace/vesper/target/parallel-next/debug/vesper`:20commands,8stdio sessions,
  70JSON-RPC requests,3explicit normal expanded-program runs and1intentional
  unhandled-entry refusal. Accepted previews retain real current-source
  association; ordinary semantic providers remain on original source.
- Shell syntax, whitespace and workspace formatting checks.
- Duplicate-key-rejecting YAML parse and direct assertions for exactly one
  preview call on Linux/macOS/Windows, preserved proof calls and removed
  duplicate workflow.
- Locked no-dependency Cargo metadata discovers `database_macro_preview`,
  `macro_preview` and `macro_preview_cli`; BUILD coverage synchronization passes.
  No additional BUILD entry or dependency change is needed.

The actual executable was built from the coordinator's later links integration,
not freshly built from this imported main tree. This CI-only stage performs no
Cargo cache writes, installation, Bazel default-path execution, RBE, hosted
platform execution, source/native change or administrative/credential edit.
Those unrun build/platform gates are not claimed. Protected-head publication and
actual required checks remain with the coordinator; #181 stays open for the
remaining explicitly staged work.
