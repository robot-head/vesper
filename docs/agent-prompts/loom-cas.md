# Loom content-addressed store

Suggested branch: `external/loom-cas`.

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
Own crates/vesper-loom-cas/**, docs/parallel-results/loom-cas.md.
Design §8. Implement an immutable BLAKE3-addressed store with canonical derivation keys, verified ingestion/readback, atomic publish, corruption rejection, concurrent writers and safe workspace materialization.
Use explicit source/interface/artifact domains and a versioned input schema. Reject path traversal and unsafe symlink/hardlink materialization. Reflink is an optional optimization with a correct copy fallback.
Test interrupted writes, duplicate/concurrent writes, corrupted contents, deterministic hashes, and adversarial paths in temporary directories.
Do not claim Sigstore/TUF/builder-quorum trust just because a digest matches; those belong to a separate verifier. No registry/network service or host build execution.
```

