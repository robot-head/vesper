# Ordinary-if integer fact preservation (#497, #567–#569)

Base: `b91fbae747012d47977f4b0d66cd0e2bd6ca861b`. The live open PRs at
start were #823 (inventory/CI timeout), #752 (paired ranks), and #750 (UI
model); none owned the checker/reference/lockstep change. This work is on
`codex/if-fact-join`, independently based on main.

## Behavior

The original `let x = 5; let y = if x == 0 { 1 } else { 2 }; x + 1`
reproducer now checks and evaluates to 6. Branch refinement and existing
branch diagnostics still run. An ordinary two-returning-arm join first keeps
all facts agreed by both arms; it restores a missing incoming exact integer
only when the declaration and mutation identity are unchanged in both arms.
The incoming snapshot is taken after checking the condition, so writes in
the condition cannot revive facts from before that write.

Each private Rust `Binding` has an `Arc<()>` identity. Environment clones
share it. Assignment (including a root field write), `inout` invalidation,
and borrowed-fact invalidation replace it even if the old fact was unknown
or the new value happens to compare equal. Identity uses `Arc::ptr_eq`.
Generic joins preserve a common identity or create a fresh one, so nested
if/match/short-circuit writes remain visible. Missing branch bindings cannot
certify an unchanged value. Constructors give shadowed bindings fresh
identities, and restoration also checks declaration spans. `Never` selection,
nonzero intersection, move histories, effect rules and public schemas retain
their existing behavior. The identity allocation is checker bookkeeping;
this change makes no checker allocation or latency guarantee.

The reference fragment has no assignment or `inout` syntax. Its
`restoreIfFacts` pairs incoming and joined bindings by list position and
name/type, filling only a missing integer fact. It preserves all other joined
fields. Only the ordinary-if call site uses it. `names_restoreIfFacts`
proves names framing and the existing `shape_ok` argument uses that lemma.
The statements of `ok_typed`, `shape_ok`, `reference_checker_sound`, and
`reference_checker_sound_closed` are unchanged. Their overlap soundness is
not a proof of the mutable Rust checker or of mutation identity tracking.

## Regression and validation

- Twenty checked-arithmetic tests pass with Lean enabled, including the exact
  reproducer, equality/inequality and swapped zero conditions, nested joins,
  prior assignment, shadowing, nested writes through if/match/short-circuit,
  condition writes, and `inout` invalidation. Existing overflow, resumption,
  linearity and backend comparisons remain enabled.
- The original rejection fixture is renamed to
  `accept_fact_preserved_after_if.ves` without changing its source. Four
  additional fixtures cover inequality, swapped zero, nesting and shadowing.
  The live Lean reference regenerated `fixtures.golden` using the existing
  `VESPER_LOCKSTEP_BLESS=1` mechanism. Those five programs evaluate to 6;
  the original rejection becomes acceptance. Other fixture outcomes are
  unchanged, including all 23 retained rejections.
- All 59 representable fixtures agree between Rust, evaluator/JIT and the live
  Lean reference. The deterministic 400-program generated corpus was rerun
  live; its golden bytes are unchanged. No large nightly campaign was run.
- The reference and unchanged `MetatheoryFull` dependency compiled freshly in
  a private directory with Lean 4.35.0. The overlap soundness proof compiled;
  its four public theorem reports use only `propext`, `Classical.choice`, and
  `Quot.sound`. The new names-framing lemma is included in axiom output.

Final checks passed:

```sh
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --test checked_arithmetic
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean VESPER_LOCKSTEP_BLESS=1 cargo +stable test --test lockstep -- --test-threads=1
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --test lockstep -- --test-threads=1
cargo +stable test --test lockstep -- --test-threads=1
cargo +stable clippy --locked -p vesper --all-targets --no-deps -- -D warnings
cargo +stable fmt --all -- --check
python tools/bazel/sync_build_files.py --check
git diff --check
```

The arithmetic run reported 20 passed. Both live and offline lockstep runs
reported seven test functions passed; the opt-in large nightly campaign's
function returns without launching that campaign. The live runs separately
compile/audit the soundness proof. The offline run uses recorded goldens and
does not substitute for the live proof check. No golden entry was edited by
hand. The original fixture bytes and the generated golden bytes were compared
to main and match exactly. All new fixtures pass `vesper fmt --check`.

