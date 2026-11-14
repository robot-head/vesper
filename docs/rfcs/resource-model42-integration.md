# Executable resource model integration

Status: **complete local acceptance; required hosted CI remains the merge gate**.
PR: <https://github.com/robot-head/vesper/pull/323>.
Claims: #322 (model), #358 (kernel artifacts), #380 (private-name replay),
#392 (measured CI scheduling).

The new computable pure ANF model covers unit/naturals, finite copy inputs,
variables, strict lexical lets, modal pairs/full unpack, numeric branches and
one linear ticket kind. Private validated/checked/invocation wrappers retain
actual validation, inference and input-admission results. The approved field
premise `k_i ⊑ m_i` remains mandatory.

Actual inference constructs Full typing; admitted inputs construct initial
Full.WT. Actual steps/prefixes, returned values and ordered events correspond
to Full execution. Typed runs exclude invariant failures; refusals witness
required space or independently measured current/proposed/intermediate excess.
Preservation, progress and completed-copy exhaustion/public-trace exact-once
consumption derive from Full. A returned owner remains legal with live residuals.

## Executed final validation

The independent worktree executed source head
`94c0e4753a8ac6a9d9a4f8187698449e17e82cb5`. Its 33 model/tool/fixture/test/auditor
source files matched integration bytes. The [source manifest](resource-model42-source-manifest.json)
is a byte association, not a proof of binary derivation or native equivalence.
The actual toolchain was Lean 4.24.0 commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`.

| Actual target | Result | Test time |
| --- | --- | --- |
| Existing proof_audit_cli | 6 passed, no failures/ignored/filtered | 89.25 s |
| New proof_audit_private | 1 passed, no failures/ignored/filtered | 13.61 s |
| New resource_model42 | Complete matrix passed, no failures/ignored/filtered | 412.15 s |

The model freshly compiled all **12 modules and 78 fixed semantic assertions**.
All **284 explicit source theorem roots plus one actual proof alias** passed
strict kernel replay and the standard `Classical.choice`, `Quot.sound`, `propext`
axiom policy in three batches of at most 128, with exact union coverage.

The actual inventory was 3,839 unique original constants, 3,853 raw occurrences,
14 repeated theorem occurrences, 3,791 unchanged canonical declarations,
48 explicitly inventoried partial native auxiliaries omitted, and one mapped
alternate-proof alias. All **3,792 output declarations** were kernel-checked and
export/reimport-checked. The 21,858,464-byte projected object has BLAKE3
`511f9481e7bcefad5bb2adbf1940072a086f791dc00eb7a09f45d8cb55c8aed6`.

Actual hostile controls passed: missing/corrupt original and projected objects,
four inaccessible constructors, an AST-built forbidden-sorry proof, raw total
recursion refusal, safe suffix retention, explicit unsafe/source-partial/fake
unsafe refusal, project-axiom rejection, clean alternate proofs, and a clean
canonical proof whose forbidden alternate fails only when its mandatory alias
is included. There were 29 compilation attempts (25 successes/four intended
privacy refusals), 11 snapshot attempts and 13 strict audit calls. Refusals
were checked, not silently skipped.

Final strict Clippy for all three targets passed (8.198 s), workspace formatting
passed (4.165 s), and Bazel member metadata/whitespace checks passed. The complete
model command took 413.286 s including outer overhead. #392 gives it the long
900 s budget: full Linux/Windows execution, duplicate macOS invocation skipped,
build everywhere. The new private-name portability regression still runs on
macOS. No local macOS/Windows run is claimed; hosted required checks remain the
merge gate. See the [acceptance report](resource-model42-acceptance.md) for exact
commands, identities and process evidence.

## Artifact and replay boundary

Lean emits partial native `_unsafe_rec` auxiliaries for ordinary total
recursion, including unchanged Full definitions. Original executable objects
do **not** pass a blanket no-partial artifact audit. The new
[kernel-only exporter](resource-model42-kernel-artifacts.md) preserves canonical
imported logical ConstantInfos exactly and rejects explicit unsafe/package
opaque/unsafe logical dependencies. Matched native auxiliaries are separately
inventoried. Every alternate safe theorem proof keeps its actual type,
universes and body in an explicitly mapped alias; every alias passes the same
strict axiom audit. Package IR/extensions are not exported. Original/projected
artifact bytes and tool source are bound before/after use.

The [private-name replay fix](resource-model42-private-replay.md) uses genuine
`Kernel.Environment.addDeclCore` checks instead of the elaborator name-table
path that panics on distinct valid private names sharing a displayed spelling.
No unchecked insertion, private renaming or dropping occurs. Constructor,
recursor, inductive and complete ConstantInfo equality checks remain. CLI/schema
and unsafe/partial/axiom policies are unchanged; the auditor implementation and
its source hash change. The same private fixture genuinely panics before the
fix and passes afterward.

Three earlier independent full runs compiled all 78 assertions but correctly
refused generated Repr/equality helpers or different equation-theorem proof
copies. Unused Repr derivations were removed; an equality-only rewrite was not
integrated. A producer attempt exposed the private-name replay panic. Reports
retain those failures; they are not passing runs. The final complete run needed
no weakened assertion or source fix.

## Scope and limits

This proves the restricted logical model, not equivalence of omitted native
implementations, Rust/compiler refinement, native resource observation, parsed
source adaptation, full ordered diagnostic agreement, external Lean4Lean
coverage of this new profile, a 100000-program resource campaign, full-calculus
completeness or self-hosting. Bounds are logical bounds, not whole-process
allocation or throughput guarantees.

Changes are new model/tool/tests/reports plus the narrowly claimed auditor
replay and CI scheduling fixes. Compiler/runtime/native/metatheory/Cargo
implementation files remain unchanged by this work. Already-merged main through
`c025b9a` is included as a dependency. The [contract](resource-model42-contracts.md)
and lane reports give the exact fragment, limits, catalogs and checkpoints.
Generated Lean source uses vesper-lean-syntax AST and one render; fixed modules
and controls are handwritten.
