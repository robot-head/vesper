# Lean4Lean 4.24 tool compatibility plan (#297)

This is a source-reviewed feasibility plan, not a working installation or an
external-checking result. The public #297 claim precedes this document. The only
owned repository change is this new file. This planning lane performed no
Lean4Lean build, dependency install, port, model execution, Cargo command or
source modification. Later coordinator experiments are attributed separately
below; they do not establish a working candidate.

## Decision and exact candidate

Keep Vesper's `leanprover/lean4:v4.24.0`. No compatible unmodified Lean4Lean pin
has been established. Recommend an explicitly reviewed 4.24 port based on
**digama0/lean4lean `4faf51403bc4cb35cf6666d91fc9c938ae46d3aa`**, Git tree
`6285b8511a8efa11a093aecd3990a3fd4ec83d84`. This is the direct parent of the
4.26 toolchain bump, and still pins 4.23. It contains substantial verification
work after the earlier `93abda080b30139fa84ae92a16367cfeff5639f5` toolchain bump;
choosing the earlier bump would discard that work without improving version
compatibility.

This choice is a port starting point, not an assertion that it builds or checks
our models. The source must receive the blocking fixes below before a successful
run can become external-check evidence. Store the resulting reviewed patch
stack and its exact identities alongside the upstream pin. An unchanged old
CLI, a toolchain-file-only rewrite, or a foreign-version binary is insufficient.

Source inspection found a useful compatibility fact: GitHub's `contents/src/kernel`
listings at Lean tags v4.23.0 and v4.24.0 have the same 40 entry names and Git
object SHAs. Direct reads also confirmed identical `environment.cpp`,
`type_checker.cpp`, `inductive.cpp`, `quot.cpp` and `expr_eq_fn.cpp` contents.
This narrows the kernel-version port, while Lean APIs, library proofs and
serialized artifact compatibility still require actual validation. It does not
establish absence of later-discovered kernel bugs.

Use **Batteries `8da40b72fece29b7d3fe3d768bac4c8910ce9bee`** as the proposed
4.24 dependency. GitHub's v4.24.0 tag resolves directly to that commit; its
`lean-toolchain` is 4.24.0 and its manifest has no further packages. The old
Lean4Lean manifest instead pins Batteries
`d117e2c28cba42e974bc22568ac999492a34e812` / v4.23.0. Both lakefile and manifest
must express the chosen exact dependency consistently; no floating tag, silent
`lake update`, inherited cache, or dependency substitution is evidence of the
reviewed port.

### Observed upstream versions

| Reference | Declared Lean | Use here |
|---|---|---|
| `93abda080b30139fa84ae92a16367cfeff5639f5` | 4.23.0 | Historical toolchain bump, not the selected base |
| `4faf51403bc4cb35cf6666d91fc9c938ae46d3aa` | 4.23.0 | Selected port starting point |
| `6bca7f6fd61bcea09266303e75c437ba9745a32b` | 4.26.0 | Next default-branch toolchain bump; source of later compatibility changes |
| `5518bf83860bab48e8d53f1f447cdd6c50c30c3f` | 4.33.0-rc2 | Fixed immutable later reference for CLI/replay behavior |
| observed master `67f63b0c818920dfaed2ad07ccff187a6a8fe1e3` | later branch state | Context only; never a floating build input |

The default-branch toolchain-file history examined has no 4.24 bump. This is not
an exhaustive claim about every upstream branch or unpublished port.

## Blocking source changes and review obligations

1. **Import the actual target in fresh replay.** In the selected base,
   `Main.lean:285`–289 calls `Lean.withImportModules #[]` and then replays that
   empty imported environment. The `module` argument is used only for the new
   environment's label. This cannot implement the README's closure-checking
   claim. Later fix `c55ae0d09832b5ac7d6567372ee324bf2ff9dd70` changes the import
   to `#[module]` and handles the quotient-initialization policy for fresh replay.
   Port the applicable semantics and test the actual target's participation;
   a nonzero generic declaration count alone does not bind a check to the target.
2. **Restore declaration-value free-variable/metavariable guards.**
   `Lean4Lean/Environment.lean:42`–71 omits `checkNoMVarNoFVar` on safe-definition,
   theorem and opaque values. Upstream `779c51fdea49c57c60f8039703605faae126b17f`
   explicitly documents false-proof acceptance through a stale inference-cache
   free variable and restores all three checks. These are required corrections,
   not optional defensive differences. Run the corresponding concrete rejection
   regressions against the port, including each of the three declaration forms.
