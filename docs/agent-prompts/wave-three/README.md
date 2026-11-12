# Third parallel implementation wave

Status: **ready to launch**. Each linked file contains a complete standalone prompt
to copy into a new agent session. All seven can run concurrently under the frozen
[contracts](../../rfcs/wave-three-contracts.md); there is no source/backend schema
writer overlap. Start from fresh `origin/main` containing **`02b3a1d175efb5d2561b7fa5b0aaf80121ae1bc6`** or later.

The previous seven handoffs are integrated and registered. Full Lean AST emission
is now implemented: both lowerers construct nodes and only the module boundary
prints source. That emitter and TUF work remain coordinator-owned.

| Lane | Copyable prompt | Exclusive boundary |
| --- | --- | --- |
| 1 | [Source traits and models](01-source-traits-models.md) | Additive frontend AST/syntax, checker, interface, Pact bridge/root-free refactor; no backend writers |
| 2 | [Source-native suspension](02-source-native-suspension.md) | Native compiler and new source session helpers; fixed IR and fiber ABI v2 |
| 3 | [Cached Forge macros](03-cached-forge-runner.md) | New Forge runner; real WASI/CAS execution and fresh authority gating |
| 4 | [Crash/WAL/network simulation](04-crash-wal-network-simulation.md) | Source simulator and bounded model library; SimPulse kernel read-only |
| 5 | [SIMD kernel model](05-simd-kernel-model.md) | New Lean algorithm/bounds proofs and model tests; machine codegen read-only |
| 6 | [macOS AArch64 fibers](06-macos-arm-fibers.md) | Fiber platform implementation/tests; native compiler read-only |
| 7 | [Prism connections and routing](07-prism-connection-routing.md) | Existing Prism codec/router/state machine; no socket/source integration |

Use a separate checkout/worktree and build target per session. Do not point
diverging compiler worktrees at one `CARGO_TARGET_DIR`; it caused mixed-artifact
validation during the prior integration. Small debug-disabled targets and two
build jobs are sufficient for isolated checks.

Lane 6 benefits from Apple-silicon execution or existing macOS CI. A Linux-only
agent can finish the port/code and cross-checks, but must report actual Darwin
runtime validation as pending. No other lane waits for that port.

The coordinator owns root registration/dependencies/lock, CLI/module wiring,
existing CI/global docs and any requested shared-schema patch. Lane 1 must solve
the root-to-Pact dependency direction; it must not add a compiler dependency on
`vesper-pact-bridge`, which already imports the compiler. Source traits normalize
to existing core nodes. Lane 2 consumes those unchanged nodes and ABI v2; lane 6
preserves that ABI/API while widening direct-fiber platform support.

Merge each finished lane against its actual base, run focused checks, then run
integrated compiler/Lean/native/platform validation. Keep macro execution caches
separate from session-bound approval; keep simulation crashes separate from
source cancellation. See [the integration evidence](../../parallel-results/wave-two-integration.md)
for current guarantees and limits.
