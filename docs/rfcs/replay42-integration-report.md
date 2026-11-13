# Independent mathematical replay of arithmetic witnesses (§42)

The existing Rust-checked arithmetic witness foundation now has an optional
structured Lean exporter and a fixed mathematical model. Callers can obtain a
genuine producer certificate, inspect its shared syntax tree, serialize it once,
then compile and independently kernel-audit the resulting artifact. The default
optimizer does not invoke Lean or change its replacement behavior.

## Coordination and scope

Parent #239 and child claims #240–#242 preceded source writers. The coordinator
checked open issues and actual external PR file lists before reserving disjoint
paths. The reviewed contract was pushed at `63d60cd` and linked from all claims
before implementation. Separate worktrees covered the static model, structured
exporter and independent acceptance. Additional read-only reviews compared the
model with actual Rust arithmetic, checked every Lean lowering signature and
guard orientation, and inspected allocation, hashing and printer accounting.

The foundation is PR #238, final fixture-corrected head `c5bc146`, on main
`49a69e4`. Its protected CI/merge status is independent; the owner's subsequent
eBPF #163 merge is on main `cda092f` and has no overlapping implementation paths.
The primary main checkout was advanced to that owner merge. No existing witness
checker or producer algorithm changes here. Root changes only module
registration, the new kernel target's 900-second Bazel timeout on every platform,
and the explicit general Lean-model CI list. No CLI/main/native, core AST or
numeric, Cargo/dependency, formatter, allocator, IR or workflow edits.

## Model, lowering and actual gate

The fixed Lean 4.24 model imports Std. Four nominal kind-indexed Value types
retain signed/unsigned separation even when widths coincide. Constants require
explicit native-range proofs; exact decoding and roundtrip theorems connect
their bit patterns to integers. Checked operations use mathematical integer
arithmetic and range guards; wrapping operations use modular bit vectors. Strict
semantic functions return a value or one empty abortive failure.

All 24 rule lemmas match the Rust catalog. The six erasing/canceling rules keep
their exact totality premises. Wrapping commutation/association admit partial
operands under the one-failure model. Six fold lemmas require closed value-level
equalities; generated `by decide` checks those equalities and fitting bounds,
never equality between functions. All 33 catalog/support roots are explicit
audit targets, together with the generated final theorem.

`export_rewrite` performs the independent Rust source/candidate/type gate first.
`prepare_rewrite` calls the actual private producer and runs its full gate using
the same cumulative witness budget before a separate bounded export phase.
Barriers and unchanged results produce no artifact; errors return fresh failure.
Both APIs are optional and spawn no processes.

Every term becomes a named semantic definition at its own declared kind. Every
proof record becomes a theorem referencing earlier definitions/proofs; unused
records remain included and checked. Guard evidence uses a leaf's totality or an
earlier equality to such a leaf, with explicit symmetry when necessary. Source
identifiers never enter generated Lean names. All generated syntax uses
`vesper-lean-syntax`; no raw source, generated tactics or reparsing is involved.

An allocation-free planning pass bounds declarations, structured nodes/depth,
name/numeral bytes and the closed printer subset before AST construction.
Cumulative logical metadata/work includes retained certificate capacities,
bounded binding transcripts and hashes, requested AST storage, and conservative
printer temporary/copy/output-growth envelopes. The prepared wrapper retains
the move-only budget; consuming `render` invokes the central serializer once.
These are logical data/request bounds, not whole-process RSS or physical-OOM
guarantees. Canonical domain-separated digests associate the actual certificate,
source/candidate/relevant typing, bundled model and returned source bytes.

## Review and executed validation

Independent semantic reviews found no model/catalog, kind, parameter-order,
guard-orientation or gate-budget discrepancy. Resource review found a depth
metric that omitted name/argument structural layers. The corrected accounting
includes those layers, namespace/module levels and imports; an independent
public-AST walker checks exact node/depth counts and limit boundaries. Earlier review also
removed a duplicate charge for the prepaid final source hash.

Executed coordinator checks, Linux Rust 1.99.1, one Cargo build job, debug0 and
incremental0, with real pinned Lean 4.24:

- Full package regression on `94b704f`: **884 tests pass** across 74 target
  summaries, zero failures/ignored. This includes the original complete kernel
  matrix (105.72s) and existing optimizer, arithmetic, effect, ownership,
  evaluator/JIT/object, source macro, editor, formatting and audit regressions.
- After the structural-depth correction and additional missing-object check,
  final affected targets on `ed70d8d`: **11 pass**, zero failures/ignored. These
  are ten public exporter tests and the complete serial kernel matrix (105.61s).
  The ten exporter cases cover genuine producer preparation,
  independent digest encodings, malformed binding refusal, exact limit
  boundaries, independently walked structural depth/nodes, shared references
  and fresh-output failure. These counts are separate reruns, not additive.
- Strict all-target package Clippy, workspace formatting, Bazel metadata and the
  recorded 42-section specification inventory pass.
- The actual general Lean-model CI script passes with the new model and its
  33 printed axiom closures. The model lane additionally executed 40 closed
  numerical/partial-failure probes and three expected negative probes.
- Final acceptance executes **43 real Lean compilations** and **nine actual-tool
  audit calls**, plus explicit missing/wrong-tool and forbidden-option failures.
  It checks all 96 rule/kind mappings and 24 genuine producer fold programs,
  identities, earlier totality, signed endpoints and original callee spans.
  The 34-root restricted-policy audit and empty-policy rejection pass. Wrong
  output/kind/reference/guard ASTs fail actual Lean; corrupted and missing
  objects fail actual audit. The deliberate choice-axiom fixture fails the
  rewrite policy and passes only its separately broadened hostile-fixture policy.
  All accepted rewrite audits report real kernel replay and no cache eligibility.
  Two earlier test-fixture issues were corrected: unsupported source identifiers
  and Lean's tagged diagnostic spelling.

Logs are `/tmp/replay42-root-{export,clippy,models,regression}.log` and final
`/tmp/replay42-root-final-{focused,clippy}.log`; initial `kernel`/`kernel-fixed`
logs retain intermediate fixture failures. Lane reports distinguish authoring checks from
coordinator executions; their counts are not added together.

## Trust boundary and remaining work

Acceptance establishes equality in the bundled arithmetic model under the
explicit `{Quot.sound, propext}` policy. The actual kernel must reject wrong
values, kinds, references and guard evidence. Artifact corruption/missing files
and forbidden-axiom policy must fail independently. Missing/wrong Lean is a test
failure; preparation and digests never confer kernel acceptance or reusable
cache eligibility.

This does not prove Rust admission, AST-to-model translation, the checker,
producer, runtime or machine backend refine the model. It does not implement
PlaceIR/effect certificates, a full executable Lean differential oracle,
verified code generation, self-hosting, DDC or authenticated bootstrap. Claims
#239–#242 remain reserved through reviewed protected merge.
