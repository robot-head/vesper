# Finite resource model syntax (#322)

Owned source checkpoint: `d51a21b90509e8ffe9bb0f17f8e036af8e75fb28`, new
`lean/ResourceModel42/Syntax.lean`. The implementation follows frozen contract
`ddf5d6c` and the zero-cap/admission clarification `0399d20`. Its baseline is
`30de7fef8be040a890e34f26370ee15b1429642e`; no existing model or compiler file
was changed. The coordinator imported this source as `c821dde` before dependent
writers started.

## Exact interface

All names are in `Vesper.ResourceModel42`.

| Area | Published definitions |
|---|---|
| Types | `Floor`, `Ty`, `Ty.erase`, `Ty.kind`, `Ty.nodes`, `Ty.depth`, `Ty.WF` |
| Proven type cache | private-constructor `TypeInfo`; readonly `ty`, `nodes`, `depth`, `nodes_eq`, `depth_eq`, `wf`; `TypeInfo.makePair`, `pair`, `inspect`, `ofTy` |
| Source | `SourceAtom`, `SourceExpr`, their `site`/`erase` definitions; `RawProgram { inputTypes, body }` |
| Admission | `ValidationError`, `SourceStats`, `SourceWalk`, `validateAtom`, `validateExpr`, `validateTypeList`, `ValidatedData`, `validateRaw`, `validate` |
| Validated wrapper | private-constructor `ValidatedProgram`; readonly `raw`, `limits`, `data`, `validation_ok`, `inputInfo`, `stats`, `inputInfo_types`, `inputs_copy` |
| Runtime syntax | `RuntimeValue`, `RuntimeAtom`, `RuntimeExpr`, their `erase` definitions; `SourceAtom.toRuntime`, `SourceExpr.toRuntime` |
| Substitution and initialization | `RuntimeAtom.subst`, `RuntimeExpr.subst`, `RuntimeExpr.asValue`, `closeInputs` |
| Measured storage | `ValueStats`, `ExprStats`, `CapacityError`, `measureValue`, `RuntimeValue.measure`, `measureAtom`, `measureExpr`, `RuntimeExpr.measure` |
| Before-copy checks | `SubstitutionPlan`, `lift`, `plannedVariable`, `RuntimeExpr.preflightSubst`, `preflightUnpack`, `RuntimeContext`, `plug`, `erase`, `preflightPlug`, `preflightInputs` |

`SourceAtom` has unit, natural and variable constructors, each with a source
site. `SourceExpr` has atom, pair, strict let, full unpack, if-zero, mint and
consume. Pair/consume/unpack/if-zero operands are atoms. Source has no handle
literal. Pair floors are copy/affine only, and `TypeInfo` carries the actual
field-modality premises from `Ty.WF`. Its pair constructor checks expanded
node count and depth before creating the inferred pair type. Type kinds are
exactly the kinds of their erased Full types.

Source sites must be below `limits.rawNodes` and unique across the entire
source tree, including both branches. An atom-expression wrapper adds no site
or occurrence. Numeric variable bounds are profile checks; lookup against the
actual lexical context and natural-literal range checking belong to Checker.
The natural literal remains representable in validated source so inference can
report its semantic range error. No invocation values become static facts.

`ValidatedData` includes genuine input-type and copy-kind evidence;
`ValidatedProgram.validation_ok` ties that data to the actual public validator.
Checker owns its separate private checked wrapper and Evaluator owns its
separate invocation wrapper. Returning a linear result remains legal.

Runtime atoms admit complete closed pair/handle values after substitution.
Only mint/consume retain event sites in runtime syntax. `asValue` recognizes
closed pairs without an operational transition. Unpack preflight measures
right-at-zero followed by left-at-zero, checking both intermediate and final
terms before copying either. Input-closing pure lets put input 0 innermost;
their nodes and depth are included by `preflightInputs` and their later actual
silent transitions must consume evaluator fuel.

## Bounds and refusal behavior

`Limits` exposes `rawNodes`, `context`, `rawDepth`, `typeNodes`, `typeDepth`,
`valueNodes`, `valueDepth`, `initialValueNodes`, `inputs`, `expressionNodes`,
`runtimeDepth`, `resources`, `events`, and `fuel`, with the exact frozen
defaults and `Limits.hard` maxima. `Limits.valid` rejects values above hard
maxima; zero retains its ordinary numeric meaning. Zero inputs admits input-free
source, zero resource/event space is handled at the actual operation, and zero
fuel does not itself reject an admitted terminal value. Zero shape/type/storage
caps naturally admit no object requiring that space.

Type metrics, value metrics, expression metrics, lexical context, and operation
budgets are distinct. In particular `typeNodes` and `valueNodes` are independent:
a well-typed pair can exceed a lower dynamic value cap. The before-copy planner
detects newly closed pair islands created by substitution, including islands in
stored unselected branches and enclosing let contexts. It does not execute
those branches. All recursive admission/measurement traversals have an explicit
depth budget and refuse over-cap totals; they do not first materialize a
substituted expression to discover its size.

`RuntimeValue.measure` measures representation only. It does not assert that a
natural is in u64 range, that a value has a requested type, or that a handle is
an allowed input. Those are invocation/checking obligations. Resource-ledger
updates, event reservation, unchanged-prefix refusal, step execution and
exhaustion are Evaluator/Proofs responsibilities. These limits describe this
logical model, not all allocations of Lean or a compiler process.

## Actual evidence and remaining work

Under the coordinator's exclusive bootstrap lease, the source writer executed
the actual pinned `/home/agent/.local/share/vesper-lean/bin/lean`, with
`LEAN_NUM_THREADS=1`:

1. Compiled unchanged `lean/MetatheoryFull.lean` to a private `.olean`: exit 0.
2. Compiled the final Syntax source against that private dependency: exit 0,
   with an empty final diagnostic log.

The work root was `/tmp/resource-model42-syntax-bootstrap`, with `full.log` and
`syntax.log`. The immutable, read-only dependency checkpoint is
`/tmp/resource-model42-syntax-api-d51a21b`, containing the exact source and
compiled Full/Syntax modules. No `.olean` was written into or tracked by the
checkout. `git diff --check` passed. The bootstrap lease was released after the
coherent checkpoint; no Cargo command or additional Lean execution was run.

The current manual theorem catalog is exactly
`Vesper.ResourceModel42.TypeInfo.pair_ty` and
`Vesper.ResourceModel42.TypeInfo.ofTy_ty`, both public and kernel-checked during
compilation. No transitive axiom audit is claimed here. The independent
acceptance lane will audit the complete assembled catalog; the proof lane owns
the new erasure/substitution, checking, execution, progress and conditional
copy-exhaustion theorems. Read-only API review found no remaining syntax
blocker. Compilation is not a claim that those future bridges are proved, that
Rust/source behavior is bound, or that a resource differential campaign ran.
