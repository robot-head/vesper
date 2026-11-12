# Read-only locked resolution

Coordinator work while the seven next-wave implementation lanes are active. Source compiler, native fiber, allocator and new integration-crate ownership remain untouched.

`vesper resolve --locked --catalog catalog.json` validates an existing `Loom.lock` without searching or writing. `--lockfile path` selects another snapshot. Fresh resolution and `-o` retain their previous behavior. Locked mode rejects output/search-budget options and never creates a missing snapshot.

The reusable `vesper_loom_solver::validate_locked(&ResolveRequest, &Solution)` API validates current request/catalog inputs, compares complete selected manifests, runs calculus validation, and recomputes the minimal reachable feature/dependency/native closure from trusted request/catalog manifests. Extra roots/selections/features and stale metadata are rejected; valid older versions stay pinned when newer catalog candidates appear. Every validation error records zero search decisions. `LockedMismatch` is separate from `Unsatisfiable`, invalid inputs and unsupported policies.

Validation:

- 11 new library regressions and all 19 existing solver tests passed.
- Six new locked-resolution CLI tests plus 14 existing CLI/package tests passed.
- Workspace formatting and strict all-target Clippy passed.
- CLI tests verify unchanged bytes, permissions and modification time on success/failure, and unchanged inode on Unix. They also cover read-only snapshots, newer catalog versions, stale metadata, feature injection, missing/malformed snapshots and option misuse.

This is consistency checking over experimental local metadata. It does not authenticate manifests, bind package source bytes, audit compiler authority, establish provenance or execute builds. The complete workspace/Lean suite was not repeated for this CLI/solver-only change; compiler code and backend semantics are unchanged.

See [package workflows](../package-workflows.md) for usage and output/error envelopes.
