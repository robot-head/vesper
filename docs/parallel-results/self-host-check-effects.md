# Self-hosting SH2 lane 8: effects and capabilities in Vesper (part 1)

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1180. Base: `main` at `e43a56f`. Lane prompt:
[`docs/agent-prompts/self-host-sh2/08-effects.md`](../agent-prompts/self-host-sh2/08-effects.md).
Rust stays the default checker everywhere, and no Rust code was deleted.

This first PR ports the capability effect rules (`src/capability_effects.rs`,
`check_world` and everything it calls) and proves them against Rust. The
hybrid swap, the Beacon trace rules (`src/trace_effects/rules.rs`) and the
verifier policy inputs come in follow-ups (see "What is left").

## Summary

- **`@vesper/check-effects`** (`packages/vesper-check-effects`, about 3,650
  lines of Vesper): `Walker` (taint, the call discipline, the realtime,
  DetFloat, ConstDenorm and ConstTime rules, closures, handlers and their
  continuations, loops to a taint fixpoint), `summarise` (secret-parameter
  inference, inferred `inout` outputs, secret resumes, to a fixpoint),
  `check_world` (both walks, E0706 at the result boundary, E0724) and
  `realtime_recursion` (E0722 over the realtime call graph), with the
  diagnostics sorted and deduplicated as Rust does.
- **Census design kept**: types are `check-data` type ids, names are
  interned symbols, bodies are an index graph over one word pool (the
  `@vesper/ast` layout), scopes are one stack of rows with marks, facts are
  sorted `u64` sets, the call graph is a flat edge list between interned
  node names. `secret_type` is memoised by type id.
- **Differential**: 0 differences on every input below, over more than
  110,000 diagnostics, with every diagnostic code of the module firing.
- **Speed**: 0.28x of Rust's time (ABBA, release), 0.49x with encoding the
  input included.

## What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-check-effects/{Loom.toml,README.md,src/*.ves}` |
| Test (encoder of the task words, differential, ABBA, callgrind side) | `tests/self_host_check_effects.rs` |
| `check_world` made `pub` for the test (one word and a doc line) | `src/capability_effects.rs` |
| Bazel data and `long` timeout for the test | `BUILD.bazel` |
| Scoreboard row | `docs/maintenance/self-hosting-plan.md` |
| Report | `docs/parallel-results/self-host-check-effects.md` |

No field was added to `@vesper/check-data`: the program the rules walk
(functions with bodies, effect operations, call instantiations) is this
task's own words after the harness world, decoded by `prog.ves`. If a
second lane needs function bodies, the AST tables can move into
`check-data` unchanged.

## The differential

Each side writes one line per diagnostic: code, span, message, help and
the related labels. The test compares texts, so a difference names its
line.

| Input | Worlds | Diagnostics | Worlds differing |
| --- | --: | --: | --: |
| Corpus (every clean corpus world) | 282 | 0 | 0 |
| Fixtures: `tests/fixtures/{capability_effects,capability_verifiers,floats,vector}`, whatever their other diagnostics | 58 | 39 (all 17 codes) | 0 |
| Mutant `ConstTime` on every function, corpus + fixtures | 340 | 16,777 | 0 |
| Mutant `Realtime` | 340 | 18,418 (1,560 E0722, 60 E0723) | 0 |
| Mutant `DetFloat` | 340 | 16,777 | 0 |
| Mutant `DetFloat + ConstDenorm` | 340 | 28,894 (12,120 E0732) | 0 |
| Mutant: all four | 340 | 30,554 | 0 |
| Fixtures with the JIT optimizer off | 58 | 39 | 0 |

The codes are E0700, E0702-E0709, E0720-E0725 and E0730-E0732. The four
tests take about 4 minutes together in an unoptimized build on a 4-core
host.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
only `src/` change is the visibility of `capability_effects::check_world`.
The `bootstrap-stage1` workflow runs on the branch; the PR links the run.

## Numbers

ABBA, best of six per side, release build, Linux x86-64, 4 cores. The
input is the 340 corpus and fixture worlds plus their all-capabilities
mutant (680 worlds):

| | Rust `check_world` | Vesper `check_effects` | Ratio |
| --- | --: | --: | --: |
| Checking | 2.32 s | 0.66 s | 0.28x |
| Encoding the inputs (Rust harness) | | 0.47 s | |
| Checking + encoding | 2.32 s | 1.13 s | 0.49x |

As in the census, the speed comes from the data design: Rust clones a
`Type` tree for every expression type it asks for, keys scopes and facts by
`String`, clones the call graph in every loop fixpoint round, and walks
`secret_type` afresh on every expression. The
Vesper side compares `u32` ids, truncates flat tables and memoises
`secret_type`.

Callgrind numbers are not in this PR: the ignored `effects_callgrind_side`
test is the harness for them, and they come with the hybrid swap.

## Compiler gaps

1. **`effect` and `handle` are keywords, not only `eff`** (census C1, SH2
   lane 3 covers `eff`). `fn effect() {}` and `fn handle() {}` are E0002;
   so is `let effect = 1;`. The port spells them `decl_of`, `handler` and
   `ename`.
2. **A `const` cannot be imported from another module** (check-data gap 1):
   `walker.ves` and `check.ves` each declare the capability bits.
3. **`vesper fmt` stops at `DepthLimit` on a long `||` chain.** A
   function whose body is one `a || b || ... ` of 24 calls is refused
   (`walker.ves`'s intrinsic-name test). The port checks the names in two
   halves of 10 and 14 terms, and all five files are formatted. Repro: a
   `-> bool` function returning 24 `f(x)` terms joined by `||`.
4. **No borrowed compiler trees for the AST.** The bodies cross as
   hand-encoded words (`encode_task`): 0.47 s, 42% of the Vesper side's
   total.
   ABI v4 owned trees (SH1 lane 6) are the way to drop the encoder, which
   is the check-data ABI v4 follow-up.

## What is left

- Hybrid swap: a `check_world` swap point in `src/capability_effects.rs`
  in the parse-swap pattern, the encoder moved into
  `crates/vesper-hybrid` (module `effects`), the full suite on the hybrid
  compiler, and the scoreboard's corpus numbers and callgrind counts.
- `src/trace_effects/rules.rs` and the checker half of `trace_effects.rs`
  (E0750-E0758) on the same tables.
- `capability_policies` (the Stage 5 verifier inputs): the walk is there;
  it needs the policy output and its differential.
- The small helpers `check.rs` calls (`secret_prelude`, `used_names`,
  `reserved_declaration`, `row_entry`, `handler_rejected`) move with the
  checker module that calls them.
