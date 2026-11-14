# Executable resource model: independent acceptance (#322)

## Design checkpoint, not executed evidence

This lane owns only `lean/ResourceModel42/Acceptance.lean`, `tests/resource_model42.rs`, and this report. The published contract is `ddf5d6c`, frozen against main `30de7fef8be040a890e34f26370ee15b1429642e` and specification snapshot `8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d`. At this checkpoint only this design report has been written. No Lean/Rust implementation, model compilation, Cargo command, CLI probe, kernel audit or external replay was executed by this lane. Source authoring waits for the syntax writer's first coherent API checkpoint.

This is a finite logical reference model, not an observation adapter. Its own Mint/Consume trace is not a native or evaluator backend trace. Existing runtime-provider, arithmetic-campaign and external Lean4Lean results do not establish this new model's correctness or coverage.

## Fixed semantic matrix

The eventual handwritten acceptance module will prove named equalities about actual validation/checking/invocation/evaluation APIs. It will not receive expected answers as authoritative model inputs, fabricate checked wrappers, or replace computation with caller-supplied typing assumptions. Exact constructors, result accessors, error names and theorem names follow the coherent syntax/checker/evaluator checkpoints.

| Control | Required property |
| --- | --- |
| Terminal unit, fuel zero | Completed without a transition or invented trace event |
| Bare mint, fuel one | Completed owned handle, one Mint, one live residual; no blanket exhaustion guard |
| Let mint then consume | Exactly three Full transitions: mint in let context, value substitution, consume; fuel three completes, fuel two retains Mint-only prefix and the live handle |
| Two simultaneous handles | Distinct fresh serials, actual ordered mints, forward/reverse consume order distinguished; serial allocation does not reuse a consumed identity |
| Move through new lexical slot | Same handle identity, no extra terminal event; stale reuse/alias is rejected by checking |
| Pair and full unpack | Structural transfer preserves child IDs, left binds at de Bruijn index one and right at zero; selected consume order is observable |
| Owned result and pair result | Completed handle/pair of handles retains exactly the owned residual; copy-result exhaustion is a separately conditional theorem |
| Strict unused initializer | An unused copy result whose initializer mints and consumes still executes that initializer; an unused linear initializer fails checking |
| De Bruijn scope/new slots | Introducing a scalar does not overwrite an older owner; new index zero versus older index one remains distinct; scope leaks and type errors stay separate |
| Both branches checked | A constant-zero condition cannot hide an ill-typed or linearly mismatched other arm; runtime executes only the selected accepted arm |
| Affine-only demand disagreement | Explicit profile refusal rather than a falsely claimed semantic ownership error or full Rust diagnostic match |
| Modal pair fields | `k_i ⊑ m_i` holds independently per field; ticket under copy/affine field override is rejected; copy/affine nominal floors do not relax a child obligation |
| Naturals at the boundary | Static literals zero and `2^64 - 1` accepted; `2^64` rejected by literal inference; invocation values checked separately and never change static acceptance |
| Copy-only initial inputs | Declared types are checked independently of actual values; invocation shape/range/cap refusals are distinct; closing lets count as actual fuel with input zero innermost |
| Compact copy doubling | Expanded type, per-value and whole post-substitution expression bounds account for duplicated children, even when host objects share storage |
| Dynamic refusal after mint | A lower independent value/expression cap refuses before allocation or ledger/event commitment, retaining the actual prior term, Mint prefix and live handle |
| Global fuel | Nested initializers/branches do not reset it; exact predecessor fuel exposes the current admitted state and prior prefix without cleanup consumes |
| Resource/event zero/exact caps | Deliberate first-operation refusal is not success; tombstones count toward total minted history; all proposed step growth is prepaid before commitment |

Pair floors are restricted to copy/affine by this contract; nominal-linear aggregate-root identities are not tested as supported. Source atoms are unit/nat/variables, not computed pair expressions or raw resource literals. Runtime values produced by substitution can be pairs/handles. Every fixed source occurrence uses a unique bounded logical label; those labels are distinct from slots and serials and are not Vesper byte spans.

