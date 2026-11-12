# Lean effects and mutable places: results

Lane: Lean effects and mutable places (`docs/agent-prompts/lean-effects-places.md`).
Branch: `claude/project-thread-2qcc39`, based on `origin/work` at `3be6891`.
Owned paths changed: `src/lean.rs`, `src/lean_ir.rs`, `tests/lean.rs`, `lean/EffectPlaces.lean`, this file.

## Slice delivered

Mutable `inout` (and borrowed `in`/default) effect-operation arguments, and handler bodies that mutate bindings outside the handler, now extract to executable Lean. This includes clauses that write a borrowed argument before and after a non-tail `k` resumption, stateful handlers, aborting clauses, nested handlers, labeled evidence, disjoint projected arguments, and clauses that perform outer effects. Lean compiles each extraction and `#eval` prints the same value as the reference evaluator.

Before this change, extraction rejected every operation with an `inout` parameter ("Lean handler inout operations require state-passing operation evidence"). It also produced Lean that did not compile whenever a handler body, branch, or block mutated a binding declared outside that nested expression, because Lean `do` blocks cannot mutate an outer `let mut` from a nested `(← do …)` term.

## Audit: what the evaluator observes

Each of these was checked against `vesper run`:

1. Borrowed operation arguments are aliases. `perform` binds every non-`sink` parameter pattern to the performer's slot (`src/runtime.rs`, `bind_pattern_slot`). A clause write before `k` is visible to the resumed body. After `k` returns, the clause reads the slot's final value, including body writes made after the operation. A clause write after `k` lands in the performer's place, which is visible after the handler when that place is outside it. Default and `in` parameters alias too: in `tell(x, k) => { let r = k(5); x * 100 + r }`, `x` reads `y`'s later value.
2. Mutations in a handled body to outer bindings persist after the handler, and also when a clause aborts. For example, `x += 5; Stop::stop()` with an aborting clause leaves `x == 5`.
3. Clause and return-clause captures are snapshots taken when the handler is installed. Writes to them stay local to the clause.
4. The checker already rejects abortable effects that cross an active `inout` parameter, whether in a helper or a clause (`E0402`). As a result, a helper's unsynchronised writes are never discarded by an abort.

So yes: operation evidence must return explicit mutable state, and that alone is not enough. Writes after a resumption, and aborts, need the alias target. That target lives outside the clause and crosses the handler boundary.

## Encoding

A handler becomes `HandlerCPS (S × W) (O × W) parent`. `S` is the handler's own state and `W` is its *frame*: the outer bindings its body can write, through assignment or mutable arguments. Closures and clauses are excluded because they capture snapshots.

* **Operation evidence.** `class Effect_E` methods return `m (R × X₁ × …)` for operations with `inout` parameters, the updated arguments after the result. Performers restore only declared projections, as the CFG backend does for calls.
* **Frames.** The body copies `W` into fresh mutable variables. Before every action, including effect operations, effectful calls, nested handlers, and primitive failures, it stores the current frame of every enclosing handler body in that handler's CPS state, lifting through inner layers. The return clause and aborting clauses answer with `(o, state.2)`. The handler's caller reassigns `W` from the answer.
* **Resumption lenses.** Each handler emits one implementation per operation that takes an `Option` lens `(W → X) × (W → X → W)` for every borrowed parameter. A perform site that lexically resolves to that handler supplies `some lens` when the argument's root is in the frame, and `none` otherwise. Resolution follows the innermost handler, a label, or a closure's labeled row. The type-class dictionary uses `none`. A direct `k(v)` in the clause resumes with `(v, current inout values)`, takes the resumed frame from the answer, and reloads borrowed arguments through their lenses. At clause exit, `inout` arguments are written back through the lens. Projected parameters write back only their projections, so `inout a.{x}` and `inout b.{y}` on one root do not clobber each other.
* **Statement-level mutation.** The effectful generator now emits A-normal form: compound expressions bind `let t ← if/match/do …` do-elements, which Lean permits to reassign outer mutable variables. Every source binding receives a hygienic Lean name, because Lean rejects shadowing a mutable variable. Operands are bound to temporaries when a later operand has effects, which preserves left-to-right order. `&&`/`||` stay short-circuiting when the right side has effects. Code after a `Never`-typed statement is not emitted.

`lean/EffectPlaces.lean` defines the same `HandlerCPS` text the extractor emits; a test checks this. It proves, without axioms (each theorem is checked with `#print axioms`):

* the monad laws used;
* that an aborting clause observes the last synchronised frame;
* that the tuple and composed lenses are lawful;
* that writes to disjoint frame components leave each other unchanged;
* write-back laws, including disjoint projected write-backs.

A worked instance computes the evaluator's `1020302` program with a lens. It also shows that a snapshot (no lens) would give a different answer. These are facts about the encoding. They do not show that extraction preserves Vesper semantics.

## Explicit rejections

The extractor reports an error, rather than extracting a snapshot, in these cases, because the evaluator would observe a change:

* A clause accesses a borrowed argument after resuming, and the argument's place is local to the handled computation. The evaluator reads the dead local's final value, which no frame holds. Message: "…the borrowed place is local to the handled computation".
* A clause accesses a borrowed argument after resuming, and the handled body may reach the operation through an effectful helper, callback, or closure. Those cannot supply lenses. Message: "…through a helper, callback, or closure".
* A continuation is used other than as a direct call in its own clause, for example captured by a closure, while the handler has a nonempty frame or the operation has borrowed parameters. The resumed frame would be lost. When neither applies, the existing first-class continuation fallback is still used.
* A clause writes a borrowed argument and then performs an action that may abort. The checker already rejects these programs (`E0402`). The guard is kept as a second check.

