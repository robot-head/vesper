# Lens remainders: poisoned recovery, obligation completion, proof lenses, latency (§21)

Wave-six lane 8, claim issue #250. Base `07322e3` (main after #268, a
descendant of the required `cda092f`). Branch `claude/project-thread-u0gb4k`.

## What was built

All four items run in the real stdio server (`vesper lsp`).

### 1. PoisonedRecovery (§21.2)

The design's Stage-3 `LocalCapability::PoisonedRecovery` is modelled in the
editor, not in the checker. Vesper's checker never sees unfinished code: any
`tx.` or `let r = commit(` is a parse error, and the existing recovery path
(`tooling::recovery`) marks the damaged function `damaged_function` and checks
only complete functions. That keeps leak errors out of broken buffers. It also
leaves the damaged function with no types, so nothing can complete `tx.`.

`src/lsp/lens.rs` adds a repaired-prefix view for each damaged top-level
function:

- From the `vesper-syntax` recovery snapshot (read-only use), it finds the
  first syntax damage inside the function and cuts at the end of the last
  complete top-level statement before it. A `{ .. }` block statement counts
  as complete only when no `else` or continuation follows. Bytes from the cut
  to the end of the declaration become spaces, and a `}` goes at the cut.
  Newlines and every byte offset stay the same.
- The repaired text goes through the normal editor guards
  (`frontend::check_source`), then the normal parser and checker. Their facts
  are used only for presentation. The repaired module never becomes a
  `CheckedSource` and never feeds `guarded_analysis`, navigation authority,
  code actions or certificates.
- A syntax-free walk over the checked AST collects bindings, their types and
  their consumption events: sink arguments resolved against callee
  signatures, `sink self` method receivers, moves into `let`, struct fields,
  tuples, tails, `match` and `?`. Events are path-aware, so a consumption in
  one branch of an `if` or `match` does not count at a cursor in a sibling
  branch.
- Owned linear bindings still live at the cut are the poisoned set. Each
  poisoned binding gets exactly one diagnostic: code
  `VESPER_POISONED_RECOVERY`, severity Information, range at the binding's
  origin, and `relatedInformation` pointing at the unfinished code. The scope
  exit leaks (E0302) and abort-frame errors (E0402) that the truncated prefix
  provokes are absorbed, never published. They are counted in
  `vesper.recovery.lens.absorbed_scope_exit_diagnostics`, and the poisoned
  set and cut are in `vesper.recovery.lens`.
- Bindings consumed before the damage are not poisoned. Finishing the code
  clears every poison, and the checker is authoritative again. A real leak in
  finished code is still E0302.

A repaired-prefix view is unavailable, with a typed reason in
`vesper.recovery.lens.reason`, when the damage is not inside a top-level
function body. That covers a damaged impl, struct or effect, a damaged header,
opaque declarations, top-level debris, a repair that does not parse, a guard
rejection, or a resource limit. Diagnostics then stay exactly as before.

### 2. Obligation-aware completion, hover and inlay hints (§21.1.5)

