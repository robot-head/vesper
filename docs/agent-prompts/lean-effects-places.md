# Lean effects and mutable places

Suggested branch: `external/lean-effects-places`.

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
Own src/lean.rs, src/lean_ir.rs, tests/lean.rs, lean/EffectPlaces.lean, docs/parallel-results/lean-effects-places.md.
Design §11 and current extraction limits in docs/implementation.md.
Complete one coherent remaining extraction slice: mutable inout effect-operation arguments and nested mutable handler expressions, including mutations before and after non-tail k resumption. Preserve lexical evidence and one-shot semantics.
Use the shared Place IR where possible. Audit whether operation evidence must return explicit mutable state and how that state crosses handler boundaries. Do not paper over aliases with snapshots if the evaluator observes changes.
Add actual Lean compilation AND execution comparisons to the reference evaluator for stateful/nested handlers, disjoint projections, aborts, and continuation effects.
Retain pure extraction and existing callback/open-row tests. No sorry, axioms, admitted proofs, or claims that executable extraction proves compiler preservation.
If the full slice requires a shared IR contract change, deliver the precise interface proposal and a failing regression first; do not independently change checker/IR outside ownership.
```

