# Loom sandbox builds backed by CAS

Suggested branch: next/loom-cached-builds.

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

Assignment: connect the existing CAS and real WASI component host into a usable offline build-step runner.

Exclusive ownership:
- NEW crates/vesper-loom-build/** (library plus a small standalone executable or runnable example)
- docs/parallel-results/loom-cached-builds.md
Existing vesper-loom-cas, vesper-loom-sandbox and vesper-loom-calculus APIs are read-only dependencies. Do not edit root CLI/manifests or the Forge lane. Use a temporary standalone workspace for the new crate.

Read the CAS/sandbox reports and public APIs. Store distinguishes source/interface/artifact domains and derivation inputs. SandboxHost executes committed wasm32-wasip2 components with fresh stores, deterministic configuration, read-only declared inputs and a writable initially empty /out. Its output-size check is post-run acceptance, not an enforced disk quota.

Implement:
1. A versioned BuildRequest and BuildOutcome plus an actual local run command/example. Inputs include component bytes, build invocation, immutable input trees/mount mapping, target/options, sandbox determinism and resource limits. Validate unknown versions, duplicate/overlapping mounts, incompatible policy and malformed input before executing.
2. Snapshot every declared input into CAS, then execute against independently materialized snapshots. Hashing one host directory and later mounting that mutable original is not a valid cache key. Define behavior when inputs change during ingestion; never label unsnapshotted data as immutable.
3. Canonical derivation keys must bind component implementation bytes, source/input tree digests, exact invocation, sandbox/WIT schema, builder/engine implementation identity/configuration, target/host compatibility where relevant, limits, clocks and randomness. An interface MIF alone or an unchanged package-version string cannot key code execution. Refuse reusable results when a required execution fingerprint is unknown.
4. On a hit, verify referenced objects/metadata and materialize outputs independently of the store. On a miss, run the REAL sandbox host in a fresh dedicated output directory, validate the complete result and output tree, then publish immutable artifacts and a complete result manifest atomically.
5. Only a successful accepted build may publish a derivation. Guest errors, traps, fuel/memory/timeout/output failures and corrupted hits must not publish success or clobber a valid destination. Define cleanup, stale staging and concurrent same-key winners; detect divergent outputs for one derivation rather than overwriting them.
6. Restrict this milestone to StepKind::BuildScript. Reject ProcMacro/DeriveMacro until trusted provenance and authority checking is connected. Do not certify package confinement from the manifest's residual effect rows or the sandbox's lack of network.

Acceptance using the existing REAL fixture component:
- Miss then hit with identical accepted outputs; workspace edits cannot mutate stored artifacts.
- Each observable input/configuration change invalidates the key; a same-interface body/component change produces a new key.
- Mutating the original host input after snapshot does not affect execution; inputs are read-only and ambient access remains denied.
- Failure/timeout/budget paths publish no derivation and preserve an existing output destination. Concurrent writers and corrupted/dangling cache references are handled explicitly.
- Different target/engine/builder identities cannot reuse incompatible results.
- Formatting, strict Clippy, library tests and a documented end-to-end runner invocation. Commit fixture bytes/checksums if you add fixtures; ordinary tests must not need network downloads or WASI-target installation.

Document key encoding, compatibility domain, trust assumptions and output-quota limits. This is an offline build integration, not the registry, authenticated lockfile, Forge expander or full Loom build graph.
~~~

