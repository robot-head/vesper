# Hermetic WASI macro/build host

Suggested branch: `external/forge-wasi`.

```text
You are implementing one bounded milestone in https://github.com/robot-head/vesper.
Fetch and branch from origin/work in a NEW cloud checkout or dedicated git worktree. Do not share a checkout with another agent.
Read docs/specification.txt (expanded design), docs/implementation.md, crates/README.md, and docs/parallel-work.md.
The existing compiler is a working checkpoint, not a full implementation. Keep its validated dependency versions; illustrative scaffold versions in the design are not an instruction to downgrade dependencies.

Ownership and delivery:
- Edit only the paths assigned below. Root Cargo.toml/Cargo.lock, shared AST/HIR/MIR, CLI, and CI are owned by the integration coordinator.
- For a new crate not yet registered in the workspace, use a temporary [workspace] table in its own Cargo.toml and test it independently. The coordinator will register it, remove that temporary table, and generate the root lockfile.
- Make a real, reviewable implementation with meaningful positive/negative tests. Do not replace missing functionality with stubs, fabricated certificates, or an interpreter fallback claimed as native code.
- Keep unsupported behavior explicit. Do not claim hardware isolation, formal soundness, zero allocation, deterministic exploration, or performance targets without appropriate evidence.
- Do not modify the Google Doc. Record ambiguities and proposed decisions in your report.
- Commit and push only your assigned branch (or supply a patch if push access is unavailable); return its SHA or PR, public API, exact validation commands/results, integration requirements, and remaining limitations. Do not merge into work.
- Do not send messages to third parties or publish packages. Use local fixtures for network/provenance tests.

Your assignment:
Own crates/vesper-loom-sandbox/**, docs/parallel-results/forge-wasi.md.
Design §§8.2, 16.4. Implement the shared Wasmtime component execution host for a bounded deterministic build/macro fixture protocol.
Enforce fuel and memory limits, explicit read-only inputs, a dedicated writable output directory, no ambient filesystem/network/environment authority, and deterministic clock/random policy.
Run real WebAssembly component fixtures to test allowed reads/output and denied traversal, filesystem writes, network, environment access, fuel exhaustion and memory exhaustion. Distinguish preview1 modules from the requested wasm32-wasip2 components.
Do not merely configure a policy object without executing denial tests. Scope-set hygiene and compiler macro expansion remain separate; define the versioned token/input-output transport for their later integration.
Keep dependency additions in your crate; do not downgrade the existing compiler stack.
```

