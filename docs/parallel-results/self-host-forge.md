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

## What is left

- Part 2: `authority.rs` (the transitive authority closure and the policy
  checks) in Vesper, and the marshaling cuts that the numbers point to: one
  call for parse and resolve when every path is a single token (interning is
  then a no-op), and a Vesper-side mirror of the symbol table appended on
  `intern`, so calls pass symbol ids instead of copying texts.
- The quotation expansion of `host.rs` (`expand_items`, splice lookup) is
  host state; it can move once the host's arenas do.
- `typed_quote.rs` parses with `vesper-syntax`'s lexer, so it moves with the
  parser swap (lane 12) rather than here.
- `src/forge_frontend` (the `vesper forge` command) is compiler code in
  `src/` and belongs to SH2, not to this crate's lane.
