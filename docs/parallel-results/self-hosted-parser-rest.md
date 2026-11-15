# Self-hosted parser: traits, models, flat syntax and recovery

Wave nine lane 3, issue #580. Taken over from the unused Claude claim by Codex.
Base: `05bc1a54841b2cc256b6379fce07d82c782f2d98`, a descendant of the required
`37f7b8bcfa4cd409177856c8fe5deeaa7f2a88ff`. Branch:
`codex/issue-580-self-hosted-parser`. No overlapping parser PR was open at
preflight. No subagents were used. Integrated accepted main
`d204923a47675a95fbc74750a0cf054ebed3975e` before final checks.

## Implemented

- Traits, public traits/models, named models, `use model` at module and block
  scope, canonical trait impls and contiguous `impl @scope/package::Trait`
  headers, associated type declarations/bindings, trait bounds and where
  clauses, `T::Assoc` and `<T as Trait>::Assoc` projections.
- JSON `\uXXXX` escapes, including valid surrogate pairs, and explicit
  rejection of malformed, short and unpaired escapes.
- Exact byte comparisons over indexed lexical name chains. Function,
  closure, block, pattern, match-arm and handler scopes distinguish lower-case
  indexing from row instantiation without hashing identifiers or guessing.
- A retained token buffer and a real flat CST: preorder nodes with parent
  links and byte spans, plus every token and its owning node. The package's
  [layout and embedding contract](../../packages/vesper-compiler/src/syntax_layout.md)
  specifies the versioned byte view, limits and consumer API.
- Batch item/statement recovery, seed diagnostic codes/messages/spans,
  lexical error tokens, and message rendering written in Vesper. Both
  multiple-error fixtures and truncated declarations compare the first
  diagnostic with the seed; shared recovery fixtures compare later messages
  and spans too.
- A Vesper region-backed implementation of the linear memory interface,
  checked against the bounded embedding adapter. Bounds and byte-view tests
  exercise the last address and typed rejection of invalid addresses.

The existing corpus target is actually `tests/self_host_front_end.rs` (the
prompt named `tests/self_host.rs`). This change updates that existing target,
adds `tests/self_host_parse.rs`, and shares its memory-only test adapter under
the owned `tests/fixtures/self_host/` path. The scan now includes crate,
bootstrap and editor fixtures, as well as examples, tests and package sources.
Every accepted input compares **all node and token records**, including trivia
ownership, rather than accepting a digest match or an Unsupported result.
Rejected inputs compare seed diagnostic codes, including the macro fixtures.

## Public API and decisions

`lex_main()` and `parse_main()` retain their scalar compatibility signatures.
`parse_transport()` exposes the complete parser through `SyntaxMemory` for an
embedding or checker; `syntax_run(consume)` closes that interface using Vesper
region storage. The callback consumes the tree before its region ends. No
region-generic nominal types, byte-slice language extension, core AST/IR schema
change, backend edit, dependency or unsafe code was required.

The CST mirrors `vesper_syntax::parser::parse`, including its trivia attachment
and empty nodes. It does not insert the editor-only wrappers from
`parse_recovery`. Diagnostics retain the seed's literal-error spans even where
they point after a consumed literal. Limits return typed errors and never
certify incomplete buffers as successful syntax.

The implementation checks Vesper effect rows and source ownership rules. The
linear memory interface must be resumed once per operation; the ordinary
entry discharges it and its region locally. The embedding entry explicitly
requires it. No ambient I/O, zero-global-allocation, formal parser-correctness,
Stage 1 lock, native parser, or end-to-end compiler proof claim is made.

## Checks

Environment: Linux, AMD EPYC 9V74 host, five visible CPUs, four corpus workers,
Rust 1.100 debug test profile, isolated `/workspace/target-issue580`. Build settings:
`CARGO_BUILD_JOBS=2`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`.

- `cargo +1.100.0 test --locked --test self_host_front_end --test self_host_parse -- --nocapture`:
  passed after main integration: **369/369 token streams**, **305 complete
  CSTs**, **64 matching rejections**, **101 JIT lexer runs**. The corpus target
  took **458.66 s**, within its existing 900 s Bazel timeout; the five focused
  tests took 24.34 s. The final grouped-callee follow-up also passes all five
  focused tests (34.86 s), including `(xs)[e]`, nested grouping and tuple versus
  name disambiguation.
- `cargo test --locked --test self_host_parse -- --nocapture`: five tests pass;
  recovery, prefix, region adapter, byte layout and bounds cases are covered.
- `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  passed (Cargo emits an existing unused manifest-dependency warning for
  `vesper-pact`; the Clippy command exits successfully).
- `cargo +1.100.0 clippy --locked --test self_host_front_end --test self_host_parse -- -D warnings`:
  passed; repeated for the final test sources before delivery.
- `cargo +1.100.0 fmt --all -- --check`: passed.
- `vesper fmt --check packages/vesper-compiler/src/*.ves`: passed. Helpers are
  split into canonically formatted files below the formatter's work budget.
- `python3 tools/bazel/sync_build_files.py --check` and `git diff --check`: passed.
- `cargo test --workspace --locked`: ran and stopped at the existing hard
  failure in `lean4lean42_acceptance::genuine_three_model_replay_and_independent_refusal_matrix`:
  `VESPER_LEAN4LEAN42_ROOT` is unset. This test requires a genuinely prepared
  Lean4Lean candidate and deliberately does not skip it. This is not a full
  workspace pass and no proof setup or held proof paths were changed.

## Root patch

Commit `a1e1ebc` adds four `BUILD.bazel` `extra_test_data` file groups for the
crate `.ves` fixtures included by the expanded corpus. The existing doc crate
input was already present. No Cargo manifest or lockfile change by this lane. The patch
preserves the independently held resource-oracle timeout hunk and all existing
Bazel reservations, timeouts and platform policies.

## Remaining backend boundary

Full-parser JIT execution is infeasible with the seed's existing effect
specialization rules. A direct attempt on an empty input spent 111.209 s
before returning `recursive effect specialization exceeds 64 inline frames`.
Passing region state explicitly was also rejected at that guard. The native
session backend separately rejects recursive suspending call cycles before
code generation. Those backends are outside this lane's ownership.

The real structured parser therefore runs on the evaluator. JIT corpus
coverage is deliberately **lexer coverage**, increased from every sixteenth
source to every fourth source plus every large source; it is not reported as
parser JIT coverage. The Vesper-only region adapter is exercised separately.
A native recursive storage/effect ABI is still needed for full-parser JIT or
AOT execution. Macros remain outside this parser's supported grammar, and
editor-only CST recovery wrappers remain separate from the batch contract.
