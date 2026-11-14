# Lens rest: deeper recovery, pattern obligations, real proof lenses (§21)

Wave-seven lane 10, claim issue #333. Base `283c811` (main after #354, a
descendant of the required `c025b9a`). Branch `claude/project-thread-afiw3k`.
Only `crates/vesper-lsp/**`, `src/lsp.rs`, `src/lsp/**` and this report are
edited. No root patch, no new dependencies, no BUILD changes.

## What was built

### 1. Repaired views beyond top-level function bodies (§21.2)

`src/lsp/lens.rs` replaces the single "cut at the last top-level statement"
repair with a delimiter-stack repair that is checked by the real parser:

- For each damaged declaration it scans the tokens up to the first damage and
  records every open `{`/`(`/`[` with the end of its last complete element
  (`;`, `,`, or a block followed by a statement start) and whether a `let` at
  that level is still unterminated.
- It builds candidate patches, most specific first, and keeps the first whose
  repaired text parses with no diagnostics (every other damaged declaration
  is blanked while one is validated, and the final text is parsed again).
  Bytes are blanked, closers are written in place, newlines and every byte
  offset are kept. At most 64 trial parses run per view.
- **Nested statements**: a cut at each enclosing block, innermost first, with
  the closers the outer blocks need (including the `;` an unterminated
  `let x = match … {` needs). Bindings of the inner block before the damage
  are now part of the view and of the poisoned set. `regions[].depth` says
  which block was cut.
- **Impl methods**: the damaged method is found from its `fn` token; the
  first candidate closes only the method and keeps the later methods of the
  impl, the fallback closes the impl. Regions are named `Type::method`.
- **Headers**: an unfinished parameter list is closed at its last complete
  parameter (or a damaged signature tail is dropped) and the complete body is
  kept. Such a region (`kind: "header"`) poisons nothing; its function's
  scope-exit checks are just not published. Recovery marks these functions
  opaque, so opaque *functions and impls* are now tried; any other opaque
  declaration is still `opaque_declaration`.
- **Structs, enums, effects**: cut at the last complete member and closed
  (`kind: "member"`); the rest of the file keeps its facts.
- The repaired module is still presentation only: it never becomes a
  `CheckedSource` and never feeds navigation, code actions or certificates.

### 2. Pattern-bound linear values (§21.1.4)

The Lens walker now types patterns. A constructor pattern looks up the
constructor's fields in the checked `World` and substitutes the ADT's type
arguments from the scrutinee's type (a field that mentions an unknown type
parameter stays untyped rather than guessed). `match` arms get the
scrutinee's type. So `let Pair { left, right } = pair;` makes `left` and
`right` owned obligations, `Maybe::Some(t) => …` makes `t` one, and hover,
inlay hints, completion and the poisoned set all see them.

### 3. Proof lenses (§21.1.5, §21.3 Layer 5)

Two lenses per function.

**Extraction** (`lean_extraction_axiom_free`, unchanged meaning): the
extracted module compiles under Lean 4.24.0 and
`VesperLens.obligation_<n> : @f = @f` replays through the kernel with an
empty axiom closure.

**Semantic** (`lean_model_arith_fault_free`, new, per function): the
extraction writes every function as `partial def`, so the kernel cannot prove
anything about its body. `src/lsp/proof_model.rs` therefore builds Lens's
own model of each *scalar* function (u8/u32/u64/i64/bool parameters and
results; `let`, statement-level `x = e`/`x += e` on a local, `if`, checked
`+ - *`, `/ %`, unary `-`/`!`, comparisons, short-circuit `&& ||`,
`wrapping_add/sub/mul`, and non-recursive calls to other scalar functions,
inlined; effect rows limited to `Fail[ArithOverflow]` and
`Fail[RuntimeError]`). The theorem is:

> for every input in range, evaluating the function under
> `vesper.arith.checked/1` raises neither `Fail[ArithOverflow]` nor
> `DivisionByZero`.

It is rendered twice from one model term:

- an exact-integer theorem in which every intermediate value is a variable
  with a *total* defining constraint (so the universal statement is
  equivalent to the property), proved by `omega` and replayed by
  `vesper proof-audit` with at most Lean's standard axioms (`propext`,
  `Quot.sound`, `Classical.choice`; never `sorryAx` or `Lean.ofReduceBool`).
  Status `proved`.
