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
actual build/rejection/coverage report. Until that exists, the unchanged
historical CLI is unsuitable for an external-check acceptance claim. A later
pilot may honestly say that specified Vesper model/artifact declarations were
externally rechecked with enumerated assumptions; a claim of a fully verified
checker requires separate closure of the implementation proof holes and its
remaining trust assumptions.
