# Lean4Lean proof scope and external-checking gates (§42, #297)

This is a source-based plan for [issue #297](https://github.com/robot-head/vesper/issues/297),
not an implementation or execution report. No Lean/Lake/compiler builds,
installations, external replay, or transitive axiom audits were executed by this
lane. The only repository change is this document. Compatibility/build probes
belong to the coordinator and the separate tool-compatibility lane.

## Decision

Do not adopt an unchanged historical Lean4Lean executable as a trusted checker
for our Lean 4.24 artifacts. A reviewed 4.24 port may be a useful additional
checker after the security, coverage, provenance, and rejection gates below.
Report its completed checks as **external rechecking of specified declarations
under stated import and axiom assumptions**. Do not describe either a successful
CLI build or the current proof library as whole-implementation self-verification.

Upstream explicitly describes its kernel as derived directly from the C++
implementation and therefore likely to share bugs: it is a second
implementation, with useful divergences, rather than an independent design.
The value of running it does not require overstating that independence.

## Revisions inspected

| Revision | Source observation | Role in a candidate decision |
|---|---|---|
| `93abda080b30139fa84ae92a16367cfeff5639f5` | Toolchain 4.23.0; older proof layout | Historical reference, not the latest 4.23 candidate |
| `4faf51403bc4cb35cf6666d91fc9c938ae46d3aa` | Toolchain 4.23.0; parent of the 4.26 bump | Compatibility lane's plausible port base; unchanged security/CLI blockers remain |
| `6bca7f6fd61bcea09266303e75c437ba9745a32b` | Toolchain 4.26.0 | Not a native 4.24 pin |
| `779c51fdea49c57c60f8039703605faae126b17f` | 4.32.2 bump and declaration-value free-variable repairs | Required security history and discriminating regression design |
| `67f63b0c818920dfaed2ad07ccff187a6a8fe1e3` | Upstream HEAD observed during this review; toolchain 4.33.0-rc2 | Current implementation/proof-scope reference, not a compatibility result |

The inspected upstream toolchain history has no 4.24 entry. A toolchain-file
edit is a port, not evidence of compatible kernel APIs, `.olean` layout, or
sound checking. Pin the complete upstream revision, port patch, Lean release,
and Batteries revision. The compatibility lane identified Batteries 4.24 tag
commit `8da40b72fece29b7d3fe3d768bac4c8910ce9bee`; its actual use/build is not
claimed here. Review later fixes against the selected API, rather than copying
every current patch into the older implementation blindly.

## What the upstream proofs actually cover

The [README](https://github.com/digama0/lean4lean/blob/67f63b0c818920dfaed2ad07ccff187a6a8fe1e3/README.md)
distinguishes four targets. `Lean4Lean` builds the library interface and
`lean4lean` builds the CLI; neither includes proofs. `Lean4Lean.Theory` builds
the abstract metatheory and `Lean4Lean.Verify` builds implementation-to-theory
results. A successful `lake build` can include these targets while still
accepting unfinished proofs and explicit assumptions.

The current [CI file](https://github.com/digama0/lean4lean/blob/67f63b0c818920dfaed2ad07ccff187a6a8fe1e3/.github/workflows/ci.yml)
expressly says Verify contains deliberate `sorry`s and that warnings must not
fail the build. This is a documented boundary, not a reason to infer that those
holes are harmless. Concrete source observations are:

* Historical 4.23 `Verify/TypeChecker.lean` contains conditional correctness
  results for inference, checking, weak-head reduction, and definitional
  equality. Their premises include a translated well-formed environment and
  local context; `checkType.WF` also requires the appropriate free-variable
  condition. They do not establish that every declaration-admission caller
  enforces those premises. The missing value checks are a direct example.
* Current `Verify/Environment.lean` states `Lean4Lean.addDecl.WF` for successful
  checked declaration addition from an already well-formed related environment.
  Its `inductDecl` branch remains `sorry`. Current `Theory/Inductive.lean`
  defines both inductive well-formedness and `VEnv.addInduct` using `sorry`.
  A broad, unconditional whole-declaration-checker verification claim therefore
  exceeds the source, even at the current revision.
* Historical `Theory/Typing/UniqueTyping.lean` contains unfinished structural
  results. Current metatheory has made progress, but still has an unfinished
  branch in that file. Roots must be audited individually, with their actual
  transitive dependencies, rather than judged from the name `Verify`.
* Both old and current `Verify/Axioms.lean` introduce project assumptions about
  opaque/runtime operations, including persistent arrays/maps, expression
  operations, and level representation/normalization. The 4.23 source even
  labels several cached-flag equations “currently false” in connection with
  Lean #8554; current comments distinguish repaired upstream behavior from
  still-assumed equations. Inventory the selected revision's actual closure;
  do not replace a proof audit with a grep count or assume current repairs
  automatically establish the corresponding old-port assumptions.

Well-formed declaration addition is also different from consistency: accepting
a well-typed *axiom* of `False` is permitted by a declaration checker. Axiom
policy is a separate gate. Likewise, a proof about the Lean definitions is not
a proof of the native compiler, runtime, unsafe module loader, deserializer,
or machine code implementing the executable.

## Candidate security and replay blockers

The [779c51f explanation and regression](https://github.com/digama0/lean4lean/commit/779c51fdea49c57c60f8039703605faae126b17f)
are mandatory evidence for selecting a candidate. The old implementation
deliberately removed value-level free-variable checks from safe definitions and
theorems and lacked the opaque check. Upstream records that this was unsound:
type checking primes an inference cache with a fresh local, then the declaration
value names that local after its scope is popped. A type beta-reducing to
`False` lets this accept a proof of `False`. Both inspected 4.23 revisions still
omit the three checks; `4faf514` has not silently fixed the issue.

Require the three actual value checks before inference, with fresh-variable
identities appropriate to the selected implementation, and test all three entry
points. Rejecting an arbitrary *cold* unknown free variable is insufficient:
that already failed before the repair. The upstream test constructs raw
declarations, primes the cache through the type, and checks the specific
free-variable rejection path, alongside accepted closed controls.

Further required review/regression items are:

| Path | Concrete concern | Required evidence |
|---|---|---|
| Historical `Main.replayFromFresh` | Calls `withImportModules #[]`, omitting the requested module | Reviewed target-import correction; actual nonzero target and import closure coverage; absent target must fail |
| `Inductive/Add.lean` | [d7e70a5](https://github.com/digama0/lean4lean/commit/d7e70a5f09d514db2a2b3a052f3a9d2dde153da6) repairs dropped, ill-typed nested-inductive parametric arguments | Port or otherwise establish the needed checks; reject the actual dropped-argument exploit and accept a well-formed nested inductive |
| Prop recognition/projections | [997e298](https://github.com/digama0/lean4lean/commit/997e2985e513793c3dcc2e5ffead983f6b7d1cba) documents a proof-of-False route through `Sort (imax 1 0)` and universe-dependent projections | Review the old path and backport the appropriate invariant; discriminate Prop/proof-irrelevance exploits from valid data projections |
| Primitive, quotient, constructor/recursor handling | Special admission/reconstruction paths are not ordinary theorem checking | Actual primitive types/bodies, Eq-before-Quot, valid/invalid constructor and recursor controls, and relevant upstream hardening tests |
| Loader/import lifetime and module parts | Current source has fixes for compacted-region lifetime and newer module parts | Review against the actual 4.24 format/API; wrong-format, missing/duplicate module and loader failure must fail, not become a skipped success |

The 4.24 `ConstantInfo.value?` API includes theorem bodies by default, and
historical `Main.getUsedConstants` already has an explicit opaque-body fallback.
Do **not** label the later `allowOpaque := true` adaptation itself a proven 4.24
dependency bug. Instead, test dependencies that occur only in theorem proofs
and opaque bodies, and string literals in proofs, against the actual port.

## Distinct acceptance gates

1. **Reproducible candidate.** Record source commit/patch and dependency digests,
   exact Lean/Batteries versions, build commands, compiler and executable hashes.
   Build the library, executable, Theory and Verify as distinct observed results.
   Preserve warnings and failures. Compatibility is not checker correctness.
2. **Real rejection controls.** Exercise `Lean4Lean.addDecl` with checking enabled
   on hand-built invalid declarations, without first asking the native kernel
   or elaborator to reject them. Run the discriminating defects above and closed
   positives. A blanket failure, loader error, missing module, timeout, zero
   declarations, or wrong tool must not count as rejection of the intended
   logical defect. Match the failing entry point/reason. If a CLI rejection is
   claimed, the malformed declaration must actually reach that CLI's checking
   path; a source rejected before an `.olean` exists is not such evidence.
3. **Exact external model/artifact replay.** Compile the exact bundled model and
   structured generated artifact in a fresh private regular-file root. Hash
   source, `.olean` parts and the checker actually used. Explicitly constrain
   search paths; verify the named theorem and expected declaration inventory
   were visited. Test a good artifact plus a false changed result, one
   inconsistent nominal signedness, out-of-range literal and missing dependency,
   with actual external verdicts. A globally changed but well-typed statement
   may still be true: its association failure belongs to the binding layer,
   not an invented claim that a kernel must reject every altered artifact.
   Distinguish native Lean acceptance from the later external check.
4. **Import scope.** Normal CLI replay assumes imported modules correct; the
   README says so. A corrected `--fresh` path can recheck the loaded closure,
   but still needs actual coverage checks and its own failure handling. The CLI
   skips unsafe and partial constants. List what was checked, trusted, skipped,
   or unsupported. Current upstream CI restricts fresh replay to core
   `Init.System.IO`, noting failures for modules importing `Lean` (#17); it is
   not evidence that our full `Std`-using model closure replays successfully.
   Avoid unbounded module-prefix fanout, which upstream CI explicitly warns
   can exhaust memory; supervise the actual chosen target under finite limits.
5. **Target axiom policy.** For each model support/catalog root and each final
   generated theorem, obtain an actual transitive axiom closure and reject
   `sorryAx`, unexpected project axioms, and any non-allowlisted root. Preserve
   the existing artifact policy (currently `propext`/`Quot.sound` where allowed)
   separately from the checker's own assumptions. Include a deliberately
   sorry-backed theorem and a custom-axiom-backed theorem: a type checker may
   accept them, while the policy layer must reject them. Ordinary source grep
   cannot establish this transitive property.
6. **Implementation-proof audit.** Audit exact roots matching any claimed
   checker coverage, for example type inference/checking/defeq results and the
   relevant declaration-addition theorem, after inspecting the selected
   revision's precise names and premises. Capture actual `#print axioms` output,
   source statement, and import closure. Record `sorryAx` and project assumptions
   honestly; do not reuse the model's small axiom allowlist as a fictional
   assertion that upstream currently satisfies it. Completing this gate is
   separate work from deploying an explicitly experimental external checker.

The actual external checker must fail closed on malformed frames/artifacts,
wrong revisions, unsupported imports/constructs, timeout, crash, or incomplete
coverage. A success requires a real visited target and all required rejection
controls; no mocked checker, native-kernel-only rejection, cached stale binary,
or precomputed “accepted” result supplies external-check evidence.

## Claim boundaries and next decision

Keep these facts separate in reports: the exact source-to-artifact association;
native Lean kernel acceptance; transitive axiom policy; external Lean4Lean
replay with its stated imports; and conditional implementation-to-theory proofs.
None alone proves the Rust checker/translator, source binding, backend, JIT,
Lean compiler, or whole Vesper implementation refines the mathematical model.
The existing arithmetic oracle is executable differential evidence, not a
substitute for any of these proof or provenance gates.

The next reviewable result is a pinned, reviewed 4.24 candidate patch plus an
actual build/rejection/coverage report. The coordinator's prototype below is
progress toward that result; the unchanged historical CLI remains unsuitable.
An audited pilot may honestly say that specified Vesper model/artifact
declarations were externally rechecked with enumerated assumptions; a claim of
a fully verified checker requires separate closure of the implementation proof
holes and its remaining trust assumptions.

## Coordinator prototype: observed evidence, not a completed security audit

The coordinator supplied `/tmp/lean4lean42-port6.diff` and the actual
`/tmp/lean4lean42-model-check-47qniaf5/results-port6.json`. This lane inspected
those files without running the checker. The recorded patch SHA-256 is
`17d11684630a4c7d7aa0981235449313df0e9ec34980041d4c55e201fda4b555`.

| Coordinator-executed target/control | Observed result |
|---|---|
| Fresh `RewriteWitness` | Exit 0; 84,702 declarations; 173.966 seconds |
| Fresh `ArithmeticOracle` | Exit 0; 86,270 declarations; 172.626 seconds |
| `UniverseControls`, coordinator-reported | Valid FloatSpec-like datatype accepted by both kernels; undersized universe rejected by the candidate's universe check |
| Exact `DeclFVar` controls, coordinator-reported | Three cache-primed invalid declarations rejected; three closed controls accepted |

The patch retains primitive checks and universe checks. Besides the three value
guards, it imports the actual fresh target with `importAll`, adapts the private
`mainModule?` API explicitly, applies the exact
[`d7a52a83bea3e93968672b89ba88af4372b08f23`](https://github.com/digama0/lean4lean/commit/d7a52a83bea3e93968672b89ba88af4372b08f23)
Church-encoded primitive reflection repair, and applies
[`bc9a736be9c318209992e91ef6a218864b817dcf`](https://github.com/digama0/lean4lean/commit/bc9a736be9c318209992e91ef6a218864b817dcf)'s
constant comparison correction in `NormLevel.le`. These observed successes do
not exercise arbitrary malicious declarations, close Verify's proof holes, or
waive the reachable paths below.

## Disposition of later fixes against the actual 4.24 prototype

“Required” below means a reviewed targeted backport and an actual discriminating
control before presenting the candidate as a hardened external checker. It is
not a request to transplant entire later-version commits. All newly recommended
tests in this table are **unexecuted by this lane**. Native 4.24 is a comparison
baseline, not a negative-result authority: it can contain the same later-fixed
bug, so a hardened candidate may intentionally reject a declaration it accepts.

The main adversarial source is
[`KernelHardening.lean` at `5518bf8`](https://github.com/digama0/lean4lean/blob/5518bf83860bab48e8d53f1f447cdd6c50c30c3f/Lean4Lean/Tests/KernelHardening.lean),
alongside `Tests.NestedInductive`, `Tests.DeclFVar`, and `Tests.Environment`.
Those tests assemble actual `Declaration`/`Expr` values; elaborator rejection
must not replace the candidate check.

Full upstream revisions for the principal outstanding changes are
`d7e70a5f09d514db2a2b3a052f3a9d2dde153da6`,
`997e2985e513793c3dcc2e5ffead983f6b7d1cba`,
`5518bf83860bab48e8d53f1f447cdd6c50c30c3f`,
`cbb70bc3500060f22098c838f92e56bd363834e6`,
`80d3dbef8b8130e49cedbdc4a22fdf0217222ee3`, and
`4172c0fadafa54a8cb39787843f262883bb20e5b`.

| Fix/source | Reachability in `4faf` plus port6 | Disposition and focused control |
|---|---|---|
| `779c51f`: safe-definition/theorem/opaque value closure | Present in port6; all three original entry points were exposed | Retain the applied guards and six actual coordinator controls. Keep error-path assertions; a cold unknown FVar is not the cache exploit. |
| `d7e70a5`: dropped nested-inductive parameters (#14577) | Missing. Port6 abstracts `aux2nested` early and never checks the discarded applications after reconstruction | **Required coupled backport.** Store applications open over the saved parameter context, abstract on restoration, then check them against the final environment. Test `NestedInductive.treeDecl` accepted, `badDecl` containing `Bad0 Bool.true` rejected, and `nestedIllTypedParams`' double projection rejected specifically as invalid projection. Adding a check to the old abstracted representation alone regresses valid nested declarations into loose-bound-variable errors. |
| `5518bf8`: original inductive/constructor closure (#14607) | Missing before nested elimination. Existing `checkConstructors` checks transformed constructor types, so elimination can hide the original bad term | **Required.** Check original inductive and constructor types for MVars/FVars before rewriting. Use `nestedBadDecl` with the bad nested argument `.fvar l4lBadFVar` and `.mvar l4lBadMVar`, plus a closed nested counterpart. Assert the intended free-variable/metavariable error. |
| `5518bf8`: constructor `_nested` auxiliary reservation (#14616) | Missing. A constructor may name an auxiliary actually generated by another constructor in the same block | **Required constructor scan.** Use `nestedAuxRealDecl`: one constructor nests `List I`, another names the generated `_nested.List_1`. Require the reserved-prefix error and accept an ordinary nested block. A nonexistent auxiliary only tests unknown-constant rejection and does not discriminate the hole. Include the `.proj`-name scan branch separately. |
| `997e298` plus `5518bf8`: normalized Prop recognition, projection/eta restrictions, recursor universes (#14613/#14615) | Missing. Port6 uses syntactic `.prop` equality and `.isZero` in these paths | **Required coordinated fix.** Use `isAlwaysZero` where identifying a proposition permits an operation; use `isNeverZero` where ruling out Prop permits data projection or structure eta. Also update constructor-result, large-eliminator and K-target classification sites. Admit `imaxDataDecl` at `Sort (imax 1 0)`, inspect its recursor to ensure no large-elimination level, then reject `imaxLeakDecl` projecting Bool from a proof. Add valid Bool data projection and ordinary Prop controls. Do not substitute `!isAlwaysZero` for the conservative receiver restriction at uncertain universes. |
| `5518bf8`: mutual-block universe lists and duplicate names (#14608/#14632) | Missing. Port6 checks headers under one `M.run` with the first member's level parameters and inserts all names only afterward | **Required small guards.** Test `L4LMutA [u]` and `L4LMutB []` with the same `u`-using type/body: reject for unequal universe parameter lists. Test duplicate `L4LMutDup` members `Nat := 0` and `Bool := true`: reject duplicate name before overwriting. Accept two distinct, equally parameterized members. Existing per-header duplicate-universe checks do not enforce either block invariant. |
| `5518bf8`: four quotient name checks | Missing. `addQuot` can reach direct insertion with `quotInit = false` | **Required small guards.** With a valid Eq environment before quotient initialization, preoccupy each of `Quot`, `Quot.mk`, `Quot.lift`, `Quot.ind` independently and require duplicate-name rejection without replacing it. Accept a clean initialization and the already-initialized idempotent call. `checkEqType` does not establish these fresh-name conditions. |
| `cbb70bc`: reject non-safe primitive definitions | Missing from port6's `checkPrimitiveDef` | **Required safety guard/test.** Clone the genuine Nat.add `DefinitionVal` with `.partial`; it must not be recognized as a logical primitive. Retain a genuine safe positive. For this older API `.ok false` from the recognizer is appropriate; whole-declaration admission must then reject the reserved primitive name. Do not require the current refactored recognizer's precise error protocol. |
| `80d3dbe`: primitive checking after body typing and strengthened reflection/recursion checks | The large later refactor is not present; port6 still invokes the recognizer while evaluating `checkConstantVal`'s primitive argument, before body admission | **Review/test obligation, not a blind 649-line backport.** The current ordering explicitly ensures defeq consumers see already typed terms, and the new reflection check validates proposition/boolean/evidence argument types together. Build a minimal equivalent admission-order fix or justify the older preconditions; test wrong primitive type/body, ill-typed reflection evidence and altered Nat.mod/div/gcd/bitwise equations alongside their genuine definitions. No concrete false-acceptance exploit for this older recognizer is claimed from source inspection alone. |
| `4172c0f`: level canonicity, subsumption and per-sublevel comparison | Still missing after `bc9a736`. Port6's `NormLevel.le` requires one right node to dominate a bundled constant and variables | **Known reachable incompleteness; fix or report unsupported input.** Test `imax 2 v <= max 2 v` and the valid declaration `inductive Foo.{v} (b : Sort v) : Sort (max 2 v) | mk : (Type → b) → Foo b`. Upstream documents rejection by the old algorithm. Include `imax u (max u v)` versus `max u v` equality and bounded independent semantic level comparisons. This is not evidence of a False-acceptance exploit, and does not invalidate the completed fixed-model replays. |
| `29d9243`, later level fast paths/reification changes | Different revision-specific algorithms and Std APIs | Do not replace the 4.24 algorithm with current Std operations merely because their names match. Preserve/check the corrected port semantics, document incomplete cases, and separately review any later algorithm transplant and its soundness assumptions. Later successful proof builds do not automatically prove the old code. |
| `d41b637`: remove `reduceNat`'s `hasFVar` filter | Port6 retains the filter; upstream says native Lean removed it in 4.19 | Compatibility/reduction control rather than a demonstrated soundness fix. Test a local variable erased by a reducible lambda before primitive arithmetic; do not relax arithmetic checks to obtain a positive result. False rejection or fuel exhaustion remains incomplete, never success. |
| `5b1385a`: replay Eq before Quot | Explicit Eq replay is absent in port6; existing successful runs happened to find a workable dependency order | Reviewed small ordering repair recommended for deterministic replay. Build a fresh inventory that reaches quotient information before Eq and require actual Eq replay plus successful quotient initialization. Do not skip `checkEqType` or synthesize quotient initialization metadata. |
| `4b60e53`: move K-target computation earlier | Same read-only flag computation on `stats`/`indTypes`; upstream says no observable change | Phase alignment, not an additional demonstrated security patch. The normalized-Prop K-target fix above is material; this later movement alone is not. Retain recursor flag/level controls. |
| `67f63b0`: erase primitive domain metadata for exported text | Addresses lean4export dropping borrow-marker `mdata`; port6 uses actual 4.24 `.olean` data | Later transport compatibility, not required just to replay the measured `.olean` models. If a text-export path is introduced, test both metadata-preserving and metadata-erased genuine primitives and reject a wrong semantic type; do not silently broaden all primitive comparisons. |

### Checks that are not automatically required backports

The `5518bf8` commit deliberately distinguishes reachable bugs from redundant
defenses. The selected older port still needs its own invariant review; the
upstream explanation is not a completed proof of this port.

* **Projection structure-name rechecks during reduction/equality (#14631).**
  Port6's `inferProj` already requires the supplied name to equal the actual
  receiver type's head. Reduction/defeq thereafter compare the projection index.
  Keep that ordering invariant; test a wrong structure name through `checkType`
  (reject), an exact valid projection (accept/reduce), and index `2^32` on a
  one-field structure (remain stuck under whnf and reject typing, never truncate
  to field zero). The source uses Nat indices. Do not demand that arbitrary
  *unchecked* projection terms satisfy a typed-defeq contract. Add the redundant
  name comparison only if a reachable checked path violates this invariant.
* **Scanning an inductive's own type for `_nested` (#14616).** The documented
  exploit rewrites constructor types; auxiliary names are not in scope while
  the block's own types are checked. The constructor scan is mandatory above.
  Reserving the prefix in the own-type path is additional policy/defense; test
  the existing unknown-name failure rather than claiming it closes the same hole.
* **Rechecking restored constructor/recursor types and rule RHSs (#14621), and
  replacing restoration assertions with exceptions (#14632).** Upstream labels
  these redundant after correct nested elimination, and retains total Lean
  accesses rather than C++ release-build out-of-bounds reads. Do not waive them
  because the port has a complete verified eliminator: it does not. First apply
  the actual nested closure/parameter/auxiliary fixes and test valid recursion,
  nested parameters/indices, and candidate-generated recursor shapes. Port6's
  replay already compares postponed constructor/recursor information to the
  independently regenerated information; explicitly mutate a stored constructor
  type and recursor rule/constructor name to test that rejection. Those replay
  comparisons do not themselves prove the generator correct. Any reachable
  assertion/default path or malformed generated rule demands a checked error or
  recheck, not a successful fallback through `Inhabited.default`.
  Current `divergences.md` contains a contradictory later assertion paragraph
  claiming restored-type rechecks are retained; the actual `Add.lean` tail
  checks only the discarded auxiliary applications. This disposition follows
  the code rather than treating that sentence as an implemented guard.
* **Dynamic fuel and native recursion caps (#13956, `8865b15`).** The old code
  already uses fixed explicit fuel; the later configurable API is not a required
  soundness repair. Run a shallow accepted/deep refused control under the actual
  selected bounds and enforce process deadlines. A fuel refusal establishes
  incompleteness, not rejection of a logical counterexample.

### Suggested execution order for the coordinator

First run the raw-declaration controls for dropped nested parameters, original
FVar/MVar closure, the generated `_nested` auxiliary, normalized-Prop projection
and recursor levels, mutual headers/names, quotient collisions, and partial
primitives. For each, retain a closed valid counterpart and assert the intended
rejection reason. Establish that the unsafe original form was actually reached:
an unrelated missing-constant or loader error cannot discharge the test.

Then rerun both exact fresh model closures after the chosen repairs and run
the constructor/recursor mutation controls. Finally extend the level and
primitive boundary controls where a general checker claim is intended. Keep
source patch digests, actual command logs and target inventories tied to each
candidate. The port6 successes remain useful evidence for that exact candidate;
none of the newly proposed controls is reported as passing until executed.
