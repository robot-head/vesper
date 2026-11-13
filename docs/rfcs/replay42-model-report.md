# Static rewrite replay model (claim #240)

Implemented `lean/RewriteWitness.lean` after frozen contract `63d60cd` was published and linked to the public child claim. The coherent model is commit `20a4edcef6905088eccbbc25b40eaca864d67e81`. Only the model and this report are owned by this lane; no source checker, exporter, build/CI file, dependency, or existing fixture was changed.

The model imports only `Std` and provides the exact `Vesper.RewriteWitness` API: four nominal kinds, a distinct kind-indexed `Value` structure, explicit decidable integer Fits, exact fitting literals, stable copied environment variables, and strict Option-valued semantic functions. Checked arithmetic computes mathematical Int results and refuses out-of-range values. Wrapping arithmetic uses BitVec operations. `none` is the single empty abortive overflow failure; this is not a model of arbitrary effects or divergence.

All 24 existing rule theorems and nine support theorems use the frozen names, parameter order, endpoints, and guards. Rules 6/7/8/17/18 require actual operand Totality; rule 16 requires Totality of its canceled second operand. Wrapping commutativity and associativity apply to partial operands too. The six fold theorems require closed value-level equalities in addition to explicit Fits proofs, permitting independent kernel `decide` proofs at each generated constant instance. Equality, congruence, symmetry/transitivity, and equality-to-leaf Totality transport support topological semantic/proof references without expanding the term DAG in the model interface.

The source prints axiom closures for all 33 fixed public theorem roots, including rules unused by any particular fixture. All are subsets of `{propext, Quot.sound}`. There is no sorry, project axiom, native_decide, Classical.choice dependency, or caller-selected replacement semantics. This required avoiding Lean 4.24 Std helpers `BitVec.toInt_le`, `le_toInt`, `ofInt_toInt`, `mul_one`, and `one_mul`, whose actual closures include Classical.choice. Signed bounds use `BitVec.isLt` and the direct toInt conditional; literal roundtrip follows constructive injectivity and exact decoding. The wrapping multiply-by-one proof uses direct Nat modular arithmetic. Conjunctions are split before omega to avoid its otherwise observed choice dependency in the bounds probe.

## Actual validation

Pinned toolchain executed:

```text
Lean (version 4.24.0, x86_64-unknown-linux-gnu, commit 797c613eb9b6d4ec95db23e3e00af9ac6657f24b, Release)
```

Commands from `/workspace/vesper-worktrees/replay42-model-plan`:

```sh
/home/agent/.elan/bin/lean --version
/home/agent/.elan/bin/lean -o /tmp/RewriteWitness.olean lean/RewriteWitness.lean > /tmp/replay42-model-static.log
LEAN_PATH=/tmp /home/agent/.elan/bin/lean /tmp/RewriteWitnessCases.lean
git diff --cached --check
```

The actual model compiled cleanly. A bounded Python assertion checked every one of the 33 printed theorem closures against exactly the permitted axiom set; none contained a forbidden axiom. The model source SHA-256 is `4f373f08a9741a93e79f1ef4bc02535a94334af059b16f936d650b171b6a1ce4` (an observation of this source, not approval or authenticity evidence).

The private `/tmp/RewriteWitnessCases.lean` imported that actual compiled model and kernel-checked 40 examples: all six fold families at all four kinds; maximum-plus-one overflow for each kind; unsigned zero-minus-one underflow; U64 maximum multiplication overflow versus modular result 1; signed MIN times -1 overflow versus wrapping MIN; refusal to treat a failing operand as Total; checked/wrapping erasures that would change failure without their Total premise; and wrapping commutativity/associativity with genuinely partial operands. This is model arithmetic evidence, not a Rust/Lean executable differential oracle.

Three additional private probes imported the actual model and were required to exit nonzero with their expected diagnostic: `/tmp/RewriteWitnessBadKind.lean` rejects U64-to-I64 Value substitution despite equal bit widths; `/tmp/RewriteWitnessBadLiteral.lean` rejects Fits for 256 at u8; `/tmp/RewriteWitnessBadFold.lean` rejects a checked u8 255+1-to-0 fold with kernel `decide`. A Python subprocess wrapper enforced a 30-second timeout, checked the nonzero statuses and diagnostic categories, and confirmed all three expected rejections. These probes are temporary lane evidence, not checked-in CI fixtures or a claim that the independent acceptance suite ran.

The worktree was first fast-forwarded to the published contract as authorized and was otherwise unchanged outside the two owned files. No Cargo build/cache, push, PR, or protected-branch merge was used. Root owns the model-CI registration/execution, and the independent acceptance lane owns the genuine exporter-to-Lean compilation and explicit proof-audit replay; neither is claimed by these static checks. Lean formatting has no configured formatter here; the committed model passed the staged whitespace check.

## Limits

The theorems establish mathematical equalities in this fixed model. They do not prove Rust admission/checker correctness, AST-to-model binding, source ownership/exhaustion, backend refinement, machine execution, DDC/self-hosting, or kernel-time/memory bounds. The existing `k_i ⊑ m_i` field restriction and all compiler behavior remain untouched. Sharing-friendly declarations avoid deliberate source expansion, but no linear-time elaboration or replay claim is made. Independent kernel replay, axiom-policy enforcement, genuine generated artifacts, combined Rust tests, hosted/platform results, and release integration remain separate coordinator/acceptance evidence.
