# Checked AST arithmetic rewrite witnesses (claims 230, 232–235)

The default AST optimizer now installs integer-expression replacements only
after independently checking an exact source/candidate equality certificate.
The same gate covers the six direct constant folds and candidates selected by
the actual 18-rule egg search. Unsupported barriers permit ordinary traversal;
refused admitted islands retain their complete original AST and spans.

## Coordination and final base

Public parent #230 and child claims #232–#235 preceded source writers. Reviewed
contracts were pushed before implementation and amended before dependent policy
changes. Four exclusive worktrees covered checker, actual producer, adversarial
tests and runtime/CLI acceptance. Two additional read-only reviews covered
checker/resource binding and producer/integration behavior. All compiler builds
were executed by the coordinator in one stable target cache; writers clearly
distinguished source authoring from runtime evidence.

The supplied design remains at the recorded §42 revision modified
`2026-11-13T04:46:25.948Z`; fresh metadata was checked during this work. Snapshot
#228 and region collections #229 are merged. Final source was tested after
importing main `49a69e4`, preserving the owner's formatter #236, Chrono #139,
Kinetic #141, Migrate #140 and earlier changes. Integrated tree `dd101bb` adds
those owner changes to implementation head `2260c6a`; subsequent commits add
reports only.

The diff changes `src/optimize.rs`, three new optimizer modules, dedicated new
tests/script and these reports/contracts. Existing compiler core/numeric, main,
native, shared IR/effect optimizer, Cargo/dependencies, BUILD/workflows,
allocator, formatter and existing fixture files are unchanged by this work.
External integration #136 and formatting #231 scopes remain separate.

## Actual gate and producer

The checker accepts topological typed term/equality DAGs for closed u8/u32/u64/
i64 islands. It validates every record, including unused ones, against a closed
24-rule catalog. Equality reasons are shallow reflexivity, explicit rule and
substitution, ordered congruence, symmetry and transitivity. Guarded erasure
requires the actual erased term to be a total leaf or an earlier checked
equality to one; e-class membership never supplies proof.

It independently binds both endpoints to actual AST occurrences, source-derived
name identities, checked types and original intrinsic callee templates.
Contextual IntLiteral metadata does not supply literal values. Every candidate
value uses the original root span; callees preserve their admitted original
span/name. This matters because downstream typing uses spans. Retained caller
String/Vec capacities are accounted along with expanded AST shape and scratch.

The producer first builds explicit bottom-up fold/congruence proofs, then
instruments the actual egg appliers and bounded matching. Explanations and
recursive proof flattening stay disabled. Source variables become indexed Math
inputs instead of arbitrary globally interned symbols. Recorded rule instances
and bounded proof-bearing forest/congruence paths justify the extracted term.
High-u64 literal-range ineligibility has an explicit fully witnessed fold-only
profile; it is not fallback after failed search.

Only saturation or an explicit witnessed-search completion at an actual size-one
Num/Input root finishes search normally. The latter attains the global AstSize
lower bound and avoids exploring cyclic annihilator classes after reaching the
objective. It supplies no semantic authority: sticky failures remain failures,
and the full recorded proof still passes the independent checker.

One move-only budget covers admission, prefix, actual search/capture, proof
construction, candidate materialization, prepaid boxed handoff and final
verification. Geometric raw-node credits include cumulative cleanup allowances;
failed match assignments are prepaid before the successful-match cap. Limits
are logical data/work/request accounting, with explicit ordinary physical-OOM
boundaries. The 2048-node/eight-iteration private ceilings are not promises that
all inputs below them fit the combined budgets. Unsupported or exhausted cases
retain the original admitted island, with no child retry after refusal.

## Independent review outcomes

The checker review found two concrete gaps before release: candidate child
spans could alias a different integer kind, and expected shape accounting did
not cover actual retained candidate capacities. Both were fixed after published
policy amendments and exercised through the real installation seam. Focused
rereview found both closed. The producer review confirmed that no class/union
membership bypasses the gate; subsequent review covered minimum-root markers,
sticky errors, explicit boxed handoff and prepaid growth.

A source ownership audit confirmed that admitted integer names are Copy in the
actual checker/runtime. Sink exhaustion additionally requires a Linear type;
moving/destructuring an aggregate is checked before creating Copy integer
locals. Those transfer/field/binding operations remain rewrite barriers. The
existing `k_i ⊑ m_i` field restriction is unchanged. This is source evidence,
not a new general ownership/refinement theorem.

## Final executed validation

Coordinator Linux Rust 1.99.1, one build job, debug0/incremental0, pinned Lean
4.24.0 where existing tests invoke it:

- `cargo test --locked -p vesper --no-fail-fast`: **874 passed**, zero failed or
  ignored, across 72 target summaries on the combined main/implementation tree.
  Existing arithmetic, evaluator/JIT/object, ownership, source macros, editor,
  formatting, kernel-audit and source-session regressions remain enabled.
- Focused actual gate/checker/producer/runtime/CLI plus existing arithmetic:
  **129 passed** across six targets, zero failed or ignored. Includes 13 producer
  unit tests, 12 checker cases covering 96 rule/kind combinations, three local
  builder/budget cases, 11 private genuine-proposal attacks, 15 independent
  public attacks, eight runtime tests, five real CLI tests and 18 existing
  checked-arithmetic tests.
- Strict all-target package Clippy with `-D warnings`: passed without new lint
  suppressions. Workspace formatting, whitespace, Bazel generated-metadata and
  recorded specification-inventory checks pass.
- Independent actual binary oracle: **340 commands over 49 source programs**,
  default optimization versus `--no-opt`, evaluator/JIT/HIR and deterministic
  outputs. Covers four kinds, six folds, overflow/high unsigned products,
  totality, intrinsic shadowing/original callee spans, child barriers and whole
  depth-refused island retention. All pass; input files remain unchanged.

Logs: `/tmp/rewrite42-integrated-{tests,clippy,python,fmt,bazel}.log` with aggregate
exit0; focused logs use `/tmp/rewrite42-{critical,clippy,python}-fixed.log`.
Lane reports preserve their intermediate execution heads and authoring limits;
their totals are not added to these combined counts. No new Miri or sanitizer
run is claimed. Hosted/platform checks are separate and required before merge.

## Remaining design scope

This is a bounded Rust-checked AST arithmetic witness foundation. PlaceIR/effect
certificates, SIMD/FIP/handler rules, Lean refinement of this Rust checker,
verified direct machine-code emission, the executable Lean differential oracle,
self-hosting, DDC and authenticated builder/TEE gates remain future work.
The trusted already-checked AST/TypeMap association and current arithmetic
semantics are explicit inputs. No persistent certificate wire format, complete
compiler proof, performance, whole-process RSS or bootstrap claim is introduced.

Claims 230 and 232–235 stay reserved until reviewed protected merge after the
required checks pass.
