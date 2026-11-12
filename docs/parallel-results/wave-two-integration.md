# Second parallel wave integration

All seven completed lanes are integrated on main: checked arithmetic (PR #17),
native lifecycle (PR #20), cached WASI builds (PR #16), Pact compiler bridge
(PR #15), Strata collections (branch `claude/project-thread-5ily4n`, `efc5563`),
source simulation (PR #11) and Forge (PR #14).

Registered the three new build/bridge/simulation crates in both workspace lists,
removed standalone workspaces/locks, and kept the root default binary. Focused
integration ran **240 tests/doctests across 29 suites** with zero failures.
The final integrated workspace, including full Lean AST migration and TUF
updater follow-up, passed **1052 tests/doctests across 114 suites**, zero failures
or ignores, with actual pinned Lean 4.24. Formatting and strict workspace Clippy
passed. Root CLI/Lean syntax/Prism targets cross-compiled for Windows GNU using
pure BLAKE3; Windows runtime and TUF native-crypto cross validation are separate.

Checked arithmetic uses `vesper.arith.checked/1`. Interface semantics version 2
rejects old wrapping exports as Unknown, including old-to-old comparisons.
CI now checks `lean/CheckedArithmetic.lean` and runs the SIMD AArch64 harness.
All eight source examples executed under QEMU with their expected results.

Lean emission now constructs a full syntax AST: no raw-source fallback,
intermediate serialized terms or parsing of generated fragments. Both lowerers
return nodes; only `lean::extract` renders the complete module. Shared helpers
preserve literal control characters and exact floating-point bits. Real Lean
tests caught and fixed do-action context affecting mutable state. The CPS model
contract is checked semantically before importing the reference model's
instances, so formatting changes cannot invalidate or mask that check.

Builds of source-divergent worktrees now use distinct target directories:
sharing compiler artifacts produced misleading cross-worktree results. Final
integration validation was rerun from the coordinator's own sources. Generated
root caches were cleaned to reclaim space; source branches and logs were kept.

The TUF worker preserves synced state before subsequent fetches, latches failed
commits, retains authenticated failure progress and rejects persisted-state
regression. It survives handle drop while retaining its store lock; process
death/panic and full TUF recovery remain outside that guarantee. Independent
review found and fixed a stale-final-checkpoint rollback. Store drop explicitly
unlocks even if a process spawn briefly inherits its file description; a direct
duplicate-description regression covers that ownership boundary.

Kernel io_uring is unavailable on this managed host; explicitly skipped kernel
paths still count as passed Cargo tests. This is not kernel execution evidence,
nor a compiler-correctness, total linear exhaustion or performance proof.
