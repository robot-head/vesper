# Ignis barrier graph core

Suggested branch: `external/ignis-barrier-graph`.

```text
You are implementing one bounded milestone in https://github.com/robot-head/vesper.
Fetch and branch from origin/main in a NEW cloud checkout or dedicated git worktree. Do not share a checkout with another agent.
Read docs/specification.txt (expanded design), docs/implementation.md, crates/README.md, and docs/parallel-work.md.
The existing compiler is a working checkpoint, not a full implementation. Keep its validated dependency versions; illustrative scaffold versions in the design are not an instruction to downgrade dependencies.

Ownership and delivery:
- Edit only the paths assigned below. Root Cargo.toml/Cargo.lock, shared AST/HIR/MIR, CLI, and CI are owned by the integration coordinator.
- For a new crate not yet registered in the workspace, use a temporary [workspace] table in its own Cargo.toml and test it independently. The coordinator will register it, remove that temporary table, and generate the root lockfile.
- Make a real, reviewable implementation with meaningful positive/negative tests. Do not replace missing functionality with stubs, fabricated certificates, or an interpreter fallback claimed as native code.
- Keep unsupported behavior explicit. Do not claim hardware isolation, formal soundness, zero allocation, deterministic exploration, or performance targets without appropriate evidence.
- Do not modify the Google Doc. Record ambiguities and proposed decisions in your report.
- Commit and push only your assigned branch (or supply a patch if push access is unavailable); return its SHA or PR, public API, exact validation commands/results, integration requirements, and remaining limitations. Do not merge into main.
- Do not send messages to third parties or publish packages. Use local fixtures for network/provenance tests.

Your assignment:
Own crates/vesper-ignis/**, docs/parallel-results/ignis-barrier-graph.md.
Design §19. Implement a GPU-independent typed resource/pass graph and hazard/barrier planner as the first engine milestone. Do not attempt the whole renderer.
Model resource identity, subresource/range overlap, read/write access, layouts, queue ownership, and cross-queue synchronization. Verify the design scaffold against actual D3D12 enhanced-barrier semantics rather than copying it unquestioned.
Test RAW/WAR/WAW hazards, disjoint ranges, compatible read-only reuse, layout changes, cycles and cross-queue fence ordering. Return deterministic barrier/fence plans with structured errors.
Do not claim hardware validation, DXR/DirectStorage support, native UI integration, or soundness proofs from unit tests. Windows execution is a later acceptance gate. Keep source compiler, CI and workspace edits out of scope.
```