- a bit-vector theorem checked by `bv_decide` only when `omega` fails. A
  counterexample is shown in source terms (`a = 9223372036854775807, b = …`,
  status `refuted`). A `bv_decide` success relies on native evaluation the
  kernel auditor cannot replay (I checked: replay fails on the
  `_reflection_def` constants), so it is reported as `checked_native`, never
  as `proved`.

Functions outside the fragment get an `unavailable` lens with the reason
(for example "parameter 'tx' has type Tx outside the scalar model").

These are theorems about Lens's model of the source function. They are not
theorems about the Rust compiler, the Lean extraction or native code, and
the model's faithfulness to the checker's semantics is argued, not proved.

**One run, one audit.** A job (command or background) compiles each needed
module, writes one aggregate module that imports them, and runs a single
`vesper proof-audit` over every theorem (at most 128). The auditor costs
about 7 s per call here, so this matters.

**`.olean` reuse across runs.** `LensConfig.cache` (`VESPER_LENS_CACHE`, else
`$XDG_CACHE_HOME/vesper/lens-proofs` or `~/.cache/vesper/lens-proofs`) is a
private (0700) content-addressed cache. The key covers the Lean version, the
module name and the module text, and the stored source must equal the
requested text byte for byte before its `.olean` is reused. A reused `.olean`
still goes through the kernel replay; the cache skips only elaboration.
Refuted and `checked_native` outcomes are cached as outcomes. Entries are
published by atomic rename.

**Outcome identity.** Extraction outcomes stay keyed by exact source hash.
Semantic outcomes are keyed by the model's blake3 identity (which covers
inlined callees), so editing one function keeps every other function's
semantic status and re-verifies only what changed.

