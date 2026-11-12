# Pact bridge from checked source types

Suggested branch: next/pact-compiler-bridge.

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

Assignment: build the missing checked-source-to-Pact adapter and an evidence-consumption gate, without competing with the arithmetic lane for parser/checker files.

Exclusive ownership:
- NEW crates/vesper-pact-bridge/** (library, tests and a runnable example)
- docs/parallel-results/pact-compiler-bridge.md
Read-only dependencies: the root vesper compiler and vesper-pact. Do not edit AST/syntax, src/check.rs, src/interface.rs, the solver, root CLI/manifests or other lanes. Use a temporary standalone workspace for the new crate.

Read docs/parallel-results/pact-goals.md, especially its adapter specification, and the current Pact README. Program::new validates DTO shape but DOES NOT establish coherence. A derivation proves an applicable declaration; it does not by itself prove global uniqueness. ModeScheme is only base joined with selected argument modes, whereas World::mode includes field modalities, generic bounds and recursive instantiations.

Implement:
1. A bridge session created from successfully parsed/checked source and explicit package identity/definition metadata. It must reject source diagnostics. For trait/impl/model declarations not yet expressible in source, accept a validated versioned declaration-binding DTO that refers to resolved source types; document this boundary clearly.
2. Deterministic package-qualified external definition identities and dense Pact ID tables, with reverse maps for diagnostics. Whitespace/reordering cannot change external identity. Numeric table indices may be revision-local; never use spans, process addresses, local variable names or $cap span labels as stable public IDs.
3. Translate supported primitives, tuples, nominal ADTs, generic mode/row parameters and lexical capability placeholders. Derive constructor mode schemes from actual checked declarations, including phantom parameters and modality bounds. If Pact's scheme cannot faithfully represent a case, return Unsupported with a precise reason; do not approximate it into a false proof or change World::mode.
4. Build and validate ProgramDto, run check_coherence, and make the successful result an opaque session required for queries/evidence consumption. Incoherence must block the gate even if Solver would return Proven. Rebuild the gate and tables when declarations change.
5. Translate query substitutions and named-model brands consistently. Preserve all five solver outcomes. Re-check returned derivations before exposing accepted evidence; unsupported/overflow/ambiguity are not acceptance. Keep branded type identity distinct even where runtime layouts coincide.
6. Provide source-facing names/spans in diagnostics without leaking them into canonical goals. A runnable example must parse/check real source ADTs, bind actual trait/model DTO declarations, solve a goal, and consume validated evidence.

Acceptance:
- Source type/mode behavior agrees with World::mode for supported well-formed instantiations; cover every allowed copy/affine/linear argument combination in small fixtures.
- Recursive generic ADTs, phantom parameters, row/capability alpha-renaming and reorder/whitespace stability.
- Two differently qualified same-named definitions never alias; invalid source/types/IDs/arity/kinds are rejected.
- Coherence overlap/orphan failures block acceptance, including overlapping impls with identical substitutions.
- Distinct named models and brands remain distinct; ambiguous, unsupported and bounded-overflow queries never succeed.
- Declaration changes invalidate sessions/evidence; malformed certificates return diagnostics rather than panic.
- Formatting, strict Clippy and real compiler/Pact integration tests.

Do not invent a second Vesper parser or claim trait/model syntax, dictionary codegen or full source trait checking is implemented. Deliver the real reusable adapter first; the next compiler lane will connect surface declarations and calls after arithmetic merges.
~~~

