# Execute source effects through SimPulse

Suggested branch: next/source-simulation.

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

Assignment: connect checked Vesper source execution to the deterministic SimPulse kernel, producing a runnable source-level simulation and replay milestone.

Exclusive ownership:
- NEW crates/vesper-sim-session/** (library, runnable example or small standalone binary, tests and fixtures)
- docs/parallel-results/source-simulation.md
Read-only dependencies: root vesper, vesper-simpulse and the existing Pulse public APIs. Do not edit src/runtime.rs, src/io_session.rs, scheduler/driver, simulator internals, root CLI/manifests or any compiler files. Use a temporary standalone workspace for the new crate.

Read Runtime::begin_external, Execution, Suspension, the existing io_session embedding and the SimPulse report/API. A Suspension uniquely owns the reference evaluator continuation. The host bridge only installs concrete linear effects. SimPulse owns opaque task payloads and rejects replay mismatches before moving ownership. It does not itself execute source, emulate the OS or provide DPOR.

Implement:
1. A source-session adapter that parses/checks real source before execution and runs its external linear operations through Simulator-owned task tokens. Preserve the lifetime of World and every live Suspension without self-referential unsafe shortcuts.
2. A versioned, explicitly bounded scenario format mapping checked effect/operation signatures to yield, virtual sleep/clock and scripted I/O completion/error/cancellation. Validate signatures and argument/result types; unknown operations/signatures fail explicitly. No ambient sockets/files or hidden real-clock/random calls.
3. Preserve inout operation aliases via Suspension::write_argument where supported. Complete a linear operation by resuming with the checked response; cancellation cannot simply drop the source continuation or invent an abort where the signature is linear.
4. Bind replay to the actual embedding executable/compiler build fingerprint as well as source/scenario, operation bindings, runtime fuel/options and semantics. An unchanged package version such as 0.1.0 or an unchecked caller-supplied version label is insufficient; use a real executable digest or a verified build manifest. Reject changed programs, scripts, bindings or execution identity. Keep metadata separate from the simulator's existing journal schema unless a future coordinator change is required.
5. Expose record and replay APIs plus a runnable command/example. A replay mismatch, external step-budget exit or adapter/simulator error detected before a consuming runtime transition must retain pending owners in a recoverable session/run result. Validate host response shape and policy before resuming. The baseline Runtime::begin_external(self, ...) and Suspension::resume(self, ...) consume the machine and return only a string on interpreter faults/fuel exhaustion: report those as explicit terminal execution failures, not recoverable runs or successful cleanup. Do not invent an owner after the baseline API has consumed it. Document this limit and propose an owned runtime-error API separately for a later coordinator change.
6. Demonstrate two source tasks with virtual delays/interleaved scripted I/O and a source linear ticket/resource live across suspension, consumed once after resumption.

Acceptance:
- Same source/scenario/seed produces the same source outputs, virtual times and trace; replay reproduces it.
- Modified source/scenario, reordered/missing/extra completions and altered response/error data cause explicit mismatch.
- Cancellation acknowledged before actual scripted I/O completion retains the original request/continuation until that completion.
- Nested/non-tail handlers, linear state and supported inout operation arguments remain correct against direct reference evaluation where comparison applies.
- Duplicate completions/resumes, malformed signatures and unknown operations are rejected; pre-transition adapter errors and external step-budget exits return a usable owner; interpreter fault/fuel-exhaustion tests expose the documented terminal baseline limitation.
- No real-time sleeps/network needed in ordinary tests. Formatting, strict Clippy and real source integration tests.

Use fixtures that avoid depending on implicit wrapping arithmetic; the concurrent compiler lane is changing that behavior. Be explicit that this executes the reference evaluator under deterministic simulated effects. It is not native execution, kernel I/O, exhaustive scheduling/DPOR or a production Storage/Net implementation.
~~~

