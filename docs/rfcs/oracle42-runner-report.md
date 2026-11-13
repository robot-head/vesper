# Arithmetic oracle runner #275: coordinator validation

The implementation follows frozen contract `cae1bc5b50ebe7cd448fd9a18ff87f9f4c1ec6c2`
and the subsequently published carrier and launcher/evaluator amendments.
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

Independent review findings are resolved. The configured source byte cap is
checked before any case request or compiler-worker dispatch. Confirmed backend
calls are counted before fallible worker retirement; malformed or interrupted
replies identify unknown partial activity. Optional shrinking first reaps idle
normal workers, logs started/progress/failed attempts, and preserves separate
call counts. Complete artifact content, inventory and summary quotas are
preflighted before initializing the output directory. Each child's complete
shutdown uses one absolute deadline. Timely queued child completions remain
timely while the parent services another slot; after the lowest failure is
known, higher pending work is killed and reaped. Transcript hashes include
observed complete Lean frames and confirmed written request frames, including
rejected complete handshake/terminal frames, without inventing partial bytes.

Published carrier changes box the two outcome arrays and the partial report
without changing JSON data. Tool discovery preserves multicall launcher names
for configured Lean and optional rustc probes; the actual worker/hash identity
and pinned prefix compiler remain canonical. Rustup/elan version output cannot
be reported as rustc metadata.

The coordinator actually executed the following checks; the source author did
not run Cargo or Lean under the coordinator's exclusive build lease:

- On integration `3e0653e`, the runner target passed all 20 tests in 4.73s,
  with zero failures. This includes shared private process/worker regressions
  as well as the runner integration tests. The corpus target passed 20 tests
  in 0.84s. Log: `/tmp/oracle42-root-corpus-runner-final.log`.
- On integration `f1b4cde`, independent acceptance passed 11 tests in 50.86s.
  The actual CI128 suites ran with jobs1 and jobs2 against real Lean 4.24.0,
  checking all eight compiler/evaluator/JIT settings, all 15 isolated negatives,
  exact counts, source binding/caps, process fault cleanup, and deterministic
  Lean request/response transcript hashes across worker schedules.
- On integration `c5150f5`, strict package all-target Clippy passed in 19.33s:
  `cargo clippy --locked -p vesper --all-targets -- -D warnings`.

The target counts include shared tests and must not be added as distinct
independent coverage totals. Source formatting and `git diff --check` also
passed in the author worktree. The initial request-size fixture failure was
corrected in `72d8109`: exact-size success and one-byte-smaller refusal use the
same request ID, preserving the actual size boundary.

These results validate the bounded CI128 profile and its actual process paths.
The release build and full 100000-program execution were still pending at this
report update. No full-run count, throughput extrapolation or compiler
refinement proof is claimed.
