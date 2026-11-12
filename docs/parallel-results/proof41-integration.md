# Explicit formatter document-model verification — issues #116–120

`vesper verify SOURCE --format` now captures the actual first rendered formatter
Doc, builds a bounded private projection and generates a Lean witness through the
structured Lean AST. It compiles the bundled logical model and witness with real
Lean 4.24.0, then reuses the existing kernel-replay auditor with an empty axiom
policy. A successful report has scope `formatter_document_model`; `--emit-lean`
publishes the witness only after the same checks. Input files remain untouched.

The private FormatVerification association is returned only after ordinary
formatting has passed token/comment conservation, reparsing and idempotence.
Actual source lexer comments must match the retained model payloads, and hashes
derive from actual source/output bytes. No reconstructed count-only document,
caller-supplied hash, cached approval, CheckedSource or semantic World is used.
Default fmt/check/LSP behavior does not execute Lean or macro components.

The projection validates current-owner Doc IDs and prepays expanded occurrences,
including repeated shared children, payload/depth/metadata/work and retained
comment copies before allocation. AST emission preflights escaped bytes,
recursive printer temporaries, AST storage and work before cloning/printing.
All Lean values use typed Module/Term/Declaration nodes with fixed imports and
names; source payloads remain escaped strings. The central AST gained ordinary
list literals and a total noncomputable definition kind. Existing ordinary,
partial and abbreviation behavior is unchanged.

The Lean model proves ordered tagged-comment conservation for arbitrary
group-choice schedules and a generic projection-subset/disjointness lemma. Its
public Doc.concat normalizes ordered lists to binary append. This preserves the
emitter API while avoiding a nested inductive recursor unsupported by the pinned
strict auditor. Logical definitions are explicitly noncomputable to omit unsafe
codegen auxiliaries. The auditor policy was not relaxed, and no axioms, sorry,
partial or native_decide proof was introduced.

## Evidence and integration

Claims #116–120 and proof41-contracts.md preceded writers; all claim comments
were checked before delegation. Work used separate worktrees and lightweight
private one-job caches. The live design remains at 41 sections and Appendix A,
modified 2026-11-12T09:18:30.204Z; §39.4 was reread. Fresh open-board and actual
PR101 filename checks left all external Warp work untouched. The owner has since
merged PR101 as af03122, so the pending parent stack is being synced with main.

The initial model elaborated and printed empty axiom dependencies, but actual
kernel replay correctly rejected compiler-generated unsafe helpers and the
nested recursor. Those real failures drove the model normalization above. The
fixed model passes the unchanged auditor: 186 replayed constants, both required
theorems with empty axiom lists, no allowed or forbidden axioms.

Final end-to-end CLI tests pass all five Unix cases, including real kernel
acceptance, current source/output hashes, fixed Expr macro documents without
expansion, independent hostile-payload witness checking, invalid/opaque/resource/
file/tool failures and final-symlink rejection. The independent Python harness
passes 28 commands, three actual kernel JSON reports and a separately compiled
witness with actual theorem/axiom and injection-rejection checks. No fake Lean
executable or fabricated version output was used. The unavailable alternative
toolchain-version case is not claimed as tested.

The combined formatter/Lean-AST suites pass 67 tests in 11 suites, no ignores,
with actual Lean configured, including private construction failures and real
emission/corruption checks. Existing source/kernel-security regressions pass
81 tests in six suites; the six artifact-audit cases still reject forged,
partial, sorry and unauthorized-axiom artifacts and do not execute package
initializers. Strict workspace all-target Clippy passes on Rust 1.99.1. Native
three-OS CI remains a separate required validation of this pending stack.

An independent read-only review found no certificate bypass in capture,
projection, source association, AST emission, resource preflight, environment
isolation or output gating. The emitter's escaped-printer temporary bound was
tightened from a concrete control-payload regression before final acceptance.

## Scope still unfinished

This verifies the document-algebra model instantiated with an actual formatter
projection, together with runtime source conservation checks. It does not prove
that Rust refines Lean, that the lexer/parser is correct, that a source program
is semantically valid, or that an actual compiler autofix satisfies the generic
projection lemma. The selected toolchain and artifact parser remain trusted.
Recovery-format proofs and valid documents exceeding the conservative proof
profile can explicitly refuse verification. No RSS, timing, package provenance
or reusable proof-cache certificate is claimed. These remaining requirements
stay within the active design implementation goal.
