# Parameter macro CI reconciliation (#181, stage3)

The coordinator imported the reviewed proof/preview stages as ea24502 before
this stage began. The published `docs/rfcs/bazel-stack41-contracts.md` fixes its
ownership and execution interfaces. All imported gates and source changes remain
unchanged.

The new executable `scripts/test-source-macro-parameters.sh` uses the existing
Bazel helper for `//:bin/vesper`, or its explicit
`VESPER_SOURCE_MACRO_PARAMETERS_BINARY` override. It preserves the actual Python
harness, executable suffix handling and temporary-directory cleanup. BuildBuddy
and macOS execute the wrapper after proof/preview acceptance; Windows uses its
actual Cargo-built `target/debug/vesper.exe` with the same Python harness.
No later-stage script or existence conditional is introduced.

Validation passes:

- Actual wrapper override against the coordinator's read-only root executable:
  65commands,9stdio sessions,47JSON-RPC requests and9separate expanded programs
  checked and run to42. The executable is from the later links integration, not
  a fresh build of this imported main tree.
- Shell syntax, whitespace, executable mode and workspace formatting checks.
- Duplicate-key-rejecting YAML parsing and explicit Linux/macOS/Windows wiring
  assertions, including preservation of proof/preview calls.
- Locked no-dependency metadata discovers `source_macro_parameters`,
  `typed_function_parameters`, `forge_parameters` and `forge_parameters_cli`.
  BUILD coverage synchronization passes without any BUILD or dependency edit.

No Cargo cache write, installation, Bazel default-path/RBE or hosted platform run,
credential/admin edit or source/native change was performed. The coordinator is
separately investigating failed BuildBuddy checks on an earlier published proof
head; these local acceptance results do not claim those failures are resolved.
Protected-head publication/checks and the remaining #181 stages stay with the
coordinator.