3. **Review reachable later kernel fixes.** The current upstream regression
   modules `Lean4Lean.Tests.DeclFVar`, `NestedInductive` and `KernelHardening`
   provide concrete inputs, including nested inductive/projection/recursor and
   duplicate mutual-name cases. The proof-scope lane is reviewing their exact
   applicability. Record a per-case disposition before accepting a historical
   base. Do not assume identical 4.23/4.24 kernels or a successful positive model
   check makes those later counterexamples irrelevant.
4. **Keep 4.24 import and primitive APIs.** Installed Lean 4.24 source still has
   `ImportedModule.mainModule?`, two-argument `subsumesInfo`, `readModuleData`,
   and `String.mk`, resembling the selected base's API generation. Presence of
   those names does not establish source-level compatibility: private visibility,
   aliases and field notation must be checked by actual compilation. The first
   coordinator feasibility build failed on precisely this boundary, described
   below. The 4.26 bump changes these and string-primitive handling; copying that
   bump wholesale would introduce later-version APIs. Port against actual 4.24
   declarations and record each required adaptation.
5. **Preserve proof/opaque-body dependency traversal for this version.** In
   4.24, `ConstantInfo.value?` includes theorem bodies by default and hides only
   opaque bodies. Base `Main.lean:35`–39 already handles the opaque fallback.
   Current upstream's `value? (allowOpaque := true)` change addresses a later
   API state; it is not itself evidence that the 4.24 base omits theorem proofs.
   Independently exercise theorem and opaque dependencies in the port.
6. **Replay quotient prerequisites and manage compacted-region lifetimes.**
   Current replay explicitly schedules `Eq` before `quotDecl` (upstream
   `5b1385af027cb867f15f218165eec44213d5c6a2`). The old import-only path manually
   frees regions while its module data remain local (`Main.lean:267`–283);
   upstream `9fc9c838a9656bf420bfc9f94a8f64330502bc95` repairs a resulting
   segmentation fault. Prefer a narrowly exposed fresh-only gate using the
   callback lifetime of `withImportModules`. If retaining/importing other replay
   paths, port and test their actual lifetime fixes against the 4.24 loader;
   don't copy later multi-part-module APIs without checking their availability.

The future reviewed contract should enumerate the exact owned port paths and
patch order before source writers begin. A plausible minimal package is a
pinned upstream base plus exact Batteries/Lean metadata, required checker
corrections, and a small static strict fresh-only entry point. Completing all
upstream correctness proofs is a separate undertaking.

## Actual CLI behavior and target scope

The selected source and README differ in details that matter to integration:

| Source behavior | Integration requirement |
|---|---|
| `lake build lean4lean` builds the executable without proofs; `lake build Lean4Lean` likewise omits proofs | Record these as implementation builds only. Build/audit Theory and Verify separately when reporting proof scope. |
| `main` partitions all `-`-prefixed arguments; unknown flags are silently ignored | Use a strict wrapper/fixed argument vector. Reject unknown/duplicate options before spawning. |
| No target uses a lake manifest's capitalized name, or anonymous name when absent | Always provide an exact fixed model target. Vesper has no root Lake package to infer here. |
| Explicit targets are namespace prefixes matched by scanning the complete search path | A wrapper should select exact allowlisted targets; do not let a same-prefix child module change the claimed target. |
| `--fresh` requires exactly one discovered module | Run each fixed model explicitly, or use a reviewed exact-module adapter; never interpret a discovery failure as success. |
| Ordinary mode imports dependencies as trusted environment data and rechecks target additions only | Require corrected fresh replay for the target-plus-imports claim. Record import-only checks distinctly if exposed at all. |
| `replay` skips `ci.isUnsafe` and `ci.isPartial` | Report eligible/omitted declaration inventories. Checking ArithmeticOracle does not validate execution of the server or every unsafe/partial runtime implementation. |
| `--compare` invokes Lean's C++ kernel for timing comparison on slow declarations | Disable in the gate, keeping external checking and ordinary Lean replay distinct. |
| Completion prints `checked N declarations`; errors produce nonzero status | Bind the count and exit to the requested target, require positive target participation, and fail on missing/extra/malformed completion. Human output is not an authenticated proof certificate. |

