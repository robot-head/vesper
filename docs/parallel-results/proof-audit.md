# Local Lean artifact replay and axiom audit

Coordinator work while all seven external lanes are active. The command and driver live in `src/proof_commands.rs`; an embedded trusted helper lives in `tools/proof-audit/Audit.lean`. The compiler extractor, arithmetic migration and source IR are unchanged.

`vesper proof-audit Module --root artifacts --theorem Namespace.name` snapshots existing local artifacts, imports their data without package plugins/environment-extension initialization, constructs an independent trusted-toolchain base, and kernel-replays artifact declarations with Lean 4.24.0's `Environment.replay`. Requested names must be artifact theorems, every replayed constant is checked against the input metadata, and unsafe/partial artifacts are explicitly unsupported.

The default axiom policy is empty; explicit `--allow-axiom` names remain visible, and `sorryAx` is always forbidden. Reports bind copied implementation files, actual imported artifact parts, the actual Lean binary/version and the auditor code to BLAKE3 inventories. Reports always have `cache_eligible: false`; full toolchain/native-environment provenance and the specification's reusable proof-certificate contract remain future work.

Validation used real Lean 4.24.0:

- Six CLI integration tests passed, covering foundation theorems, transitive assumptions, changed imported definitions, forged axiom-free false artifacts, missing/non-theorem names, unsafe/partial artifacts, `sorryAx`, package initializers and argument limits.
- The seven existing generic CLI tests passed.
- Five binary unit tests passed, including existing DOT escaping and new name validation, panic-output handling and killing/reaping an overdue process (18 tests total).
- Workspace formatting and strict all-target Clippy passed.
- Helper exploration additionally rejected forged cyclic theorems, constructor/recursor metadata and trusted-name collisions. Plain import/axiom collection/`--trust=0` accepted a controlled forged artifact; replay rejected it, which is why the implementation does not treat import success as verification.

The selected Lean toolchain, kernel, deserializer and host OS remain trusted. This is local logical auditing, not an OS sandbox, source compiler-preservation/termination proof, authenticated package proof or publication operation. Ordinary snapshot/import limits and checking-process timeout are documented in [proof audits](../proof-audits.md). Platform runtime validation beyond this Linux host remains outstanding.
