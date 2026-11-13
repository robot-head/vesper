# Arithmetic oracle runner #275: authored checkpoint

The implementation follows frozen contract `cae1bc5b50ebe7cd448fd9a18ff87f9f4c1ec6c2`.
Owned source checkpoints are `95dc143` (shared DTOs), `c7be985` (bounded framing
and Lean data protocol), and `78577f3` (subprocess supervision). `f4e99b7` adds
nine runner integration tests. Source suballocation `worker.rs` belongs to
params_plan, original owned commit `84d4535`; its local dependency copy is
`196165c`. The Lean and corpus commits present in this worktree are dependencies,
not runner-owned changes.

The runner starts an actual pinned Lean 4.24.0 server from private regular-file
copies of the bundled model/driver. It preserves one Lean session and explicitly
checks READY, response IDs, diagnostic pairs, terminal counters, EOF and exit.
Rust worker IPC carries typed cases and no expected Lean answers. All i128
payloads use exact decimal strings. PATH-aware executable resolution also handles
Bazel's bare executable names. The actual resolved worker binary is hashed in
bounded chunks.

Each worker response confirms actual parser/checker/AST-optimizer and all eight
evaluator/JIT outcomes, or the actual isolated diagnostic. Cases are dispatched
in bounded batches of at most jobs (maximum four), committed in case order, and
workers retire at the configured handled-frame cap including static negatives.
The default cap is 32; hard cap is 128. Compiler/JIT allocations are contained
by process recycling, with no claim of a reusable compiled handle or whole-RSS
bound. A killed worker's unknown partial activity is identified separately from
confirmed completed call counts.

Production framing handles short reads, length checks before payload allocation,
bounded writer queues, concurrent stderr draining, parent deadlines, kill/reap
and thread joins. The test-only transport probe invokes these same mechanisms.
Independent negative peers cannot provide successful arithmetic oracle answers
or a RunReport. Artifact contents are bounded and individually hashed; report
and failure summaries are excluded from their own hash inventory. Optional
shrinking retains the original failure and separate call counts, using the same
healthy oracle and increasing wire sequence.

At this checkpoint the source author executed only Rust formatting and
`git diff --check`; both passed. Cargo and real Lean execution are reserved to
the coordinator. No authored test, real CI128 matrix, full 100000-program run,
strict Clippy result, or process-cleanup assertion is represented as executed
here. Actual coordinator results and any resulting fixes will be recorded in a
follow-up after execution.