The base public replay functions take declaration data from `.olean` files and
call `Lean4Lean.addDecl ... true`. The checker accepts well-formed axiom
*declarations*, so external acceptance is not an axiom-policy verdict. Retain
Vesper's separate `sorryAx`/project-axiom hygiene and provenance checks. No new
cache/publish trust approval should follow automatically from this tool.

### Known closure-checking feasibility risk

At observed HEAD, upstream's actual
[CI](https://github.com/digama0/lean4lean/blob/67f63b0c818920dfaed2ad07ccff187a6a8fe1e3/.github/workflows/ci.yml)
replays only `Init.Core` in ordinary mode and `Init.System.IO` in fresh mode.
Its comment says modules importing `Lean` currently fail with “type checker does
not support loose bound variables” and links issue17. It also warns that the
CLI's unbounded per-module tasks can exhaust memory for broad prefixes. This is
source-reported upstream behavior, not an executed finding for the proposed
4.24 port.

`ArithmeticOracle` imports `Lean.Data.Json`, so a positive core or `Std`-only
pilot cannot establish that its required closure works. Actual fresh replay of
both `RewriteWitness` and `ArithmeticOracle` is a feasibility gate. If the latter
fails, report unsupported/failure; do not silently switch to trusted-import mode,
drop its Lean imports, call the core-library pilot a full model check, or claim
that a current upstream build alone solves compatibility. Keep resolving this
blocker distinct from merely fixing the historical missing-target bug.

## Search path, sysroot and reproducible setup

The selected CLI begins with `initSearchPath (← findSysroot)`. Installed 4.24
`Lean/Util/Path.lean` shows the actual environment contract:

- `findSysroot` honors `LEAN_SYSROOT`; otherwise it runs `lean --print-prefix`
  using PATH. A binary's location does not by itself choose its Lean version.
- `initSearchPath` prepends parsed `LEAN_PATH` to the builtin sysroot library
  path. An ambient same-named module can substitute unless the caller controls
  this environment and checks artifact identities.
- `findOLean` resolves the first matching artifact. Loading `.olean` data is
  version-sensitive and uses Lean's runtime/loader representation. No source
  inspection here establishes that a 4.23, 4.26 or 4.33 executable may safely
  consume 4.24 files.

The README recommends `lake env` for executable search/library environment.
Vesper's root is not an equivalent Lake project; future setup must measure the
actual built executable's requirements on each supported platform. Prefer an
absolute executable path, the exact 4.24 sysroot and an explicit private model
artifact directory. Allow only the required compiler/tool/dependency library
paths, and exclude ambient project/package paths. A controlled `lake env` may
supply measured native-library settings, but must not reintroduce unrestricted
artifact resolution. Set a known cwd; do not infer a target from its manifest.

Recommended future setup/acceptance sequence, **not executed in this phase**:

1. Obtain the exact upstream commit and Batteries commit in fresh private build
   storage. Verify source trees/lock metadata, apply the reviewed patch stack
   with no fuzz, and hash the resulting sources. Never build from a dirty or
   moving checkout or accept a downloaded opaque binary as the reviewed tool.
2. Use exactly Lean 4.24.0 / commit
   `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, recording the actual executable,
   compiler/sysroot identity, platform and build commands. The fixed Lake
   dependency graph must be resolved before an offline replay claim; runtime
   checking must not install tools or fetch dependencies implicitly.
3. Build the runtime checker with that toolchain in its own target storage.
   Record source/patch/dependency and resulting executable hashes. Separately
   build requested proof libraries, inventory root-specific transitive axioms,
   and record partial/assumption-bearing proof scope without a self-verified
   label. A successful proof-library build alone does not reject `sorry`.
4. Compile exact bundled `RewriteWitness.lean` and `ArithmeticOracle.lean` into
   a fresh private artifact root with the same pinned compiler. Record their
   source and artifact identities and required imports. Use `lean -o` for the
   oracle; do not execute its long-lived `main` while preparing this check.
5. Run the corrected external checker on each exact allowlisted module, with
   sanitized search paths/sysroot, bounded concurrently drained output,
   measured watchdogs, nonzero/crash/timeout rejection, and child kill/reap.
   Missing/wrong tools and incompatible/missing artifacts fail closed.
6. Execute real positive and malicious-declaration rejection fixtures before
   reporting any external acceptance. Include a deliberately absent/wrong
   target and contaminated search path so a generic successful check cannot
   stand in for checking the requested artifact. Keep a fixed target inventory
   and complete actual declaration counts/skips in the report.

An external replay validates eligible declaration typing in the reviewed Lean
implementation, conditionally on its compiler/runtime/loader and accepted
axioms. It does not validate Rust-to-model refinement, the model's IO behavior,
source resource traces, the native compiler, whole-process memory bounds, DDC
or full §42. Do not label the CLI self-verified merely because its project also
contains verification files. The independent proof-scope plan records the
remaining proof gaps and assumptions.

## Coordinator feasibility update: private API access

After the initial planning commit, the coordinator publicly authorized a
coordinator-only temporary experiment under
[#297 comment6851501308](https://github.com/robot-head/vesper/issues/297#issuecomment-6851501308).
The reference checkout remains clean at the selected 4faf commit. The separate
experiment is `/tmp/vesper-lean4lean42-port`, with Lean 4.24, the proposed
Batteries commit, the three declaration-value guards and fresh target import.
This planning lane did not modify or build that experiment.

The coordinator's actual first build failed in
`Lean4Lean/Environment/Basic.lean:85:21`: field access `mod.mainModule?` could
not resolve `Lean.ImportedModule.mainModule?`. The declaration exists but is
private in 4.24. Adding `mainModule?` to the existing `open private` list alone
still produced the same field-notation failure in the second build. This lane
read the retained logs `/tmp/lean4lean42-feasibility-build.log` and
`/tmp/lean4lean42-feasibility-build2.log`; both record Lean exit1 and a failed
`Lean4Lean.Environment.Basic` target. These are actual compatibility failures,
not merely predicted risks.

The coordinator reported a third experiment using the opened private alias
through explicit `mainModule? mod` application instead of field notation. Its
outcome is not established by this update. Do not infer build success, a complete
compatibility patch or external model acceptance from the proposed correction.
Further private-API, proof or runtime adaptations may remain. Identical kernel
files and familiar declaration signatures cannot replace compilation and the
required rejection/coverage checks.

## Evidence and limits

Read-only source evidence was gathered through GitHub repository/file/commit
GETs and the coordinator-provided clean reference checkout
`/tmp/vesper-lean4lean42-upstream`. Its exact HEAD/tree were inspected with
`git rev-parse HEAD`, `git log -1` and `git status --short`; source paths were
read using `rg`, `nl` and `sed`. The upstream checkout was not edited or built.

Immutable references:

- [Selected base](https://github.com/digama0/lean4lean/tree/4faf51403bc4cb35cf6666d91fc9c938ae46d3aa), including [Main](https://github.com/digama0/lean4lean/blob/4faf51403bc4cb35cf6666d91fc9c938ae46d3aa/Main.lean), [Environment](https://github.com/digama0/lean4lean/blob/4faf51403bc4cb35cf6666d91fc9c938ae46d3aa/Lean4Lean/Environment.lean), [README](https://github.com/digama0/lean4lean/blob/4faf51403bc4cb35cf6666d91fc9c938ae46d3aa/README.md), and [manifest](https://github.com/digama0/lean4lean/blob/4faf51403bc4cb35cf6666d91fc9c938ae46d3aa/lake-manifest.json).
- [4.26 bump](https://github.com/digama0/lean4lean/commit/6bca7f6fd61bcea09266303e75c437ba9745a32b), [fresh/import fix](https://github.com/digama0/lean4lean/commit/c55ae0d09832b5ac7d6567372ee324bf2ff9dd70), [value-FVar fixes](https://github.com/digama0/lean4lean/commit/779c51fdea49c57c60f8039703605faae126b17f), and [later replay source](https://github.com/digama0/lean4lean/blob/5518bf83860bab48e8d53f1f447cdd6c50c30c3f/Lean4Lean/Replay.lean).
- [Proposed Batteries](https://github.com/leanprover-community/batteries/tree/8da40b72fece29b7d3fe3d768bac4c8910ce9bee) and [Lean 4.24 kernel](https://github.com/leanprover/lean4/tree/797c613eb9b6d4ec95db23e3e00af9ac6657f24b/src/kernel).

At the initial source review, compatibility/port compilation and external
checks were unexecuted. The separately attributed coordinator update now records
only the observed failed builds; this lane still performed no builds or source
changes. A successful candidate build, external model checks, rejection tests,
elapsed-time measurements and platform behavior have not been established by
this document. It does not claim those gates passed or authorize repository
implementation outside the separately published scope.
