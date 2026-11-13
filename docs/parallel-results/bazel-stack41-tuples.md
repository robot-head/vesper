# Tuple macro CI reconciliation (#181, stage4)

Coordinator import570cee8 brings the reviewed proof, preview and parameter stages
into the existing tuple branch. This stage follows the published
`docs/rfcs/bazel-stack41-contracts.md`; prior gates and source implementation
remain unchanged.

The new executable `scripts/test-source-macro-tuples.sh` obtains `//:bin/vesper`
through the landed Bazel helper, or uses its explicit
`VESPER_SOURCE_MACRO_TUPLES_BINARY` override. Actual Python acceptance, executable
suffix handling and scratch cleanup are retained. BuildBuddy and macOS call the
wrapper after the three preceding stages. Windows retains Cargo and invokes the
same harness against its actual `target/debug/vesper.exe`. No doc-link stage or
conditional existence check is added early.

Validation passes:

- Actual wrapper override against the coordinator's read-only root executable:
  157commands,21stdio sessions,108JSON-RPC requests,23expanded programs separately
  checked and executed to42, and1intentional unhandled-entry refusal. The binary
  is from the later links integration, not a fresh build of this imported main.
- Shell syntax, whitespace, executable mode and workspace formatting.
- Duplicate-key-rejecting YAML parsing and ordered, exactly-once acceptance
  wiring assertions, including preserved proof/preview/parameter gates.
- Locked no-dependency metadata discovers `source_macro_tuples`, `typed_tuple`,
  `forge_tuples`, `forge_tuples_cli` and `tuple_analysis`; BUILD coverage
  synchronization passes. No BUILD/dependency edit is needed for these targets.

No Cargo cache write, installation, Bazel default-path/RBE or hosted platform run,
source/native change, credential or administrative setting edit was performed.
Previously failed BuildBuddy proof checks remain under coordinator investigation;
these actual local results do not diagnose or claim to resolve those failures.
Current-head publication/protected checks and the final #181 stage remain with
the coordinator.
