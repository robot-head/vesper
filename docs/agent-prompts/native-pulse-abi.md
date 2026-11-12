# Native continuation ABI prototype

Suggested branch: `external/native-pulse-abi`.

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
Own crates/vesper-fiber-abi/**, experiments/native-pulse/**, docs/rfcs/native-pulse-abi.md.
Design §§7, 10, 12. Work with the existing vesper-pulse frame/driver/scheduler APIs, preserving one-shot completion ownership.
Deliver a versioned repr(C) evidence/continuation ABI and an actual Cranelift-generated park/resume round trip that can be scheduled, with a live scalar/aggregate value surviving suspension. Specify caller/callee ownership, state layout, error/cancellation resumption and module/code lifetime.
Demonstrate x86_64 execution and AArch64 object emission. Test repeated resume rejection, nested evidence identities, mutable-place restoration, and moving suspended data storage.
Be explicit about physical stack self pointers: saved_sp/saved_ip fields alone do not prove arbitrary native stack relocation. Document the relocation invariant and demonstrate it for the implemented representation.
Do not edit src/native.rs or production Pulse internals in this milestone. Produce a narrow adapter proposal for integrating the prototype into real source lowering. No reference-evaluator fallback. Do not implement an unrelated VM and call it stack switching.
```

