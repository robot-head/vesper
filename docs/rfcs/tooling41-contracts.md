# Source tooling implementation contracts

Baseline: published main `ba0fadedf91be3bfd059362dd4fe14cc2f2dc51a`
([PR #61](https://github.com/robot-head/vesper/pull/61)). The recorded design is
sections 1–41 plus Appendix A. The coordinator checked open issues/PRs and posted
each ownership claim before implementation. These claims are separate from the
external Claude Warp issues #36–#45 and their production PRs #53/#55–#60/#62.

| Issue | Branch | Exclusive implementation |
| --- | --- | --- |
| [#63](https://github.com/robot-head/vesper/issues/63) | `tooling41/grammar` | New `crates/vesper-grammar/**`, optional `lean/GrammarCanonical.lean` |
| [#64](https://github.com/robot-head/vesper/issues/64) | `tooling41/fmt` | New `crates/vesper-fmt/**`, `src/fmt_commands.rs`, `tests/fmt_cli.rs` |
| [#65](https://github.com/robot-head/vesper/issues/65) | `tooling41/lint` | New `crates/vesper-lint/**`, `src/lint_commands.rs`, `tests/lint_cli.rs` |
| [#66](https://github.com/robot-head/vesper/issues/66) | `tooling41/doc` | New `crates/vesper-doc/**`, `src/doc_commands.rs`, `tests/doc_cli.rs` |
| [#67](https://github.com/robot-head/vesper/issues/67) | `tooling41/doctest` | New `src/native_batch.rs`, `src/doctest.rs`, `src/doctest_commands.rs`, matching new integration tests; one-line batch module registration in `src/native.rs` |
| [#68](https://github.com/robot-head/vesper/issues/68) | `tooling41/bench` | New `crates/vesper-bench/**`, optional `lean/MetronomeArithmetic.lean` |
| [#69](https://github.com/robot-head/vesper/issues/69) | `tooling41/integration` | Root package/module/CLI/workflow wiring, new `src/bench_commands.rs`/`tests/bench_cli.rs`, optional shared tooling I/O helper, acceptance workflow/scripts and integration report |

Each lane owns only its matching `docs/parallel-results/tooling41-<lane>.md`.
All unlisted production paths are read-only. Source/core AST, checker, Place IR,
existing native/session implementation, simulation, Forge, Prism, Wave-four
networking and generated Lean extractor remain unchanged. Existing arithmetic
is `vesper.arith.checked/1`, interface semantics v2 and native ABI v2. Preserve
the user-approved `k_i ⊑ m_i` field premise.

## Dependency and source evidence

New leaf crates depend on existing root-free crates and typed DTOs. A facade
adapter may consume checked `World`/IR/interface information above them; it must
not introduce `vesper -> new leaf -> vesper`. Root main/lib registration stays
with the coordinator. Temporary uncommitted wiring for tests is allowed, but a
lane's final commit contains only its exclusive paths. Root changes approved
for other external PRs remain allowed and will be reconciled during integration.

Formatting uses the existing lossless lexer/Rowan CST. It must validate exact
non-trivia token kind/text and comment-byte/order conservation and idempotence;
opaque macro payloads stay verbatim where trivia can affect expansion. A line
comment cannot absorb a following token. Invalid or unsupported cases fail or
preserve the original source, rather than guessing a different program.

Lint diagnostics use actual resolved accesses/call signatures and structural
effect instances, not bare effect-name sets. Unknown indirect calls, open rows,
closures and handlers must be conservative. An offered edit is transactional:
reparse and fully recheck the complete candidate; semver-safe also validates the
supported checked public interface. No error permits a partial file replacement.

Documentation retains complete types, pass/mode bounds, effect instances and
selectors. It excludes generated private trait helpers and distinguishes actual
native layouts from unsupported wire layouts. A reachable terminal graph edge
is not an all-path ownership proof, and a certificate boolean cannot authorize
a verified badge. Static HTML must escape source/doc content.

## Shared JIT and measurement boundary

The doctest owner supplies a reusable owned native batch that compiles a union
of reachable no-argument entries once. Entry code remains private and executable
only while its module lives; reject invalid signatures, foreign imports and
unsupported/unbounded execution. Preserve the existing single-entry APIs.
The owner communicates its concrete API before the coordinator adds the source
benchmark adapter; the benchmark leaf's callback/statistics work is independent.

Positive doctests use the actual parser/checker/PCG and batch JIT. Negative tests
must match the requested diagnostic code, without an unrelated base-package or
parser failure counting as success. Recursion/cyclic or unresolved execution
that cannot be bounded is unsupported, never a silently skipped passing test.
General SimPulse I/O and source arenas are separate integrations.

Measurements always carry their provider and units. Wall-clock samples are not
PMU counts; deterministic offsets are not actual code-layout randomization.
Statistical intervals state their method and assumptions. Empty, invalid or
insufficient samples cannot pass as zero regression. Missing allocation/SIMD/
hardware evidence remains unavailable. Source benchmarks parse/check and compile
before measuring repeated calls, with setup excluded from the intended window.

## Delivery

Use separate worktrees/private targets, one build job, zero dev/test debug info
and disabled incremental caches. Commit only owned source/tests/report files,
then supply SHA, APIs, integration patch, actual commands and limitations.
The coordinator reviews and exercises real positive/negative CLI flows, full
workspace checks, strict Clippy, target/platform evidence and actual pinned Lean
for any new models before protected-branch publication.
