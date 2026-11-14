# Lens: the 10 KB fixtures, a Send writer, Loom ceilings in completion (§21)

Wave-eight lane 16, claim issue #465. Base `1094e64` (main after #477, a
descendant of the required `ea1ed29`). Branch `claude/project-thread-t1o23r`.

Edited paths:

- Lane paths: `crates/vesper-lsp/**`, `src/lsp.rs`, `src/lsp/**` (new
  `src/lsp/session/manifests.rs` and `src/lsp/fixtures/`), this report.
- `crates/vesper-doc/src/lib.rs`: the preflight-bound hunk, in its own commit
  (lane 17 owns the crate; the change is additive).
- `src/tooling/frontend.rs`, `src/tooling/frontend/tuple_growth.rs`,
  `tests/tuple_analysis.rs`: the generic-inference guard fix, in its own
  commit. No wave-eight lane owns these files (see "Decisions").
- `src/main.rs`: one line, in its own commit (`io::stdout()` instead of
  `io::stdout().lock()`, because `serve` now needs a `Send` writer).

No new dependencies, no root manifest or BUILD changes.

## What was built

### 1. The large fixtures check (§21.3)

Two things stopped them, as `lens-rest.md` recorded.

**The preflight bound.** `vesper_doc::source_preflight` had fixed bounds
(weighted segment 256, 64 `if` tokens, 128 `fn` tokens, nesting 48).
`warp_ack.ves` peaks at 260. The crate now has `PreflightLimits` (validated,
with hard caps) and `source_preflight_with`; `source_preflight` keeps the
fixed profile exactly. The editor facade scales the bounds with its AST
profile: `max_ast_nodes / 8` for the weighted segment, `/ 32` for
conditionals, `/ 16` for functions, never below the fixed values. So the
default editor profile (2048 nodes) is byte-for-byte unchanged, and Lens's
profile (8192 nodes) admits 1024 / 256 / 512.

**The generic-inference guard.** The editor guard refused any use of a
generic constructor (`Some`, `None`, `Ok`, `Err`, user generic ADTs) before
checking, to keep inferred types from growing exponentially. The fixtures
use `Option` and `Result` throughout. The guard now hands those programs to
the numeric growth envelope (`tuple_growth.rs`, which already bounded
inferred tuples), extended to model how the checker types them:

- The checker infers constructor arguments locally and bidirectionally
  (`Checker::construct`): each type argument comes from the expected type or
  from a sub-term of a field's actual type, refined at most once per field
  (`refine_annotation`), never through global unification. So an argument is
  bounded by the expected type's envelope plus the sum of the field
  envelopes, and the result by the ADT node plus one such bound per type
  parameter.
