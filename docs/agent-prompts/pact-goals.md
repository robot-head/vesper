# Pact canonical trait goals

Suggested branch: `external/pact-goals`.

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
Own crates/vesper-pact/**, docs/parallel-results/pact-goals.md.
Design §16.2–16.3. Build a standalone canonical goal representation and tabled trait-resolution engine for a clearly bounded first fragment, with modal/effect arguments, associated-type equalities, canonical impl uniqueness and type-branded named-model separation.
Use stable versioned IDs/DTOs at the compiler boundary. Do not tie the solver to source-name strings or duplicate the compiler's entire AST.
Tests must cover recursive goals, equivalent canonical queries, negative/coherence overlap cases, distinct named models, and deterministic diagnostics. Explicitly distinguish coinductive cycles from unsupported/indeterminate goals.
No parser/checker/CLI integration or macro expansion in this milestone; specify those adapters. Fail closed on unsupported reasoning rather than returning a false success.
```

