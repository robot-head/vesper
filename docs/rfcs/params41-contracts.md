# Parameter-aware pure source macros

Claims #157/#159–162 precede source writers. Baseline is published PR156
head55610b451e2a26abfdc2cc82af7ccd82195acb82, stacked on142/115/110. Fresh open
issues and actual external PR filenames found no Forge/syntax collision. New
external #158 was detected before writers: it owns Bazel/build files, all
.github/workflows/** and scripts/test-*.sh. We leave those paths alone and posted
exact native coverage requirements there. Claude #121–136/domain/native/main CLI
hooks remain reserved. Live design remains41sections/AppendixA, modified
2026-11-12T09:18:30.204Z; §16.4 was reread.

## Ownership

| Claim | Branch | Exclusive writes |
| --- | --- | --- |
| #157 | params41/integration | this contract, forge_commands.rs help, integration report, review and merge |
| #159 | params41/syntax | macro_source.rs, narrow parser.rs if necessary, new source_macro_parameters tests, syntax report |
| #160 | params41/host | typed_quote.rs, private resolve.rs helper, new typed_function_parameters tests, host report |
| #161 | params41/compiler | forge_frontend/lower.rs, mod.rs, narrow authority.rs helper, new forge_parameters tests, compiler report |
| #162 | params41/acceptance | new forge_parameters_cli.rs, parameter fixtures, check-source-macro-parameters.py, acceptance report; existing CLI/Python negative fixture updates only |

The syntax lane owns the obsolete parameter-unsupported case in
crates/vesper-syntax/tests/source_macro_syntax.rs. Acceptance owns the analogous
cases in tests/forge_cli.rs and scripts/check-source-macros.py; replace them with
still-unsupported projected/generic forms and add positive parameter coverage.
No production or workflow writes by acceptance. External #158 owns native
workflow/wrapper wiring. Existing workspace/native guards remain required.

Separate worktrees/private caches, one Cargo job/debug0/incremental0. Root
target/parallel-next is coordinator-only. Initially2.7GiB free: reuse lightweight
caches, no new Wasmtime/toolchain installation. Preserve core AST/IR/native ABI,
linear exhaustion, authority policy, ordinary semantic providers and AST-only
Lean. Claims remain reserved through reviewed merge. Publish amendments before
dependent writers change frozen interfaces.

## Supported profile and syntax API

Macro-bearing top-level functions may have plain named parameters with existing
pass/type semantics. They remain nongeneric; projected parameters, duplicate
original names, nested/recursive macros and unsupported expression forms refuse.
The existing max_parameters limit bounds macro-function parameter count as well
as macro slots: default8/HARD32. Do not impose that new count restriction on
ordinary support functions; preserve their existing generic/projected forms.

Duplicate original parameter names reject before alpha-renaming. Otherwise
renaming could silently repair a real E0100 duplicate binding into valid source.
Use the existing DuplicateName syntax refusal, without a fabricated Function or
dummy body. Full real parameter mode/type/linear checking occurs on the final
expanded source. This does not declare unsupported source semantically valid.

MacroSource privately retains actual parameter identifier spans, keyed by its
source-bound FunctionId. Public record clones do not alter this inventory:

`MacroSource::parameter_name_spans(function:FunctionId)->Result<&[Span],Error>`.

Span order matches header.params. Locate actual lexer identifiers within each
real Param.span, after an optional in/inout/sink token, and verify exact name,
UTF8 boundaries and header association. Return actual spans for ordinary support
functions too, without promoting them to the macro-function profile. Bound
scans, retained vectors/copies and metadata before allocation. No header padding,
synthetic function body or public caller-provided span inventory is accepted.

## Typed host API

Add:

`HygieneHost::resolve_expr_function_body(&mut self, source:&ExprSource,
syntax:&MacroSource, function:FunctionId,
replacements:Vec<(Range<usize>,ExpandedExpr)>, budget:&mut TypedBudget)
->Result<ResolvedBody,TypedError>`.

Validate current host ownership and exact source bytes against sealed syntax
before looking up the FunctionId/header/spans. Enforce the supported plain,
nongeneric parameter profile even when called directly for an ordinary support
function. No public SourceFunction clone, guest BinderId or hash alone can
provide parameter authority.

ResolvedBody gains `function_id()->Option<FunctionId>` and
`parameters()->&[ResolvedParameter]`. ResolvedParameter fields are private, no
constructor/Deserialize. Frozen getters: `ordinal()->usize`,
`name_span()->&Range<usize>`, `token()->usize`, `binder()->BinderId`.

Keep existing resolve_expr_body behavior/API: function_id None, parameters empty.
The new method returns Some actual FunctionId even for zero parameters. Assemble
and parse the actual body first, then append actual header identifier tokens and
source provenance, leaving body shape/token indices unchanged. Resolve parameter
binders using fresh source-ranked scopes threaded through the initial Ctx. Bare
module-scope locals would tie with globals and become ambiguous.

Use a private resolve_preparsed_with_parameters helper, without changing public
Term or ResolvedFragment wire structures. Parameter binders enter real fragment
inventory but not callable-alias tracking. Calling a function-valued parameter
remains unsupported indirect execution. Preserve existing definition-site claims,
current-host identities and both hygiene directions.

Prepay suffix tokens/provenance, sealed records, candidate scans, initial Ctx
clones and all additional scope work. Extend the existing scope envelope by the
parameter count with checked arithmetic; tokens*(max_depth+2) alone is no longer
sufficient. Do not reset budgets or raise existing caps silently.

## Lowering, header rendering and authority

LoweredBody gains `function_id()->Option<FunctionId>` and
`parameter_renames()->&[ParameterRename]`. ParameterRename is private-construction,
no Deserialize, with frozen getters `ordinal()->usize`,
`original()->&Range<usize>`, `name()->&str`. Metadata comes only from the sealed
actual resolved parameter records, never caller-provided names or World.

Alpha-rename parameter declarations and body references consistently by BinderId.
Preserving original parameter spellings allows a parameter named helper to
hijack a definition-site quoted global helper after raw source serialization.
Reserve all global, parameter, caller and generated spellings before fresh-name
selection. Seed active/seen inventories with exact parameter binders; final
active inventory equals that parameter set, while body locals leave scope.

Add actual parameter identifier edits alongside macro removals/body replacement.
Preflight counts, exact byte size, sorted nonoverlap, maps, copies and work. Only
name tokens change in headers; comments/whitespace/pass/types/effects remain
actual original bytes. Each rename has a Source provenance anchor like existing
alpha-renamed locals, without claiming lexeme equality. Reparse and compare the
actual lowered body AST as before, then derive actual authority and validate IR.

Parameter names are part of the current authority header fingerprint. Keep
public authority::derive strict with its existing API. A narrow private expanded
derivation path may consume the original sealed MacroSource and actual lowered
rename metadata to construct the exact expected renamed header. Validate current
source/function identity, complete parameter order/index/name/span coverage and
only then substitute expected name fields. Compare every other field exactly:
function name, generics, parameter pass/type/projections, result and effects.
No arbitrary public fingerprint override or global weakening of name equality.
Use borrowed typed name overrides rather than cloning full types/Worlds or
patching untrusted JSON. Unchanged support headers retain their strict checks.

The current bind_signature_capabilities keys on enclosing function name,
effect identity and row index, not parameter names. Plain parameter alpha-renaming
preserves that mechanism. Header AST types have no value-dependent parameter
references; field/capability-label body dispatch remains outside the current
typed expression grammar. This batch does not extend that grammar.

## Acceptance and integration

Actual tests must cover parameter/global name collision; capture both directions;
initializer shadowing; unused Copy values; actual linear consumption, duplication
and leak errors; supported in/inout/sink semantics; duplicate names refusing
before alpha; real header-comment/token preservation; repeated argument anchors;
foreign host/source/FunctionId and tampered header association; unchanged support
function forms; resource refusal; current helper-body authority changes.

Use actual check/expand and separately run expanded normal programs to42. Verify
explicit current unsaved editor preview works while original diagnostics/hover
remain unexpanded/unavailable. Actual formatting must conserve comments/tokens,
reparse and remain idempotent. Do not fake binaries, weaken assertions or claim
latency/RSS/per-function rechecking. All Lean emission remains structured AST.

Root updates command help and runs strict workspace checks plus actual old/new
acceptance. #158 owns CI migration and receives final target/script names for
Linux/macOS/Windows execution. Pending native jobs remain pending evidence.
The broader design goal remains active; source Tier2, other macro categories,
cross-package linking and implicit original-source semantic overlays are future
work.
