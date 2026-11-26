# Self-hosting SH1 lane 14: the vesper-forge port, part 1

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1157 (this PR is part 1, "Part of #1157"). Plan:
`docs/agent-prompts/self-host-sh1/14-forge.md`. Base: `main` at `62b4e34`
(lane 9, PR #1168, merged).

This lane starts the port of `vesper-forge` to Vesper. Part 1 ports the
hygiene core that every macro expansion goes through: the delimiter check,
the parser of the Forge lexical fragment and scope-set resolution, with the
registry's visibility and claim checks. Rust stays the default: the compiler
links the frozen Rust seed copy, the crate builds Rust unless its
`vesper-port` feature is on, and nothing Rust is deleted.

## What the crate is, and what the plan assumed

The lane prompt describes forge as "sugar expansion and macro expansion with
hygiene over the @vesper/ast tree". The crate does not touch the AST. It is
the compiler-independent hygiene and authority core that untrusted macro
output passes through:

| Module | Lines | What it does | Part 1 |
|---|---|---|---|
| `host.rs`, `identity.rs` | 790 | the `HygieneHost`: session-qualified scopes, expansions, splice handles, binders and symbols from checked counters; splice arenas; quotation expansion | stays Rust (host state); its delimiter check is ported |
| `syntax.rs` | 268 | parser of the lexical fragment (`let`, `handle .. with`, calls, blocks, paths) | **ported** |
| `resolve.rs`, `registry.rs` | 770 | scope-set binder and reference resolution, aliases, operation provenance, claim and visibility checks | **ported** |
| `lex.rs` | 66 | fixture lexer over Unicode `char` classes | stays Rust (gap 4) |
| `typed_quote.rs` | 1,927 | sealed typed `Expr` quotations over `vesper-syntax` tokens | stays Rust; its resolution runs on the port |
| `authority.rs` | 839 | transitive authority over the trusted graph | part 2 |
| `transport.rs`, `quote_transport.rs` | 462 | the flat sandbox transport | stays Rust (wasm host boundary) |

Sugar is not in forge: the parser desugars it (`for`, `continue`, `const`
uses, or-patterns), and lane 9 ported that lowering into `@vesper/ast`. So
"byte-identical expansions as canonical AST dumps" does not apply to this
crate. What part 1 makes byte-identical instead is the whole
`ResolvedFragment` (tokens, term, binders with their scope sets, references,
operations with their origin sets) or the exact `ForgeError`, plus the host
state after the call.

## What changed

The crate uses the seed pattern of `vesper-beacon` (Rust by default):

- `crates/vesper-forge/seed/` is the package `vesper-forge-seed` with the
  library name `vesper_forge`, built from the same `src/lib.rs` without a
  build script. The root package and `vesper-hybrid` (which mirrors the
  root's dependencies) depend on it by rename, with `default-features =
  false` as before; that switch is its own commit.
  `vesper-forge-runner`, outside the compiler's closure, keeps depending on
  `vesper-forge`.
- `vesper-forge` gains the features `vesper-port` and
  `vesper-port-divergent`. Its build script always depends on
  `vesper-port-harness` (so the seed-copy guard counts it as port-backed)
  and links the Vesper library `forge` only under `vesper-port`.
  `vesper_forge::IMPLEMENTATION` says which build is linked.
- `vesper/` is the Loom package `@vesper/forge` (1,768 lines):
  - `parse.ves`: `check_delimiters` and the fragment parser. It emits the
    term in post-order and lists every finished path in creation order.
  - `resolve.ves`: the `Resolver` (parameters, `let` scopes with the origin
    ranks of `Ctx`, aliases, maximal-candidate lookup with sorted ambiguous
    candidates, operation origins) and `visible_from` / `validate_claim`.
  - `lib.ves`: the entry points `fg_delimiters`, `fg_parse` and
    `fg_resolve` (export ABI v2: lent `u64` and `u8` slices in, one
    `ChunkSeq[u64]` out).
- `src/port.rs` (cfg `vesper_port`) adapts the host to the kernels. The host
  stays in Rust. The adapter flattens tokens, origins, parameters and the
  term into word tables, and keeps a per-host table of the globals, their
  definitions and the registry's packages, rebuilt only when a global is
  declared. After each kernel call it replays on the host the identities
  the kernel minted, in Rust's order: the joined path symbols the parser
  interns (also those before a parse error), then the scopes and binders the
  resolver minted (also those before an error). So both builds leave the
  host in the same state, on success and on every error, including
  `IdentityExhausted` at any counter.
- `HygieneHost::resolve`, `resolve_preparsed_with_parameters` (which the
  typed quotation host uses) and `check_delimiters` forward to the port
  under `cfg(vesper_port)`; the Rust bodies stay for the seed.

The port keeps Rust's error order: the kernels stop at the first failure
Rust would raise and report it with the same fields. Delimiter failures in
the resolver are reported after the expansion ancestors are collected, which
cannot fail on validated tokens, so the order is unchanged.

## Tests

| Test | What | Result |
|---|---|---|
| the crate's 7 test files (65 tests) | `authority`, `hygiene`, `sandbox_execution`, `transport`, `typed_quote`, `typed_tuple`, `typed_function_parameters` | pass on the Rust build, the seed and `vesper-port` |
| `tests/fragment.rs` (new, both builds) | reserved words, delimiter matching and nesting, a resolved fragment's operations | pass on all three |
| `tests/port_differential.rs` | the random corpus (`tests/corpus`): callers, up to two nested expansions with splices (also inside paths and binders), claims with right and wrong printed names, duplicate globals, mutated texts, and hosts with tight identity and nesting limits. The port and the seed must give the same `{:#?}` result and the same next scope and next symbol afterwards. | 4,000 cases in CI, 0 differences; 100,000 locally, 0 differences |
| `tests/port_memory.rs` | the Vesper runtime's live allocations after 6,000 corpus cases | unchanged |

The corpus reaches success and every error the kernels raise. Over 100,000
cases: `Ok` 13,723, `Unbound` 40,225, `Parse` 20,604, `QuoteLimit` 7,616,
`UnbalancedDelimiters` 6,912, `WrongDefinitionKind` 6,005, `Ambiguous`
2,393, `MixedPath` 985, `PrintedNameMismatch` 665, `NotADependency` 251,
`IdentityExhausted` 192, `PreResolvedBinder` 164, `ExpectedGlobal` 141,
`PrivateDefinition` 124. The test fails if a run misses one of the main
classes. It found one real difference while it was written: the adapter
returned a parser depth error before interning the paths finished before
it, which leaves the host one symbol behind Rust.

Divergence checks (`port/test-port.sh`, `EDITS` in `build.rs`, the same
edits as the Bazel `forge_vesper_divergent_*` objects), one per ported area:

| Area | Edit | Caught by |
|---|---|---|
| delimiters | a closing delimiter closes any opener | `fragment::delimiters_must_match_and_nest` |
| parse | `with` is no longer reserved | `fragment::reserved_words_are_not_identifiers` |
| resolve | a binding scope covers tokens newer than its binder | `hygiene::same_package_caller_locals_cannot_capture_macro_globals` |
| registry | package-private definitions are visible to dependents | `hygiene::forged_def_id_claims_are_rejected` |

The differential test fails on all of them except `parse` (the corpus does
not bind `with`).

## Numbers

`scripts/bench.sh` runs `examples/abba.rs`: vesper-bench ABBA, 30 pairs,
the Rust seed as A and the port as B in one binary, on a 4-core x86-64 cloud
machine, release build. Each operation resolves fresh copies of the same
tokens on one host. Instructions are callgrind's, for 5 batches (a 6-batch
run minus a 1-batch run, so setup is not counted).

| Workload | Tokens | Rust | Vesper | Wall | Instructions |
|---|---|---|---|---|---|
| `chain`: one caller fragment, 40 blocks of 25 nested `let`s | 8,402 | 4.84 ms | 4.83 ms | **0.99x** | 1.13x |
| `macro`: one expansion, a 220-token quotation with `let`s, `handle`s and splices, in a caller | 287 | 59.1 us | 64.8 us | **1.10x** | 1.17x |
| `corpus`: 2,000 resolving corpus cases, 8.6 tokens each | 17,120 | 3.91 ms | 7.47 ms | **1.85x** | 1.64x |

Before the adapter kept the host table and the kernels stopped allocating
per path and per output list, `corpus` was 2.92x and `chain` 0.96x.

Where the `corpus` instructions go (Vesper build, 5 batches, 239.7M Ir;
Rust 146.1M): the two kernels take 75.2M (`fg_resolve` 44.3M, `fg_parse`
30.9M), the adapters 85.3M (building the word tables, copying token texts,
decoding the term and the records), and host work both builds do (token
validation twice per call, collecting expansions, building the result's
names) the rest. The kernels' share is close to what the Rust parser and
resolver cost, so the gap on small fragments is marshaling, which part 2
can remove (see "What is left").

Rust stays the default under the owner's parity rule.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed. The
`bootstrap-stage1` workflow run on this branch (`bootstrap/run.sh stage1
--reproduce --outcome`) is linked from the PR, and its verified artifact
digests equal `bootstrap/stage1.lock`.

## Changes outside the lane's paths

- `Cargo.toml`, `Cargo.lock`: the seed joins the workspace, and the root
  depends on it by rename (root patch rule, its own commit).
- `crates/vesper-hybrid/Cargo.toml`: the same rename, since the hybrid build
  mirrors the root's dependencies (as lanes 1 and 11 did).
- `scripts/test-port-harness.sh`: the port's line in Linux shard 7.
- `tests/fixtures/self_host/driver.rs`: the Stage 1 self-host corpus skips
  `crates/vesper-forge/vesper`, which uses `use` (#880), like every port.

## Compiler gaps

1. **`effect` cannot name a local.** `let effect = 1;` fails with E0002
   "expected pattern" and then "expected expression" at each use, which does
   not say that `effect` is reserved. Repro:

   ```text
   fn f() -> u64 {
       let effect = 1;
       effect
   }
   ```

2. **No borrowed views in structs.** A struct cannot hold the kernel's
   borrowed input slices (`copy struct View { w: Slice[u64] }` built from an
   `in s: Slice[u64]` parameter fails with E0305 "cannot move 's' through a
   borrowed parameter"), so `resolve.ves` threads four slices (`w`, `text`,
   `g`, `gt`) through every function. Repro:

   ```text
   copy struct View {
       w: Slice[u64]
   }

   pub fn f(in s: Slice[u64]) -> u64 {
       let v = View {
           w: s
       };
       0
   }
   ```

3. **Empty sequences still allocate** (known since #1005): the resolver's
   state has ten scratch sequences, so every call pays ten `malloc`s before
   it does any work, which shows on 8-token fragments.

4. **No Unicode character classes.** `lex.rs` classifies `char`s with
   `is_alphabetic`, `is_alphanumeric` and `is_whitespace`; Vesper has ASCII
   byte classes only (#987), so the fixture lexer stays Rust.

5. **No resident host state across calls without a handle ABI for reads.**
   The symbol table and the host's tables live in Rust; each call copies the
   token texts it needs. Export ABI v2 handles could own them in Vesper, but
   the host also needs `&str` views of every symbol (`HygieneHost::text`),
   which a handle's view gives only per field, not per symbol range.

## Part 2: `authority` (`#1772`)

Ported `AuthorityGraph::requirements` (`crates/vesper-forge/src/authority.rs`) into `crates/vesper-forge/vesper/src/authority.ves` (`fg_requirements`), wired via `crates/vesper-forge/src/port.rs` under `#[cfg(vesper_port)]`, and blessed `crates/vesper-forge/port/binding.rs`.

- All `vesper-forge` (`vesper_port`) and `vesper-forge-seed` (Rust default) tests pass (`tests/authority.rs` 14/14, `tests/port_differential.rs` 2/2, `tests/hygiene.rs` 17/17, `tests/typed_quote.rs` 10/10, `tests/typed_tuple.rs` 10/10, `tests/typed_function_parameters.rs` 8/8, `tests/transport.rs` 4/4, `tests/fragment.rs` 3/3, `tests/sandbox_execution.rs` 2/2).

## What is left

- Marshaling cuts for `fg_parse`/`fg_resolve`: one call for parse and resolve when every path is a single token, and a Vesper-side mirror of the symbol table appended on `intern`.
- The quotation expansion of `host.rs` (`expand_items`, splice lookup) is host state; it can move once the host's arenas do.
- `typed_quote.rs` parses with `vesper-syntax`'s lexer, so it moves with the parser swap (lane 12) rather than here.
- `src/forge_frontend` (the `vesper forge` command) is compiler code in `src/` and belongs to SH2, not to this crate's lane.

## Part 3: the sugar surface profile in `@vesper/front` (Claude takeover)

The Claude project thread "Port forge macro expansion to Vesper" took this
lane over on 2026-11-25 (the Antigravity and Codex agents are paused). The
plan for the rest of the lane is on #1157: the compiler reaches
`forge_frontend` through `parse_compiler_input`, whose fallback runs
`vesper_syntax::sugar_expand`, and that parses every round in the surface
profile (`parser::parse_expansion_scopes`). `@vesper/front` had no surface
profile, so it comes first.

What changed:

- `packages/vesper-front`: the surface profile of the Rust parser, behind
  the new entry `parse_surface_tables(source, limits, counts)` (the byte
  limit and `include_tests` in `limits`, the token and diagnostic limits in
  `counts`):
  - `lex.ves`: `lex_sugar_with_limits`, where a lone backtick is a
    `Backtick` token (kind 81) instead of an invalid one.
  - `parse.ves`, section "Surface profile": sugar declarations
    (`parser/sugar_decl.rs`: annotations, sorts, `infix` sorts with quoted
    operators, `emits` rows, rules, holes, sequence holes and
    repetitions with separators), sugar phrases at item, statement and
    expression starts with heads, bracketed head words, continuations and
    `ghost` (`parser/sugar_phrase.rs`), pattern phrases, pipelines with
    `_` placeholders, dotted operators and tagged literals
    (`parser/sugar_ops.rs`), and expression macro declarations and calls
    (`parser/surface_macro.rs`), plus `test` blocks parsed as items when
    `include_tests` is set.
  - The core parser's hooks all check `p.surface` first. Two changes touch
    the batch profile, both without changing what it parses: binary
    operator levels are now the Rust parser's (twice the old numbers, which
    leaves level 3 for the pipelines), and call arguments, tuple and array
    elements and index bounds go through `dexpr` (`delimited_expr`), which
    differs from `expr` only in the surface profile.
- `crates/vesper-syntax/src/parser.rs`: a `#[doc(hidden)]` test hook,
  `parse_surface_cst`, which is `parse_expansion_scopes` without the
  expression macro validation (`macro_source::validate_surface`, which is
  part 6). It does not change behaviour.
- `tests/self_host_front_v2.rs`: four surface differentials (CST rows and
  diagnostics) and an ignored speed test.

Nothing calls the surface profile yet. Part 4 (sugar expansion in Vesper)
is its first user.

Tests (release, this 4-core cloud machine):

| Test | Sources | Result |
|---|---|---|
| `surface_profile_matches_on_the_sugar_corpus` | 103 hand-written surface cases plus the string literals of the sugar, macro and recovery test suites, `sugar_expand`, `macro_source`, `forge_frontend/sugar.rs`, `packages/sugar.rs` and the forge, fmt and package CLI tests; each with and without test blocks. Every surface node kind occurs. | 0 differences |
| `surface_profile_matches_on_repository_sources` | every repository `.ves` file | 0 differences |
| `surface_profile_matches_on_the_fuzzed_corpus` | 40,000 rounds (`VESPER_FRONT_FUZZ=40000`; CI runs 3,000) of edited sugar sources and surface token soup | 0 differences |
| `surface_profile_matches_under_explicit_limits` | byte, token, diagnostic and depth limits | 0 differences |
| the 13 existing front end v2 differentials | the batch and editor profiles | pass, unchanged |

Speed (`surface_profile_speed`, ignored; release, `-C target-cpu=native`,
20 alternating rounds over 1,484 sources: the repository `.ves` files and
the hand-written cases): the Vesper tables take **0.35x** the time of the
Rust surface parse and its rows (median; range 0.30x to 0.48x).

A divergence check (changing one surface diagnostic's message) fails
`surface_profile_matches_on_the_sugar_corpus` at the case that reaches it.

## Part 4a: the sugar rule matcher (`@vesper/sugar`)

`packages/vesper-sugar` is the new home of the sugar expansion in Vesper
(`crates/vesper-syntax/src/sugar_expand*`, about 3,800 lines of Rust).
Part 4a ports its rule matcher, `sugar_expand/matcher.rs`
(`match_rule`, `captures_tail`), to `src/matcher.ves`:

- the pattern compiler (literals with their adjacency, typed holes,
  sequence holes, block tails and nested `$( ... ) sep* / +`
  repetitions, with every refusal message),
- the backtracking matcher (states on an explicit stack whose frames
  live in one pool, an event log of values and repetition extents), and
- typed hole validation, which reparses each candidate fragment with
  the surface profile (`@vesper/front`'s new `surface_fragment_status`,
  Rust's `parse_sugar_fragment`).

Work accounting follows Rust exactly: every `spend` happens at the
same point for the same amount, so the remaining budget and the work
limit refusal agree as well as the captures.

Other changes:

- `@vesper/front`: `surface_fragment_status`, `lex_sugar_raw_with_limits`
  (the sugar lexer without symbols) and `core_operator_kind`. Short
  sources start with a 32-slot interner instead of 256; symbol ids do
  not change.
- `crates/vesper-syntax/src/sugar_expand.rs`: a `#[doc(hidden)]` test
  hook, `match_rule_words`, that encodes `match_rule`'s result the way
  the Vesper entry does. It does not change behaviour.
- `tests/self_host_sugar.rs`: the differential.

Tests (release, this 4-core cloud machine):

| Test | Cases | Result |
|---|---|---|
| `matcher_matches_on_the_rule_examples` | 62 hand-written rules and calls (the matcher's own tests, every sort, passing modes, repetitions, adjacency and each refusal), under 4 work budgets, with and without a tail | 0 differences |
| `matcher_matches_on_the_sugar_test_suites` | every rule pattern in the string literals of the sugar test suites, `sugar_expand`, the forge and package CLI tests and the surface corpus, against the sugar uses of its own source and a spread of others (about 800 cases) | 0 differences |
| `matcher_matches_on_the_fuzzed_corpus` | 40,000 rounds (`VESPER_SUGAR_FUZZ=40000`; CI runs 4,000) of edited rules and calls under 4 budgets | 0 differences |
| front end v2 and AST differentials | after the `@vesper/front` changes | pass, unchanged |

Speed (`matcher_speed`, ignored; one host call over all corpus cases):
the Vesper matcher takes **1.5x to 1.7x** the time of Rust's. Its
fragment reparse alone is faster than Rust's (3.0 µs against 4.5 µs per
fragment); the rest is the matcher's fixed cost per call (two lexes and
the tables). The module flip is measured on the whole expansion at the
swap (part 5), where each round's whole-source surface parse
(0.35x) dominates.

## Part 4b-1 to 4b-3: template skeleton, template-scopes parse and AST

`template::render` (crates/vesper-syntax/src/sugar_expand/template.rs)
first builds a skeleton of the rule's template: holes become markers and
repetitions are unrolled. `flow::render` then parses that skeleton with
`parser::rust_parse`, falling back to `parser::parse_template_scopes`, and
walks the AST to rename template binders and evaluate captures once.
These three parts port everything that walk needs, but not the walk
itself (part 4b-4).

- **4b-1, skeleton** (`packages/vesper-sugar/src/template.ves`): ports
  `parse`, `Skeleton::emit` and `operand_span`. It reads the matcher's
  capture words in place, and its step and byte limits refuse at the
  same points as Rust's. The Rust test hook is
  `sugar_expand::template_skeleton_words`.
- **4b-2, template-scopes CST** (`@vesper/front` `parse_template_tables`):
  in this profile, phrase bodies and tagged payloads parse as blocks, and
  raw phrase headers and nested pattern sugar are errors. To the core
  parser, a phrase is a block of its operands, which affects match-arm
  commas and expression heights. The Rust test hook is
  `parser::parse_template_cst`.
- **4b-3, template-scopes AST** (`@vesper/ast` `lower_template_source`,
  `ast_template_dump`):
  - Sugar and macro declarations and item phrases produce no item.
  - A phrase becomes a block of its operands, a tagged literal a tuple of
    its block, a pattern phrase a wildcard, and a sugar operator a binary
    expression.
  - Delimited expressions reopen struct literals.
  - The lowering reads the CST to tell which surface construct it is at.
  - The lowering's binary levels now match Rust's.

| Test | Cases | Result |
|---|---|---|
| `self_host_sugar` `skeleton_matches_on_the_template_examples` | 48 hand-written templates (repetitions, splices, shared holes, grouping, every refusal) × 4 byte limits × tail | 0 differences |
| `self_host_sugar` `skeleton_matches_on_the_sugar_test_suites` | the rules of the sugar test suites against their uses (732 renders) | 0 differences |
| `self_host_sugar` `skeleton_matches_on_the_fuzzed_corpus` | 40,000 rounds locally (CI 4,000) | 0 differences |
| `self_host_front_v2` surface tests with `compare_template` | the sugar corpus, 20,000 fuzzed sources locally, and depth-limit cases, both bare and wrapped as `flow::render` wraps them | 0 differences |
| `self_host_ast` `template_scopes_*` | 899 template sources, every repository `.ves` file, and 60,000 fuzzed sources locally (CI 3,000) | 0 differences |
| core AST and front end differentials | after the changes | pass |

## Part 4b-4 to 4b-6: template flow analysis, scope captures, pattern hygiene

- **4b-4, flow analysis** (`packages/vesper-sugar/src/flow.ves`): ports
  `template/flow.rs` and `Rendered::hole`. It parses the skeleton as
  `flow::render` does: first with the core parser (`@vesper/ast`
  `lower_source`), then with the template-scopes profile, then unwrapped.
  It renames the template's binders to fresh names, plans single
  evaluation of captured expressions, and checks each place capture's
  pass. `emit` then substitutes the captured text and records the holes,
  origins and places. The Rust test hook is
  `sugar_expand::template_render_words`.
- **4b-5, scope captures** (`packages/vesper-sugar/src/scope.ves`): ports
  `scope.rs`. A scope capture checks the `let exit = ..; cleanup;
  exit.resume()` protocol and finds the body's exits in the core AST. It
  reads the caller's effect rows and result type from the surface-profile
  AST of the whole source (`@vesper/ast` `lower_surface_source`, new). It
  then wraps the body in the exit loop and handlers, dispatches at the
  resume, and remaps holes, origins and places through the edits. The
  Rust test hook is `sugar_expand::template_scope_words`.
- **4b-6, pattern hygiene** (`packages/vesper-sugar/src/hygiene.ves`):
  ports `hygiene::pattern`. A binder that a pattern template introduces
  literally gets a fresh name, written `field: fresh` for a shorthand
  field. Binders spliced from holes keep their names, and holes, origins
  and places shift with each edit. The Rust test hook is
  `sugar_expand::template_pattern_words`. `protect`, the caller-side
  renaming, comes with the driver (part 4c).
  - Gap (4b-5 and 4b-6): the surface profile does not yet run Rust's expression macro
    validation (`macro_source::validate_surface`, part 6). A source with
    an invalid expression macro is refused by Rust's scope capture but
    not by the Vesper one.

| Test | Cases | Result |
|---|---|---|
| `self_host_sugar` `render_matches_on_the_template_examples` | the 48 skeleton templates × 4 byte limits × tail | 0 differences |
| `self_host_sugar` `render_matches_on_the_sugar_test_suites` | the 732 suite renders | 0 differences |
| `self_host_sugar` `render_matches_on_the_fuzzed_corpus` | 4,000 fuzzed templates | 0 differences |
| `self_host_sugar` `scope_matches_on_the_scope_examples` | 7 scope rules over 37 callers (315 cases: lowered, lowered with a return effect, and 15 kinds of refusal, under 3 byte budgets) | 0 differences |
| `self_host_sugar` `scope_matches_on_the_fuzzed_corpus` | 500 edited callers (2,123 cases, 1,117 matching) | 0 differences |
| `self_host_sugar` `pattern_hygiene_*` | 15 pattern rules plus every render corpus above (examples, suites, 4,000 fuzzed) | 0 differences |
| `self_host_ast` `template_scopes_*` | now also the surface-profile AST of every case | 0 differences |

## Part 4c: the expansion driver

- **Driver** (`packages/vesper-sugar/src/driver.ves`): a port of the
  `sugar_expand.rs` expansion loop for sources without imports
  (`expand_sugar_for_execution`). It covers the round parse in the surface
  profile, definition reading and binding, the call queue with
  postponement of scope captures (nested reparses join one growing CST
  forest), operator associativity, the recursion rank check, the edits
  and source mappings, and the depth, step and work limits. It also ports
  `hygiene::protect`, the caller-side renaming, which runs after the last
  round. The entry is `@vesper/sugar` `sugar_expand_words`.
- Gap (part 6): as in 4b-5, the surface profile does not yet run
  `macro_source::validate_surface`. Rust refuses 2 suite expansions
  (`E0002` unsupported source macro category or context) that Vesper
  expands. The test counts these and fails on any other difference.

| Test | Cases | Result |
|---|---|---|
| `self_host_sugar` `driver_matches_on_the_scope_examples` | 7 scope rules over 37 callers, with and without tests (518 expansions) | 0 differences |
| `self_host_sugar` `driver_matches_on_the_sugar_test_suites` | the 133 sugar programs of the test suites, with and without tests (266 expansions) | 0 differences, 2 macro-validation gaps |
| `self_host_sugar` `driver_matches_on_the_fuzzed_corpus` | 500 edited programs, with and without tests | 0 differences |
| `self_host_sugar` `driver_speed` (release, ignored) | the 133 suite programs ×5 | Vesper 1.38x Rust (single run) |

## Part 4d: imports and definition scopes in the driver

- The driver now takes the caller's imports as
  `expand_sugar_with_scopes_for_execution` does: each imported
  declaration is read and bound under its alias, and an alias that
  renames a definition rewrites the rule's head word. Definition scopes
  are shared per scope, as Rust shares them per slice. A sugar use inside
  an imported template resolves in its author's scope: the innermost
  region that holds its head, outside the holes. Regions record their
  import, and the recursion rank check compares it. The Vesper entry is
  `@vesper/sugar` `sugar_expand_scoped_words`.

| Test | Cases | Result |
|---|---|---|
| `self_host_sugar` `driver_matches_on_the_import_examples` | the import examples of `crates/vesper-syntax/tests/sugar_expand.rs` and 6 refusals (missing scopes, invalid import, duplicate alias, unknown scope binding, broken and missing declarations) | 0 differences |
| `self_host_sugar` `driver_matches_on_imported_sugar_test_suites` | the suite and scope programs with their declarations moved into imports: visible by name, under aliases, and behind a shared scope (1,035 programs, 2,070 expansions) | 0 differences, 4 macro-validation gaps |