## Public API

No public signatures changed. `vesper::lean::extract(&World) -> Result<String, String>` is the entry point. Generated output differs as follows:

* Handlers are typed `HandlerCPS (S × W) (O × W) m`. Previously they were `HandlerCPS S O m`.
* Effect class methods with `inout` parameters return tuples.
* Local names are hygienic, for example `«x'1»` and `«$t3»`.
* The header sets `linter.unusedVariables false`.

`lean_ir::extract_function` now returns `None` instead of an error when a lifted closure body needs effect lowering. The whole enclosing function then uses the effectful generator. This affects, for example, a pure closure containing a handler.

## Validation (Lean 4.24.0, pinned `lean-toolchain`)

```sh
cargo fmt --all -- --check                                   # clean
VESPER_LEAN=lean cargo test --workspace --locked             # 280 passed, 0 failed
VESPER_LEAN=lean cargo test --test lean                      # 49 passed (36 existing + 13 new)
cargo clippy --workspace --locked --all-targets -- -D warnings  # clean
lean lean/Foundations.lean && lean lean/RuntimeFrames.lean && lean lean/SpecificationCounterexample.lean && lean lean/EffectPlaces.lean  # all succeed
```

The new tests pin the evaluator's value first, so a change on either side fails them, then compile and run the Lean output. They cover:

* stateful handlers;
* nested handlers;
* disjoint projections;
* aborts, including `Fail` and `Never` results;
* continuation effects: clauses performing outer effects, and `k` passed to a generic function;
* labeled evidence;
* generic effects;
* helpers and closures;
* hygiene.

Three negative tests cover the rejections above. A test also requires every `EffectPlaces.lean` theorem to depend on no axioms.

Regression sweep (not committed): every `fn main` program literal in `tests/*.rs` and `examples/*.ves` that the evaluator runs (192 programs) was extracted and executed in Lean. Scalar results agree for 180 programs, against 168 at `origin/work`. No program that agreed before regressed. Remaining failures are unchanged from `origin/work` and outside this slice:

* generic `==` needs `BEq` instances: `same[T: mod copy]`;
* `f64 %`;
* effectful callbacks created outside the handler that later runs them (`let get = || Ask::ask(); handle Ask in { get() }`, factories passed through `apply`);
* a non-positive recursive datatype;
* calls through tuple-indexed callbacks;
* `i64::MIN / -1`, where Lean's `Int64` division differs from the evaluator's wrapping result.

`scripts/test-aarch64.sh` was not run, because this lane does not change native code generation.

## Integration requirements

* No root `Cargo.toml`/`Cargo.lock`, shared AST/HIR/MIR, CLI, or CI changes. No new dependencies.
* Suggested addition to the acceptance list in `docs/parallel-work.md`: `lean lean/EffectPlaces.lean`.
* Suggested update to `docs/implementation.md`. Replace the Lean column entries "Lexical CPS handlers, including non-tail resumption" with "…including mutable operation arguments and frames". Replace "remaining nested mutable effect cases" with the rejection list above.
* No shared IR contract change was required. Handler bodies and clauses remain high-level `Op::Handle` nodes in Place IR, so the effectful generator still works on the checked AST. It uses `check::Place` and the checker's type, call, and partial-operation tables. A CFG-based effectful extractor would need the IR to record each nested body's frame, meaning the externally captured slots it writes. It would also need a perform-site alias descriptor relating an operation argument's place to the enclosing handler's frame. I recommend adding both to `Op::Handle` and effect `Op::Call` when the coordinator next revises Place IR. That would let `lean_ir.rs` replace the AST generator and let native code share the same alias facts.

## Ambiguities and proposed decisions

1. *Do default (`in`) operation parameters alias?* The evaluator binds every non-`sink` parameter to the performer's slot. The extractor follows the evaluator, refreshing these parameters after resumption. Proposed decision: keep this, and state it in the specification.
2. *Clause capture timing.* The evaluator snapshots clause captures at handler installation, not at perform time. The extractor matches. The design (§11.2, "restricts ambient handler captures Γ_h to copy") is consistent with either reading. Proposed decision: installation-time snapshots.
3. *Reads of a dead body-local after resumption.* The evaluator returns the slot's final value. A pure model must either retain dead locals or reject such programs. Proposed decision: reject in the checker as a borrow that outlives its frame, which would also remove the extractor's rejection.

## Limitations

* The extraction is executable `partial def`. It is not a proof of compiler preservation or of termination.
* One-shot use is guaranteed by the checker, not by the Lean types.
* Writes after resumption via helpers, callbacks, or closures, and first-class continuations with nonempty frames, are rejected rather than modeled.
* The effectful path still translates the checked AST rather than shared Place IR CFGs, for the reason given above.

## Integration review (2026-11-12)

The review found two omissions in handler-frame discovery for argument modes
that callers leave implicit. Method sugar does not have a checked arrow type,
so mutable method arguments were absent from the frame. Also, merging
same-named labeled effect operations truncated the parameter list to the first
operation's arity. Both could generate Lean that tries to mutate an outer
binding from a nested computation.

Frame discovery now includes method signatures after omitting the receiver and
merges selectors across all argument indices. This remains a conservative
saved-frame analysis. Two new evaluator-versus-Lean regressions each return
`42`: an implicit mutable method argument inside a handler, and same-named
labeled operations with different arities.

Validation used actual Lean 4.24.0. The method regression reproduced the invalid
Lean before the fix; both targeted regressions pass afterward. The other 50
Lean tests passed during the review, including the axiom audit for
`EffectPlaces.lean`. Workspace formatting is clean. The integration owner runs
the final combined workspace checks after applying this review.
