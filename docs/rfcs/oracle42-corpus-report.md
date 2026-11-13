# Deterministic arithmetic oracle corpus (#272)

Public claim #272 and frozen contract `cae1bc5` preceded source writes. This lane
owns only `src/bin/arithmetic_oracle42/corpus.rs`, `tests/oracle42_corpus.rs`, and
this report. Shared DTO implementation `95dc143` was copied here as `31dd24c`;
that dependency is runner-owned work, not a corpus implementation commit.

The corpus implements the exact four-kind boundary tables, CI rank selection,
16 mandatory templates, SplitMix64 draw order, and 15 ordered static negatives.
Full mode selects exactly 100,000 programs; CI selects 128 programs from those
same ranks. Literal argument pairs provide injectivity independently of case
names, comments, expression results or hashes. Four immutable cached boundary
arrays avoid reconstructing the same tables for every generated case.

Rendering checks postfix shape, aggregate 63-node/depth-8 quotas, input ranges,
input/local caps, and a conservative source-size envelope before constructing
source. It prints real typed `f`/`main` functions, strict copy locals, and the
tagged arithmetic-failure handler. `validate_rendered` independently normalizes
the actual parsed AST to the DTO and checks declarations, statement order,
operators, intrinsic/callee identity, argument conventions, supplied runtime
input literals, and the entire fixed handler/tuple wrapper. It rejects extra
functions/statements, generic or modified calls, altered tags and missing
locals. It does not use renderer text equality as its source-binding check.
Only the two specified parser-rejected negatives are exceptions at the worker's
actual parse-failure branch; the validator itself has no intent-based bypass.

Canonical semantic fingerprints implement the contract's exact tagged binary
encoding, excluding case ID and intent. Shrinking returns at most 64 unique,
deterministically ordered suggestions, preserving the case association and
structural quotas. Suggestions strictly decrease node count, local count, or
bounded literal/input magnitude. Subtree replacement preserves postfix shape;
unused-local deletion adjusts subsequent local indices. Suggestions are not
claimed to retain a mismatch until the runner executes and independently
compares them again. No arithmetic expression is evaluated in this module.

Ten authored tests cover:

- All 100,000 generated argument pairs and fingerprints for uniqueness, kind
  counts and actual generated profile bounds. This is generation coverage,
  not 100,000 parser/checker/evaluator/JIT executions.
- The exact CI/full rank relationship at three seeds, actual parser/checker
  acceptance and independent AST binding, and complete boundary-table order.
- A fixed nonmandatory postfix fixture pinning tape draw consumption order.
- All 15 isolated parser/checker single-code negative fixtures.
- Independently assembled canonical fingerprint bytes and case-ID exclusion.
- Parsed-AST/source tampering, wrapper argument order, tags, declarations,
  strict local removal/order, extra functions and changed operators.
- Malformed postfix/profile refusal and deterministic bounded shrink suggestions
  re-rendered, parsed, rebound and checked as actual source.

Writer checks executed: rustfmt with `skip_children=true` while sibling module
files are absent, and `git diff --check`, both pass. No Cargo build or test run
has been executed by this writer.

The coordinator executed the actual `oracle42_corpus` Cargo test target at
integration head `4b9d167`. All 11 tests passed, with zero failures or ignored
tests, in an observed 0.88 seconds. Ten are the corpus-owned tests above; one is
the shared process module's private artifact-quota test included by the test
module import. The execution log is `/tmp/oracle42-root-corpus-runner.log`.
This run actually generated and checked uniqueness of all 100,000 typed
argument pairs and fingerprints, parsed/checked/bound all 128 CI sources at
three seeds, and exercised the 15 isolated diagnostics plus shrink, hash and
AST-tampering checks. The 100,000-case test is generation coverage, not 100,000
runtime oracle comparisons. Strict Clippy, the complete eight-setting runtime
oracle matrix and the full 100,000-program execution remain pending evidence.
The same log's separate runner test target had an unrelated fixture-size
assertion failure under repair by its owner; it is not reported as a passing
combined runner suite.

Earlier authorized feasibility probes used the existing ready `vesper` binary,
not this new generator: 32 distinct hand-rendered valid programs passed 160 CLI
commands (checking plus evaluator/JIT with and without optimization), with
tagged tuples, wide unsigned values, signed endpoints and copy locals. A later
16-source negative probe established the 15 frozen single-code shapes and
excluded unknown-callee calls, which produce two Rust diagnostics. These probes
do not establish the new eight-setting oracle matrix or a completed full run.

Owned source/test checkpoints are `31e3892`, `aa093c1`, `dd49e79`, and style-only
`4604410`. Lean process,
Lean process, worker scheduling, execution counts, actual mismatch shrinking,
and full-run completion remain separate coordinator/runner evidence. There is
no full-parser oracle, semantic-result uniqueness, whole-compiler refinement,
timing extrapolation, source linearity, allocation/RSS or performance claim.
