# Unit and tuple Expr macro integration

Claims [#166](https://github.com/robot-head/vesper/issues/166), #167–170 and
#172 were published before their respective writers. The frozen interface and
exclusive paths are in `docs/rfcs/tuple41-contracts.md`. Fresh open issues and
actual external PR filenames were checked first, including eBPF follow-up #165
and Bazel #171. External #158 retains workflows, build files and test wrappers;
this batch changes none of them. Claims remain reserved through reviewed merge.

The batch extends the existing pure Expr profile with unit, grouping, singleton,
multiple and nested tuple values. Syntax, sealed typed shape, all hygiene and
provenance traversals, canonical lowering and actual compiler authority checks
agree on the distinction between `(x)` and `(x,)`. Tuple arity is independent of
macro parameter arity. Existing source parameter identity, slot ownership,
definition-site globals, caller origins and element block scopes remain checked.
No core AST/IR/native ABI, guest record, authority policy or Lean emitter changes
are required.

The first real end-to-end tests exposed the existing tooling guard's blanket
refusal of nonempty tuples. A separately claimed prerequisite (#172) replaces
that refusal with a conservative numeric envelope before ordinary checking.
It bounds retained type size/depth, environment copies, metadata and work without
constructing inferred type trees or raising existing limits. The original
25-step exponential tuple example now receives a Resource refusal before checker
entry. Real bounded programs reach the unchanged checker and retain its exact
linear exhaustion and type diagnostics.

Independent review found three additional resource paths: duplicate declaration
inventories disagreeing with the checker's selected header, malformed nominal
contexts copied by nullary constructors, and short-circuit environment copies.
Each is closed and has an actual-parser regression. Explicit inference holes in
analyzed headers/annotations are conservatively refused; that clarification is
not reported as a demonstrated checker exploit. Unannotated local initializer
inference remains supported. The existing LSP unavailable-warning regression now
uses a still-unsupported generic inference example because its old small tuple
fixture is legitimately admitted; all warning/status/span assertions remain.

## Validation

On the integrated Linux executable and Rust 1.99.1:

- 132 tests across 12 root library, compiler, authority, parameter, tuple,
  preview, database and admission suites pass, with zero failures or ignores.
  This includes all 11 actual tuple lowering/authority tests, six tuple CLI tests
  and 13 bounded-admission integration tests.
- 83 additional tests across seven LSP, navigation, action, recovery, signature
  database and lint CLI suites pass with zero failures or ignores.
- The syntax lane passes 80 tests across eight suites; the pure Forge host lane
  passes 59 across eight suites. Their targeted strict Clippy checks pass.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`, workspace
  formatting and whitespace checks pass on the final integrated source.
- `check-source-macro-tuples.py --binary REAL_VESPER` passes 157 commands,
  21 stdio sessions and 108 JSON-RPC requests, including 23 separate expanded
  programs checked and executed to42, plus one intentional unhandled-entry
  refusal. Accepted preview artifacts/maps/audits match separate real compilation.
  Both nested generated-helper body-drift cases fail the actual authority gate;
  the distinct caller-origin effect case retains its declared effect row.
- The existing parameter acceptance passes 65 commands, nine stdio sessions,
  47 requests and nine explicit expanded-program runs. Existing source macro
  acceptance passes 115 commands and 11 runs; editor acceptance passes 19 child
  sessions; recovery acceptance passes 15 sessions and three batch checks.

The Rust tuple executable tests additionally check and run 20 positive artifacts.
These are overlapping acceptance mechanisms, not a claimed count of distinct
programs. All runs use the actual compiler/executable; no substitute checker,
fabricated checked artifact, guest approval or applied editor edit is used.

## Boundaries and remaining work

The bounded prechecker deliberately refuses unresolved calls, latent/higher-order
refinement, handlers, unsafe generic projections, dynamic indexing and substituted
trait/implicit-self headers when their tuple envelope is uncertain. The normal
original macro source remains a parse error to ordinary semantic providers;
explicit preview does not authorize ordinary hover or diagnostics.

Three compact tuple programs pass actual formatting conservation, reparsing,
idempotence and separate execution. A larger repeated-helper fixture still
truthfully exceeds the existing source-dependent formatter WorkLimit; no cap was
raised. See the acceptance report for that distinction.

This batch supplies new Rust targets `source_macro_tuples`, `typed_tuple`,
`forge_tuples`, `forge_tuples_cli` and `tuple_analysis`, plus the Python harness.
The external Bazel/CI owner has been notified of those targets, the actual-binary
override `VESPER_SOURCE_MACRO_TUPLES_BINARY` and the harness `--binary` option.
Hosted native checks on this published tuple PR remain pending; local Linux
results do not claim macOS or Windows execution. Source Tier2 integration,
additional expression/category grammar and the rest of the design remain work
outside this batch. No timing, RSS or whole-compiler formal proof is claimed.
