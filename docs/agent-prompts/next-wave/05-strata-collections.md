# Bounded region-backed collections

Suggested branch: next/strata-collections.

~~~text
You are implementing one bounded milestone in https://github.com/robot-head/vesper.

Start from a fresh fetch of origin/main in a NEW cloud checkout or dedicated git worktree. Minimum baseline: 0e6caf9 (all first-round handoffs integrated). Record the actual base SHA. Do not share a checkout or branch with another agent. Read docs/specification.txt, docs/implementation.md, docs/parallel-work.md, docs/parallel-results/handoff-integration.md, and docs/rfcs/parallel-contracts.md before editing.

Delivery rules:
- Implement working behavior, with meaningful positive and negative tests. Deliver code, not only an RFC, scaffolding or an adapter proposal.
- Edit only your owned paths. Root Cargo.toml/Cargo.lock, src/main.rs, src/lib.rs, CI and global documentation belong to the coordinator unless explicitly assigned below. Read other lanes' code, but do not change it. Report any necessary cross-boundary patch separately.
- Existing workspace members stay registered: do not restore nested workspaces or lockfiles. A NEW crate may temporarily use its own [workspace] and Cargo.lock for independent testing; the coordinator will register it and regenerate the root lockfile. Match current validated dependency versions. Do not edit the existing sandbox guest workspace/lockfile.
- Do not downgrade dependencies to match illustrative design snippets. Keep public module paths and existing supported behavior, except the explicitly assigned arithmetic migration.
- Keep ownership and failures explicit. Unsupported, ambiguous, overflow, invalid evidence and exhausted budgets must never become success. Preserve recoverable owners on error where the API takes ownership.
- A byte hash is not provenance; a residual effect row is not an authority audit; executable Lean extraction is not a preservation/termination proof; seeded replay is not DPOR. Do not claim kernel/hardware validation from mocks or object emission.
- Preserve the user-approved field restriction k_i ⊑ m_i. Do not modify the Google Doc. Record decisions and remaining gaps in your assigned report.
- Run formatting, relevant tests and strict all-target Clippy. Use real Lean when extraction changes; report unavailable toolchains, kernel permissions or platforms explicitly. For source fixtures outside the arithmetic lane, avoid depending on implicit wrapping arithmetic: that lane is migrating +, - and * to checked effects.
- Commit and push YOUR branch only. Do not merge into main, publish packages or message third parties. Return the base SHA, commit SHA/PR, public API, exact checks/results, integration steps and remaining limitations. If blocked at an ownership boundary, finish independent work and provide a precise coordinator patch/request.
- Other agents are running concurrently. Do not broaden your scope or start overlapping subagents.

Assignment: extend the existing Strata region core with safe, bounded contiguous collections, while preserving the region alias/rewind contract.

Exclusive ownership:
- crates/vesper-alloc/**
- docs/parallel-results/strata-collections.md
Do not edit compiler types, syntax/checker, native/Lean backends, root manifests or the source-simulation lane. The allocator is already a root workspace member; do not restore its old nested workspace/lockfile.

Read the allocator implementation/report and design §15. Existing Region uses stable chunks, checked (region, slot, serial) handles, bounded reservation, LIFO watermarks and runtime stale-handle rejection. RegionBox is invariant in T. Rust callers can currently drop/forget handles or return them out of Region::scoped; the library does not establish source-language linear exhaustion.

Implement a concrete first collection slice:
1. Region-backed contiguous vector/buffer storage and a UTF-8 string wrapper. Expose an idiomatic checked API for construction, capacity/length, indexing/views, push/pop, reserve/grow and explicit extraction/release. Explain supported T and whether handles are unique or copyable.
2. Every element buffer must come from Region backing and remain within configured limits. No fallback global Vec/String for element storage. Existing bookkeeping/backing allocations may remain host allocations; do not claim a zero-heap native implementation.
3. All length/capacity/alignment arithmetic is checked. Failed growth preserves the previous collection and returns any uninserted owned value. Moving elements during growth updates initialization/finalizer metadata before old storage is reclaimed, with no double-drop or reading uninitialized memory.
4. Shared views are immutable; mutation requires exclusive region access or another demonstrably equivalent unique capability. Copied handles must not allow overlapping mutable references or bypass region identity/generation checks. Keep invariance for stored borrowed types.
5. Rewind/reset/release invalidate affected handles and views consistently. Preserve pre-watermark allocations and invalidate post-watermark allocations even after slot/address reuse. Define finalizer order, partially initialized growth failure, clear/truncate/pop behavior and region destruction.
6. Keep UTF-8 valid through all safe string APIs. Byte views must not permit unchecked mutation into invalid UTF-8.

Acceptance:
- Zero-sized and over-aligned elements, empty/max-sized capacities, integer overflow and allocation-limit failures.
- Drop-counted/non-Copy payloads across push/pop, growth, failed insertion, truncate, rewind, reset and region drop; each initialized value drops or is returned exactly once.
- Copied/foreign/stale handles, slot reuse, copied aliases and compile-fail borrow/lifetime/invariance examples.
- Growth failure leaves observable length/elements unchanged and returns the input owner.
- Invalid UTF-8 inputs/boundaries are rejected; multibyte string operations preserve validity.
- Existing allocator suite plus focused collections tests, strict Clippy and formatting. Run Miri if available; report it as unavailable rather than implying a run.

Your report must specify a RegionAccess contract usable by the future compiler: allocation origin, exclusive mutation, handle generation/watermark invalidation and linear payload obligations. Do not implement source syntax, infer source lifetimes from Rust borrowing, or adopt abort-on-allocation-failure semantics that could discard a source linear resource.
~~~