- Expected types are threaded down as their own envelopes, at every place
  the checker passes one: function bodies, annotated `let`s, block tails,
  branches and arms, unary operands, the right operand of a binary (checked
  against the left one's type), assignment right-hand sides, call arguments
  (parameter types), constructor fields (the field type with the expected
  arguments substituted, bounded by `nodes(field) × nodes(arg)`), and the
  operand of `?` (`Result[expected, _]`).
- A nullary constructor clones a matching expected type, so it is bounded by
  it. Explicit arguments (`Result::Ok[u64, u32](4)`) replace the expected
  type with `Adt[types]`.
- Patterns and projections on generic values substitute the scrutinee's
  arguments into the generic ADT field templates (`field_of`).
- `?` yields the operand's first argument. `handle` follows
  `Checker::handle`: the return clause binds the body's type (with the
  state), the result is that clause's type or the expected type, and each
  operation clause binds the operation's parameters with the effect's
  arguments substituted. The continuation's arrow type carries a refined
  effect row this envelope does not model, so any use of the continuation
  name is refused ("handler continuation use is unsupported before
  checking"); its environment footprint is still charged.
- Joins still add (not max) branch envelopes, so `Ok(a)` / `Err(a)` joined
  into `Result[A, A]` doubles per binding and is refused with a resource
  error before the checker runs.
- Still refused: generic *functions* and methods (inference through
  `apply` is unmodelled), closures, indexing, traits and conformances, a
  user ADT named `Option`/`Result`, a user variant spelled like a builtin
  one, and one spelling that names several generic variants.

This envelope is an argued over-approximation of the checker's behaviour,
not a proof about `src/check.rs`. Ownership of Rust values in the envelope
is not evidence that any Vesper typing rule is implemented.

**Large buffers in the editor.** A buffer the editor profile refuses only
for its budgets is now checked once per source hash with the same
`check_source` under Lens's profile (`Session::large_checked`). A pass
publishes `status: "checked"` with `vesper.profile: "lens"`; a semantic
failure publishes the checker's diagnostics; anything else keeps the
editor's own refusal. Navigation (hover, definition, symbols) and the Lens
view reuse that checked snapshot. The checker and its facts are unchanged;
only the (still finite) bounds are larger.

Results, by fixture:

| Fixture | Bytes | Before (main) | Now |
| --- | ---: | --- | --- |
| `kv.ves` | 4537 | `editor_guard_rejected` | checked in the editor profile |
| `warp.ves` | 9546 | `editor_guard_rejected` | checked in the editor profile |
| `warp_ack.ves` + `warp.ves` (`src/lsp/fixtures/warp_ack_full.ves`) | 22045 | `lens_resource_limit` | checked under the Lens profile |
| `warp_ack.ves` alone | 10336 | `lens_resource_limit` | `editor_guard_rejected` |

`warp_ack.ves` is a fragment: its first line says it is appended to
`warp.ves`, and `vesper-sim-session`'s test concatenates the two. Alone it
calls `pn_space_left`, `XdpNic::…` and other names it does not declare. The
preflight no longer stops it; the envelope then refuses the calls to
unknown names (it cannot tell an unknown name from a trait or effect call
the checker would resolve), which is the honest outcome for a fragment. The
complete program is `src/lsp/fixtures/warp_ack_full.ves`: `warp.ves`
followed by `warp_ack.ves`, run through `vesper fmt` (the original
`warp.ves` is excluded from formatting). The fixtures live under
`src/lsp/` because root tests may only read files of the root Bazel
package; `kv.ves` is copied unchanged for the same reason.

### 2. A Send writer for `serve`

`serve` and `serve_with` now take `impl Write + Send` (not `'static`). The
session thread runs on a scoped thread and writes its own responses and
notifications; it no longer sends them to the dispatcher to be written. The
writer and the pending-request table are shared behind locks (order:
pending, then writer). A response is written only while its request is
still pending, under the table's lock, and a cancellation answers under the
same lock, so every request is still answered exactly once. The dispatcher
still answers `$/cancelRequest` as soon as it is read. Write failures on the
session thread end the server with the error. This removes the session →
dispatcher hop for every response and every `publishDiagnostics`.

### 3. Completions ranked by the Loom.toml capability ceiling (§21.1.5)

- `vesper_lsp::lens`: `CandidateMethod.effects` (every effect in the row,
  rendered `Name` or `Name[Args]`), a new tier
  `CompletionTier::ExceedsCeiling = 3` (`"exceeds_capability_ceiling"`),
  `ceiling_grants` (a bare name grants every instance, `"Ffi[LibLz4]"` only
  itself), and `rank_methods_within(…, ceiling)`. A method that needs an
  effect the ceiling does not grant ranks last, after methods the frame
  blocks, with the badge `exceeds Loom.toml capability ceiling: Net` and the
  deprecated tag. `rank_methods` is `rank_methods_within(…, None)`.
- The server never reads files, so a ceiling applies while the client has
  the manifest open: a `didOpen` of a URI whose last segment is `Loom.toml`
  goes to a separate store (`src/lsp/session/manifests.rs`, at most 64
  manifests), is parsed with `vesper_loom_calculus::parse_manifest`, and
  applies to Vesper documents whose URI is under the manifest's directory
  (the deepest manifest wins; URIs are compared as strings). An invalid
  manifest constrains nothing and publishes a
  `VESPER_MANIFEST_UNAVAILABLE` warning on itself. Completion items carry
  `data.vesper.capability_ceiling`.

### 4. Cooperative cancellation between salsa queries

Each salsa query (`guarded_analysis` wraps one `check_source` call) runs to
completion and stays memoized. A cancelled request now stops at the next
query boundary: between documents in `workspace/symbol` and
`vesper/signatureSearch` (one guarded analysis per open document), and
between the analysis query and building a Lens view (for a large buffer, a
second check). It answers `-32800` and caches nothing. The query bodies
themselves are not interruptible without changing `src/database.rs`,
`src/check.rs` and the facade.

## Latency

Setup: the same 4-vCPU Intel Xeon @ 2.10 GHz cloud VM as `lens-rest.md`,
Linux 6.18, rustc 1.97.0, `--release`, nothing else running. Both servers
(`main` at `1094e64`, and this branch) measured with this branch's
`target/release/examples/keystroke_latency <vesper> <fixtures> --iterations 100`
(n = 200 per edit kind), run as main, lane, main, lane; the table shows the
second pair (the first pair agrees within noise). The client now also times
a `textDocument/hover` at the edited position after each valid edit, and
prints p99. Values are median / p99 in ms.

| Fixture | Valid, main | Valid, lane | Damaging, main | Damaging, lane | Lens view, lane | Hover, lane |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ticket_tally.ves | 1.350 / 2.164 | 1.072 / 1.685 | 2.051 / 3.360 | 1.696 / 2.474 | 0.255 / 0.529 | 0.203 / 0.474 |
| lease_cleanup.ves | 0.935 / 1.359 | 0.855 / 1.322 | 1.674 / 2.715 | 1.357 / 2.685 | 0.200 / 0.337 | 0.162 / 0.322 |
| transaction.ves | 0.445 / 0.786 | 0.480 / 0.817 | 0.593 / 0.958 | 0.612 / 1.217 | 0.220 / 0.441 | 0.180 / 0.355 |
| kv.ves | 0.828 / 1.415 † | 3.074 / 4.670 | 2.038 / 3.894 † | 4.824 / 7.805 | 0.398 / 0.802 | 0.240 / 0.538 |
| warp.ves | 1.621 / 2.573 † | 6.953 / 10.801 | 3.811 / 7.775 † | 8.289 / 15.121 | 0.507 / 1.006 | 0.299 / 0.520 |
| warp_ack.ves (fragment) | 0.680 / 1.042 † | 1.807 / 2.623 † | 1.175 / 1.697 † | 2.308 / 3.251 † | refused | refused |
| warp_ack_full.ves | 1.655 / 2.418 † | 13.500 / 20.164 | 2.901 / 5.377 † | 10.951 / 20.230 | 1.238 / 2.736 | 0.544 / 0.938 |

† the time to a refusal (`analysis_unavailable`), not to checked diagnostics.

Findings:

- **The Send writer.** On the fixtures both servers check, the lane is
  0.08 to 0.3 ms faster at the median on valid edits for the two larger
  programs and equal within noise on `transaction.ves`; p99 drops by up to
  0.5 ms on `ticket_tally`. Run-to-run noise on this VM is of the same order
  (the first pair gave ticket_tally 1.37 vs 1.55 ms), so the honest summary
  is: one hop fewer, a gain of at most a few tenths of a millisecond, not
  separable from noise on the smallest file.
- **Hovers are within budget everywhere**: under 1 ms p99 on every fixture,
  including the 22 KB program. Lens views (whole-document inlay hints) are
  under 1.3 ms median, 2.8 ms p99.
- **Diagnostics on the large programs are over the 2 ms budget.** Main
  answered quickly because it refused them; checking them costs 3.1 ms
  (kv), 7.0 ms (warp) and 13.5 ms (warp_ack_full) at the median. Measured
  in-process (release, 20 runs each): `check_source` on `warp.ves` takes
  6.1 ms, of which `check::check` alone is 4.8 ms; on `kv.ves` 2.6 ms and
  1.9 ms. So the checker itself, not the envelope or the transport,
  dominates, and the whole module is rechecked on every edit. Meeting the
  budget at this size needs per-function incremental checking (§21.3 Layer
  2-3 firewalls) in the checker and database, outside this lane.
- The warp_ack fragment's refusal now runs the envelope instead of stopping
  in the preflight, which costs about 1.1 ms more.
- Single runs on a shared VM; maxima include scheduler noise.

## Tests

New or changed, all passing:

- `src/tooling/frontend/tuple_growth.rs` unit tests:
  `generic_constructors_try_and_handlers_reach_the_checker` (Option,
  Result patterns, explicit `Result::Ok[u64, u32]`, a user generic struct,
  `?` and a `Fail` handler check; an ill-typed `Some` is a semantic error),
  `doubling_through_generic_arguments_is_refused_before_checking`
  (25 `Ok`/`Err` joins and 25 `P { l: a, r: a }` bindings are refused with a
  budget error before checking, also under Lens's profile; 25 nested
  `Some`s and a deep annotated `None` are admitted and check),
  `continuations_and_shadowed_builtins_stay_uncertain`,
  `lens_profile_checks_the_large_simulation_programs`.
- `tests/tuple_analysis.rs`: the generic-projection case
  (`p.value` on `Option[(i64, i64)]`) moved from "refused" to
  `generic_projection_is_bounded_and_reaches_the_checker` (the checker's
  E0202).
- `src/lsp/lens_tests.rs` (real stdio frames):
  `the_large_simulation_programs_get_diagnostics_hovers_and_lens_views`
  (kv in the editor profile, warp_ack_full under the Lens profile: checked,
  hover, checked inlay hints; a type error publishes E0200 with
  `profile: "lens"`), and
  `completion_ranks_methods_outside_the_open_loom_ceiling_last` (pure
  ceiling ranks `persist` (Storage) last with its badge and tag; a document
  outside the manifest's directory is unconstrained; granting Storage
  restores the frame ranking; an invalid manifest and a closed one
  constrain nothing).
- `src/lsp/session.rs`:
  `cancelled_requests_stop_at_query_boundaries_and_cache_nothing`; the
  "analysis unavailable" test now uses a generic function value, since
  `Some(0)` checks.
- `crates/vesper-lsp/tests/lens.rs`:
  `methods_outside_the_capability_ceiling_rank_last_with_the_missing_effects`.
- Every existing Lens, LSP and cancellation test passes unchanged against
  the new writer.

## Checks

With a private `CARGO_TARGET_DIR`, `CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`:

- `cargo fmt --all -- --check`: clean. `vesper fmt --check src/lsp/fixtures/`: clean.
- `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  clean (the pre-existing `unused dependency` manifest warning in
  `vesper-pact` only).
- `cargo test --locked -p vesper --lib -- lsp tooling::frontend`: pass.
- `cargo test --locked -p vesper --test lsp_cli --test lsp_actions --test lsp_navigation --test diagnostics_cli --test doc_navigation_cli --test forge_parameters_cli --test recovery_cli --test tuple_analysis`: pass.
- `cargo test --locked -p vesper -p vesper-doc -p vesper-lsp --no-fail-fast`:
  everything passes except tests that need `lean` on `PATH`, which this VM
  does not have (`lean4lean42_acceptance`, `oracle42_acceptance`,
  `oracle42_runner`, `replay42_kernel`, `resource_model42`,
  `proof_audit_private`), the same as on main here.
- `cargo test --workspace --locked --no-fail-fast`: 4462 passed, 24 failed.
  The two `diagnostics_cli` failures were this lane's (the raised-profile
  recheck published an oversized diagnostic batch); fixed by keeping the
  editor's diagnostic budgets, and `diagnostics_cli` then passes. The other
  22 are environmental and fail on main here too: the six Lean tests above
  and `vesper-loom-build` `build`/`cli` (`UnknownFingerprint`, see
  `lens-lane` notes).

## Decisions

- **Fix the guard where it lives.** The refusal that stopped `warp` and
  `kv` is in `src/tooling/frontend.rs` and its growth envelope, which no
  wave-eight lane owns and which the assignment names ("fix the
  generic-inference guard"). The change is in its own commit. It widens
  what the editor admits only where the envelope bounds the checker's
  clones, so the guard's purpose (no exponential type growth before the
  checker's own budgets apply) is kept.
- **Default editor profile unchanged for the preflight.** The preflight
  scales with `max_ast_nodes`, so only a larger profile raises it.
- **A raised-profile recheck instead of raising the editor profile.** It keeps
  the editor's diagnostic budgets (a batch too large to publish stays a
  refusal, as `diagnostics_cli` requires). The
  editor profile's budgets stay as they were for every buffer that fits;
  only buffers it refuses for resources get the Lens-profile check, memoized
  per source hash, with the profile reported.
- **Ceilings from open manifests only.** The server has no filesystem
  access, so a Loom.toml must be open to apply. Effects are matched by name
  or exact rendering, as `vesper-loom-calculus` lists them.
- **Scoped session thread.** It lets `serve` take a borrowed, non-`'static`
  writer; the reader thread stays detached so a blocked read cannot keep
  `exit` from returning.

## Remaining gaps

- Diagnostics latency on the 4.5 to 22 KB programs is 3 to 13.5 ms median,
  over the 2 ms budget, because the whole module is rechecked per edit.
- The `warp_ack.ves` fragment alone stays refused (undeclared names).
- Generic functions and methods, closures, indexing and traits are still
  refused before checking in the editor profile.
- Cancellation is at query boundaries only; a running `check_source` still
  completes.
- Ceiling ranking covers member completions (methods), not free functions,
  and needs the manifest open in the client. Lexically handled effects are
  counted like any other effect in the row.
