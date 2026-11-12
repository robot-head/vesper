# Strata region allocator

Suggested branch: `external/strata-regions`.

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
Own crates/vesper-alloc/**, docs/parallel-results/strata-regions.md.
Design §15. Implement a bounded region allocator core with aligned allocations, checked size arithmetic, owned region identity/generation, reset/reclamation, stale and cross-region handle rejection, and a usable byte-buffer/region-box representation.
Keep physical ownership explicit and avoid GC/ARC. Rust drop behavior is not a proof that Vesper's linear handles cannot be forgotten; explain the compiler obligations separately.
Test alignment, overlap, growth, allocation failure, reuse, stale handles, and cleanup on error. Copied RegionRef handles may alias the same cell despite distinct source roots; state the required region/object-origin or exclusive-region evidence. Rewind must invalidate handles allocated after its watermark, not merely wait for whole-region reset. Use Miri where available; document any unavailable validation.
Do not claim ShardSlab performance, @fip inference, cyclic-region typing, or AoSoA vector lowering unless implemented and measured.
No source syntax/checker/codegen edits yet. Provide a small Alloc/Region lowering contract that the compiler owner can review.
```