**Debounced background runs.** Off by default. Enabled by
`LensConfig.background` (`VESPER_LENS_BACKGROUND_MS`) or by the client with
`initializationOptions.vesperLens.backgroundProofs: true` (default delay
150 ms, the spec's Layer 5 idle time; `debounceMs` up to 60 s), and only when
an auditor is configured. `vesper_lsp::lens::Debouncer` keeps one armed timer
per document and a generation per edit; an edit cancels a run started for an
older generation (its Lean processes are killed). A run covers every
obligation of the current source that has no outcome yet. When it finishes
the server records the outcomes, sends `vesper/proofStatus` (per-obligation
results, cache counts, elapsed time) and, if the client declared
`workspace.codeLens.refreshSupport`, a `workspace/codeLens/refresh` request.

The command `vesper.proofAudit` takes an optional `obligation`
(`"extraction"`, the default, or `"semantic"`); unknown keys are rejected.
Clients still cannot name executables or directories.

### 4. Asynchronous requests and cancellation for every request

`src/lsp.rs` now runs three kinds of threads: a reader, the dispatcher (which
owns the writer and the pending-request table), and one session thread that
handles requests and notifications strictly in arrival order. Every request
is registered as pending before it is queued. `$/cancelRequest` is handled by
the dispatcher as soon as it is read: the request is answered `-32800` once,
a queued request never runs, a running one has its late result dropped, and
Lens traversals (the walker, repair) poll the request's token and stop with a
typed `request_cancelled`, which is never cached. End of input drains the
queue first, so everything already read is still answered in order.
Backpressure: at most 32 frames are read and not yet handled.

### 5. Lens's own analysis profile and latency

`LensConfig.limits` (`lens_limits()`) raises AST/type/depth/diagnostic and
metadata budgets for Lens's views only; the editor profile that guards
diagnostics, navigation and code actions (`database::guarded_analysis`, not
this lane's path) is unchanged. When the editor profile refuses a buffer for
resources, Lens builds its view under its own profile and reports it in the
diagnostics' `vesper.lens` metadata.

## Tests

Stdio protocol tests (`src/lsp/lens_tests.rs`, real Content-Length frames
through `serve_with`), new in this lane:

- `nested_statement_damage_cuts_at_the_innermost_block`: two-deep damage
  poisons `tx` and the inner `inner` (depth 1), no E0302/E0402, `inner.`
  completes consuming calls first; a consumption inside the nested block
  before the damage is respected.
- `impl_method_damage_gets_a_repaired_view_that_keeps_later_methods`:
  `Ledger::close` damaged, `self` and `other` poisoned, and `settle`, defined
  after the damaged method, is offered first.
- `struct_effect_and_header_damage_keep_the_rest_of_the_file`: damaged
  struct, effect and parameter list each give an available view (`member`,
  `member`, `header`), nothing poisoned, inlay hints for a complete function.
- `pattern_bound_linear_values_are_tracked_as_obligations`: destructured
  fields and a match-arm payload get "consumed on line N" hints, hover lists
  the destructured obligations, and damage inside the arm poisons `t`.
- `cancellation_answers_queued_and_running_requests_at_once`: a running
  request (a `#[cfg(test)]` method that blocks until cancelled) and a hover
  queued behind it are both answered `-32800` within 5 s, neither answers
  again, and the server keeps serving.
- `semantic_lenses_follow_the_model_identity` (unix, stand-in toolchain):
  semantic command and lens, status kept across an edit of another function
  and reset by an edit of the function, `unavailable` with reason, typed
  argument errors.
- `background_runs_are_debounced_and_publish_status` (unix): four edits
  inside the window give exactly one run and one audit, for the last version,
  with `vesper/proofStatus` and `workspace/codeLens/refresh`; lenses then show
  `proved` for both obligations; nothing reruns while unchanged.
- `an_edit_cancels_the_running_background_job` (unix): the stale run's
  process is killed after the edit.
- `semantic_proof_lenses_with_real_lean`: real Lean 4.24.0 and the real
  auditor, run when `VESPER_LEAN` and `VESPER_LENS_AUDITOR` are set: `inc`
  proved by omega and kernel-replayed, `add` refuted with a counterexample
  (shown in the lens title), the extraction obligation proved; a second
  server reuses the cached `.olean` (`olean_compiled: 0`) and still replays it.

The existing 8 Lens session tests pass unchanged except for the stand-in
auditor, which now echoes one record per `--theorem`.

Unit tests: `proof_model` (models and reasons, identity follows the function
and its callees only, counterexample rendering, and
`renderings_check_under_real_lean`, which compiles both renderings of eight
functions under real Lean and checks the expected proved/refuted split);
`vesper-lsp` crate tests for `Debouncer` and `PendingRequests::is_pending`.

## Latency (§21.3)

Setup: 4-vCPU Intel Xeon @ 2.10 GHz cloud VM, Linux 6.18, rustc 1.97.0,
`--release`, nothing else running, both binaries measured back to back with
`target/release/examples/keystroke_latency <vesper> <fixtures> --iterations 200`
(n = 400 per edit kind). Valid and damaging edits are as in
`lens-remainders.md`; `lens` is a whole-document `textDocument/inlayHint`
after each valid edit, which builds the Lens view of the new text. Values are
median / p95 in ms. Main is `283c811`.

| Fixture | Bytes | Valid, main | Valid, lane | Damaging, main | Damaging, lane | Lens view, lane |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ticket_tally.ves | 1891 | 1.356 / 1.889 | 1.436 / 2.027 | 2.181 / 3.019 | 2.413 / 3.574 | 0.275 / 0.449 |
| pipeline.ves | 1461 | 1.061 / 1.291 | 1.286 / 1.658 | 1.527 / 1.785 | 1.858 / 2.504 | 0.342 / 0.466 |
| lease_cleanup.ves | 1083 | 0.888 / 1.044 | 1.035 / 1.389 | 1.159 / 1.383 | 2.013 / 2.728 | 0.273 / 0.484 |
| transaction.ves | 383 | 0.369 / 0.461 | 0.542 / 0.833 | 0.532 / 0.666 | 0.723 / 1.096 | 0.258 / 0.448 |
| warp_ack.ves | 10336 | 0.609 / 0.827 | 0.924 / 1.328 | 1.234 / 1.399 | 1.604 / 2.127 | unavailable |
| warp.ves | 9546 | 1.564 / 1.913 | 1.844 / 2.529 | 4.453 / 4.973 | 5.232 / 7.377 | unavailable |
| kv.ves | 4537 | 0.844 / 1.172 | 1.149 / 1.499 | 2.310 / 2.831 | 2.700 / 4.188 | unavailable |

Findings:

- This VM is slower than the one the wave-six figures came from (main's
  ticket_tally valid median is 1.36 ms here, 0.81 ms there), so only the
  side-by-side columns compare.
- The lane costs 0.1 to 0.3 ms at the median on valid edits. That is the
  extra thread hops (reader → dispatcher → session → dispatcher) that make
  cancellation possible while a request runs, plus, on `warp_ack`, the Lens
  view the server now builds when the editor profile refuses for resources.
- Damaging edits cost 0.2 to 0.85 ms more at the median: the trial parses
  that validate each candidate repair. The worst p95 on an editor-checkable
  fixture is 3.57 ms (ticket_tally, damaging), over the 2 ms budget; main is
  also over it there (3.02 ms) on this VM.
- **The 10 KB fixtures still do not get a checked Lens view.** `warp_ack`
  fails `vesper_doc::source_preflight`, whose weighted expression/signature
  bound (256) is a constant inside `crates/vesper-doc` that `AnalysisLimits`
  does not control, so raising Lens's own budgets cannot reach it (reported
  as `lens_resource_limit`). `warp` and `kv` are refused by the editor guard
  for generic constructor inference (`editor_guard_rejected`), which is a
  supported-feature guard, not a budget. Both are outside this lane's paths.
  The Lens-view column for them is the time to that typed refusal
  (0.23 to 0.30 ms median).
- Single runs on a shared VM; max values include scheduler noise.

## Checks

With `CARGO_TARGET_DIR` private and `CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`:

- `cargo fmt --all -- --check`: clean.
- `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  clean (one pre-existing `unused dependency` manifest warning in
  `vesper-pact`, not this lane).
- `cargo test --locked -p vesper --lib lsp`: all pass (see the workspace run
  below for the count).
- `VESPER_LEAN=…/lean-4.24.0-linux/bin/lean VESPER_LENS_AUDITOR=target/debug/vesper cargo test --locked -p vesper --lib -- lsp::lens_tests::semantic_proof_lenses_with_real_lean lsp::lens_tests::proof_lens_with_real_lean_and_auditor proof_model`:
  pass (real semantic test 39.7 s for two servers × three runs; model
  renderings 5.0 s).
- `cargo test --locked -p vesper-lsp`: pass, including 2 new crate tests.
- `cargo test --locked -p vesper --test lsp_cli --test lsp_actions --test lsp_navigation --test diagnostics_cli --test doc_navigation_cli --test forge_parameters_cli --test recovery_cli`:
  pass.
- `cargo test --workspace --locked --no-fail-fast`: pending (recorded in a follow-up commit)
- `python3 tools/bazel/sync_build_files.py`: no BUILD changes.

## Decisions

- **No root patch.** `serve`'s signature is unchanged; the session thread is
  created inside `serve_with`, so `src/main.rs` needed nothing.
- **Editor-side recovery, checker untouched.** As in wave six, repairs are a
  presentation layer over the normal parser and checker. Rust ownership of
  the host code is not evidence that the Vesper source typing rules for
  PoisonedRecovery or pattern obligations are implemented; no source typing
  rule changed.
- **A Lens-owned model instead of the extraction for semantic theorems.**
  `partial def` bodies are opaque to the kernel, and changing the extraction
  (`src/lean*.rs`) is outside this lane. The model is small, bounded
  (4096 nodes, 16 parameters, inlining depth 8) and rejects everything it
  does not cover with a typed reason.
- **`bv_decide` is a counterexample finder, not a prover here.** Its proofs
  need `Lean.ofReduceBool`, which `vesper proof-audit` (rightly) cannot
  replay, so only `omega` proofs count as `proved`.
- **Standard axioms for semantic theorems.** `omega` proofs use `propext`,
  `Quot.sound` and `Classical.choice`; the extraction obligation keeps the
  empty policy. Per-theorem axioms come from the audit report.
- **Background runs are opt-in** because they start Lean processes after
  idle time; the spec's 150 ms is the default delay once enabled.
- **Waiting at end of input.** The server now answers everything already read
  before returning on EOF, which also keeps `shutdown`/`exit` streams that
  close stdin immediately working (the `diagnostics_cli` tests depend on it).

## Remaining gaps

- The 10 KB fixtures are not checked by Lens: the fixed `vesper-doc`
  preflight bound and the generic-inference guard need changes in their own
  crates.
- Latency regressed by 0.1 to 0.85 ms at the median (thread hops and repair
  validation). A writer that is `Send` would remove one hop; it needs a
  signature change in `serve`'s callers.
- Semantic theorems cover arithmetic faults of scalar functions only: no
  structs, linear values, effects other than `Fail`, loops via recursion,
  `match`, or user contracts. `omega` cannot prove nonlinear facts
  (variable × variable, division by a variable); those end as
  `checked_native` or `refuted`.
- The extraction lens is still the axiom-freedom audit; the kernel sees only
  opaque constants for `partial def`s.
- Cancellation of a running request is cooperative: Lens traversals stop
  early, but a salsa query or navigation computation already running
  completes before its (dropped) result is discarded.
- Loom.toml capability ceilings in completion ranking are still not modelled.