- `textDocument/completion` (trigger `.`) recognises `receiver.partial` at the
  cursor. It resolves the receiver lexically at the receiver's start, so the
  use being typed does not count as a consumption, and lists the methods of
  its type (`Type::method` with a `self` parameter). Ranking comes from
  `vesper_lsp::lens::rank_methods`:
  - `discharges_live_linear` (sort first, badge "discharges linear
    obligation") for `sink self` methods on a live linear receiver;
  - `valid_frame_compatible` for the rest;
  - `blocked_by_frame` (deprecated tag) for methods with an abortable
    (non-linear) effect while a linear value would stay live across them.
    This uses the same notion of abortable as the checker's E0402.
  This works in damaged buffers (from the repaired prefix), in checked
  buffers, and in buffers with semantic errors (the checker's partial facts,
  scope `semantic_error_partial`). Projected receivers (`a.b.`) and unknown
  receivers return an empty list, never a guess.
- `textDocument/hover` adds an "outstanding linear obligations here" section
  to navigation's hover. It lists every owned linear binding that is visible
  and not yet consumed at the cursor, with type, origin line and discharging
  calls. In a damaged buffer it answers from the repaired prefix.
  Navigation's own answer and error precedence are kept when nothing is
  outstanding.
- `textDocument/inlayHint` labels each linear binding `linear: consumed on
  line N`, `linear: outstanding`, or `linear: poisoned by unfinished code`.

### 3. Proof code lenses (§21.1.5, §21.3 Layer 5)

`textDocument/codeLens` puts a lens on every function of a checked buffer
whose definition appears in `lean::extract`. Its title shows `not attempted`,
`proved` or `failed at <stage>`, and its command is `vesper.proofAudit`
(`workspace/executeCommand`). Running it:

1. extracts the checked module with the existing `lean::extract`, then appends
   `theorem VesperLens.obligation : @«Vesper».«f» = @«Vesper».«f» := rfl`;
2. compiles it with Lean 4.24.0 (version checked) in a private staging
   directory;
3. runs the existing `vesper proof-audit` kernel replay with the empty axiom
   policy on that theorem. The theorem's type mentions `f`, so the audit's
   transitive axiom closure covers `f`'s kernel-visible definition.

What "proved" means: Lean elaborated and kernel-accepted the extracted
definition, and its kernel-visible closure uses no axioms (no `sorryAx`, no
classical choice). It is not a functional-correctness theorem. It is not a
theorem about the Rust compiler or the extraction's faithfulness. For
`partial def` extractions, which is how the emitter writes source functions
today, the kernel sees only the opaque constant, not the body. The lens scope
is labelled `lean_extraction_axiom_free` everywhere.

Outcomes live in a bounded ledger keyed by exact source hash and function, so
an edit returns every lens to `not attempted`. Damaged or ill-typed buffers
get no proof lenses. The Lean launcher comes from server configuration
(`VESPER_LEAN`, else `lean`). The auditor is the running `vesper` executable.
Clients cannot name executables: the command takes only `uri`, an optional
`version` (stale gives -32801) and `function`.

### Cancellation and the stdio loop

`serve` now runs a reader thread that frames input into a channel. The
session stays on the calling thread, so requests are still handled in arrival
order. Proof jobs run on worker threads and answer when they finish, and the
server keeps serving other requests in the meantime.

`$/cancelRequest` for a pending job answers that ID with -32800 exactly once,
flags the job, and kills its Lean or auditor child process. The killed job's
late completion, a repeated cancel, and cancels for unknown or answered IDs
send nothing. `exit` cancels every pending job. The bookkeeping is
`vesper_lsp::lens::PendingRequests`/`CancelToken`.

A completion is matched to its job token, not just its ID, so a cancelled
job cannot answer a later request that reuses the ID. Each child runs in its
own process group on Unix (`taskkill /T` on Windows), so cancellation and
timeouts also stop the Lean process that `vesper proof-audit` starts. The
reader queue holds at most 8 frames, which keeps the old backpressure. Cached
Lens views are dropped whenever the session database is rebuilt, including on
every `didClose`.

### Root-free crate additions (`crates/vesper-lsp`)

`lens.rs` holds `rank_methods`/`CompletionTier`, `ProofLedger`/`ProofStatus`,
`PendingRequests`/`CancelToken` and `cancel_params`. None of it computes
compiler facts or runs a process. The `examples/keystroke_latency.rs`
benchmark client is there too. README section added.

## Tests

Stdio session tests (`src/lsp/lens_tests.rs`, 8) drive `serve_with` over
in-memory pipes with the server on its own thread, using real Content-Length
frames:

- `poisoned_region_reports_each_binding_once_without_cascading`: two live
  linears and a damaged `tx.` give one notice each, no E0302/E0402, origins
  and related ranges checked, and at least 2 absorbed scope-exit diagnostics.
  Adding more unfinished code (`other.`, `let`) still gives exactly two. A
  binding consumed before the damage is not poisoned. Finished code gives
  `checked` with no diagnostics. A real leak gives E0302 and no poison.
- `completion_lists_consuming_calls_first_for_a_live_linear_receiver`: the
  order is abort, commit (discharging), slot (valid), persist (blocked:
  abortable `Storage` with another live linear). `sortText` matches. After
  `tx.commit()` the receiver is not live. No receiver and an unknown receiver
  both give an empty list.
- `hover_and_inlay_hints_show_outstanding_obligations`: checked buffer with
  per-position sets, inlay "consumed on line N", and the repaired-prefix hover
  in a damaged buffer.
- `proof_lens_cancellation_answers_once_and_keeps_not_attempted` (unix): a
  stand-in Lean whose compile sleeps. The server answers code lenses while
  the job runs. Cancel gives one -32800, and a second cancel, the killed
  job's completion and an unknown-ID cancel give no message. The lens stays
  `not_attempted`.
- `proof_lens_records_proved_failed_and_stale_requests` (unix): stand-in
  toolchain and auditor. `proved` is recorded and shown, unknown functions
  fail at `extract`, a stale version gives -32801, an unknown command gives
  -32602, an edited source resets to `not_attempted`, a damaged buffer has no
  lenses, and a rejecting audit gives `failed at kernel`.
- `closure_sink_parameters_and_function_value_calls_follow_checker_modes`:
  a closure's sink parameter is an owned obligation in its body, and passing
  a linear value to a function value's sink parameter consumes it.
- `proof_lens_cancellation_stops_the_whole_process_tree` (unix): the
  stand-in compile forks a grandchild, and after cancel the grandchild is
  gone.
- `proof_lens_with_real_lean_and_auditor`: real Lean 4.24.0 and the real
  `vesper proof-audit`, run when `VESPER_LEAN` and `VESPER_LENS_AUDITOR` are
  set and skipped otherwise. Locally: `proved`, 7.1 s.

Crate tests (`crates/vesper-lsp/tests/lens.rs`, 5): ranking tiers and badges,
abortable blocking (receiver and other bindings), ledger identity and
eviction, answer-once pending bookkeeping, and typed cancel params.

I also checked by hand with a Python stdio client against
`target/debug/vesper lsp`. Real Lean proved `main` of
`examples/transaction.ves` ("kernel replayed 73 constants with an empty axiom
policy", 6.9 s), and a cancel during a second audit left the server answering
hovers immediately.

## Latency (§21.3)

Setup: 4-vCPU Intel Xeon @ 2.30 GHz cloud VM, Linux 6.18, rustc 1.97.0,
`--release` builds, nothing else running. I used
`cargo run --release -p vesper-lsp --example keystroke_latency --
target/release/vesper <fixtures> --iterations 200`. Each sample is the wall
time from writing one `didChange` frame (an incremental single-range change)
to reading the `publishDiagnostics` with that version, including pipes and
JSON framing. The edits:
- *valid*: insert or delete one space in the last function body;
- *damaging*: insert or delete `let ` before the last body line, which runs
  recovery, the repaired prefix and poison.

Each kind has n=400 samples. Values are medians and p95 in ms. Main is `07322e3`.

| Fixture | Bytes | Open status | Valid, main | Valid, lane | Damaging, main | Damaging, lane |
| --- | ---: | --- | ---: | ---: | ---: | ---: |
| examples/native-pulse/ticket_tally.ves | 1891 | checked | 0.809 / 1.102 | 0.889 / 1.236 | 0.872 / 1.088 | 0.947 / 1.190 |
| crates/vesper-sim-session/fixtures/pipeline.ves | 1461 | checked | 0.638 / 1.013 | 0.713 / 0.948 | 0.676 / 0.919 | 0.723 / 0.957 |
| examples/native-pulse/lease_cleanup.ves | 1083 | checked | 0.522 / 0.804 | 0.574 / 0.664 | 0.561 / 0.805 | 0.622 / 0.714 |
| examples/transaction.ves | 383 | checked | 0.178 / 0.227 | 0.236 / 0.423 | 0.194 / 0.255 | 0.261 / 0.458 |
| crates/vesper-sim-session/fixtures/warp_ack.ves | 10336 | resource guard | 0.376 / 0.439 | 0.417 / 0.576 | 0.784 / 0.861 | 0.839 / 1.115 |
| crates/vesper-sim-session/fixtures/warp.ves | 9546 | unsupported guard | 1.018 / 1.105 | 1.062 / 1.551 | 2.451 / 2.674 | 2.469 / 3.500 |
| crates/vesper-sim-session/fixtures/kv.ves | 4537 | unsupported guard | 0.502 / 0.555 | 0.549 / 0.815 | 1.232 / 1.334 | 1.264 / 1.905 |

Findings:
- On every editor-checkable fixture, both edit kinds, including the repaired
  prefix and poison, have medians under 1 ms. The worst p95 is 1.24 ms
  (ticket_tally). The sub-millisecond goal is met at the median for these
  fixtures, not at p95. The 2 ms keystroke budget in §21 is met at p95 on all
  four.
- Lens adds about 0.03 to 0.08 ms at the median. That cost comes from the
  reader-thread hop and, on damaged edits, the repaired-prefix check.
- The repository's largest fixtures (`warp_ack`, `warp`, `kv`, 4.5 to 10 KB)
  are not checked by the existing editor profile at all. `warp_ack` exceeds
  the weighted expression/signature budget. `warp` and `kv` use generic
  constructor inference that the editor guard refuses. Their numbers measure
  the guard and recovery path, not checking. The largest editor-checkable
  fixture is 1.9 KB. No claim is made about larger checked sources.
- Single runs on a shared VM; max values (up to 3.8 ms) include scheduler
  noise.

## Checks

Exact commands, with `CARGO_TARGET_DIR` set to a private directory and
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`:

- `cargo fmt --all -- --check`: clean.
- `cargo test --locked -p vesper --lib lsp`: 28 passed, including the 8 Lens
  session tests and one updated existing capability test.
- `cargo test --locked -p vesper-lsp`: transport, documents and 5 new lens
  tests pass.
- `VESPER_LEAN=…/lean-4.24.0-linux/bin/lean VESPER_LENS_AUDITOR=target/debug/vesper cargo test --locked -p vesper --lib lens_tests::proof_lens_with_real`:
  passed (7.14 s).
- `cargo test --locked -p vesper --test lsp_cli --test lsp_actions --test lsp_navigation --test diagnostics_cli --test doc_navigation_cli --test forge_parameters_cli`:
  pass.
- `cargo test --workspace --locked --no-fail-fast`: everything passes except
  `vesper-loom-build` (`tests/build.rs` 14, `tests/cli.rs` 2), which fail
  with `UnknownFingerprint { missing: ["engine_dependencies"] }`. The same
  tests fail the same way on unmodified main `07322e3` in this environment.
  That crate is not touched by this lane.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: clean.
- `python3 tools/bazel/sync_build_files.py`: no BUILD changes. The new test
  sources live under already-globbed paths, and the fake-toolchain tests need
  only `/bin/sh` and `/bin/sleep`.

## Decisions

- **Root patch (one line, own commit).** `src/main.rs` now passes
  `io::BufReader::new(io::stdin())` instead of `io::stdin().lock()`, because
  `serve` needs a `Send + 'static` reader for its reader thread. `StdinLock`
  is not `Send`. Nothing else in the CLI changed.
- **Editor-side poison, checker untouched.** Shared Place-IR and checker
  schemas are frozen in wave six, so PoisonedRecovery is a presentation
  layer over the normal checker run on a repaired prefix. Rust ownership in
  this host code is not evidence that the Vesper source typing rules for
  PoisonedRecovery are implemented. No source typing rule changed.
- **Prefix semantic errors are not published.** Only the poison notices come
  from the repaired prefix. Its other diagnostics, such as a real type error
  in a statement before the damage, are not shown until the function parses.
  This keeps the existing "damaged function is unavailable" diagnostic
  contract.
- **Diagnostic code.** `VESPER_POISONED_RECOVERY`, not the design's E0303,
  because E0303 is already a checker code.
- **Loom.toml capability ceilings** in completion ranking are not modelled.
  The editor has no manifest input, and only frame-based blocking is
  implemented.

## Remaining gaps

- Damage inside impl methods, structs, effects or headers gets no repaired
  view. Nested-statement damage cuts at the enclosing top-level statement.
- Pattern-bound linear values (destructured fields, match arms) are not
  tracked as obligations, because their types are not recorded per pattern.
- Proof lenses audit extraction well-formedness and axiom freedom only.
  There are no per-function semantic theorems, no `bv_decide`
  counterexamples, no `.olean` certificate reuse and no debounced background
  runs. Each run recompiles the whole extracted module (about 7 s locally,
  most of it the `Std` import replay).
- Cancellation stops proof jobs only. All other requests are synchronous and
  finish before the next frame is read, so a cancel for them always arrives
  after the answer and is ignored, as the protocol allows.
- Latency covers only fixtures the editor profile already checks (at most
  1.9 KB). Raising the editor guard budgets so the 10 KB fixtures check is
  outside this lane.