Type/admission refusal and dynamic expansion refusal must be exercised independently. A test that is rejected by the checker before evaluation is not evidence of evaluator allocation preflight. Conversely, a dynamic refusal must retain a genuine already-committed prefix, not a syntax-derived expected ledger. Whole proposed substitution growth includes both branches, enclosing let contexts, and intermediate as well as final unpack substitutions.

The checker promises a first structured semantic error only. Acceptance will distinguish syntax/profile limits, semantic type/use failures, affine-only profile refusal, invocation failures, runtime invariant errors, fuel exhaustion and measured execution limits. It will not claim ordered Rust diagnostic equivalence. Accepted reachable execution must not be justified solely by a success-soundness theorem that would also permit an always-refusing evaluator; the progress/refusal classification and prefix correspondence are required audit roots.

## Private wrappers and proof scope

The raw program contains input types and source syntax, never inferred result types, usage masks, acceptance booleans, raw handle IDs or expected traces. `ValidatedProgram`, `CheckedProgram`, and any validated invocation expose only the frozen read-only API and genuine validation/inference evidence. Fixed handwritten compile-time privacy controls may check that public unchecked constructor names are unavailable; this is elaborator/API evidence, not a kernel theorem or reusable approval certificate.

The model proof roots must establish erasure, value/substitution correspondence, actual inference-to-Full.HasType soundness, input closure, finite ledger freshness/update, successful Full.Step correspondence, complete Full.Steps prefix correspondence, completed-value soundness and progress/refusal classification. The completed-copy exhaustion/exact-once claims keep their copy-result premise. No returned-owner empty-ledger test or posthoc guard substitutes for them.

## Planned actual kernel harness

The new Rust integration target will use the actual pinned Lean 4.24 compiler and actual existing `vesper proof-audit`. Both missing default tools and an explicitly supplied missing/wrong tool hard-fail; there is no skip path. Temporary artifacts contain only fixed handwritten model/acceptance source files copied from the checkout and genuine compiled dependencies. Any generated negative-control Lean, if needed, uses `vesper-lean-syntax` nodes and one render, never Rust text templates.

The fixed dependency order and complete all-new-named-theorem catalog will follow the actual writer checkpoints. Every public named theorem must be selected in actual replay/audit requests. The existing auditor accepts theorem roots, not direct definition roots: authoritative validation/checking/step/evaluation/substitution/metric definitions must occur in the selected theorems' actual transitive dependency closure. Computed semantic equalities provide that linkage; any additional fixed definition-identity witness is association evidence, not an extra semantic theorem. Requests have at most 128 names; a larger catalog is explicitly partitioned, with an asserted complete union and no omissions. One bounded serial matrix will avoid a Lean process per theorem or needless recompilation of the unchanged Full dependency.

Actual success assertions include schema version, exact selected root union, `accepted = true`, `kernel_replayed = true`, `cache_eligible = false`, nonempty checked constants and artifact inventory, actual tool/source associations and unchanged artifact bytes. Transitive axiom sets must be subsets of exactly the sorted standard allowlist `Classical.choice`, `Quot.sound`, `propext`; those are permitted axioms, not required ones. No sorry/admit/project axiom is accepted. Fixed `#print axioms` output is supplementary to actual replay, not a substitute for it.

Meaningful negative controls include a missing/corrupt actual object and, if included, a separate AST-built custom-axiom theorem that genuinely compiles but fails the standard policy. Such a deliberately rejected control is isolated from the successful model catalog. Tool/process outputs, deadlines and capture are finite; processes are serial and reaped, and failures remain failures rather than empty successful evidence. No missing model or tool can silently pass.

Cargo/Bazel discovers the new integration target, but existing BUILD/CI ownership stays with #320/#321. Actual target elapsed time will be measured before requesting any coordinator timeout adjustment. No new dependency, registration, source adapter, external Lean4Lean target/profile, native integration, ChoiceTape campaign, 100,000-program result, source-checker refinement or full §42 completion is authorized by this acceptance slice.

## Current status

Design only. The syntax API checkpoint, implemented theorem catalog, semantic assertions and real compile/replay results are pending. No source/test/artifact success is claimed by this document.
