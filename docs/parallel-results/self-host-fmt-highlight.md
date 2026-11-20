# Self-hosting SH1 lane 11: formatter and lexical highlighter in Vesper

Claim: #1154. Plan: `docs/agent-prompts/self-host-sh1/11-fmt-highlight.md`.
Base: origin/main `e29e63d`, merged up to `a73dfe2` (lane 5's #1163 and lane
10's #1166).

This lane ports `vesper-fmt` (the whole formatter: lexer gate, layout,
document algebra and renderer, limits and the idempotence check) and the
lexical part of `vesper-highlight` (Tier 0's class spans, in token and CST
mode) to Vesper. Both run on `@vesper/front`, the front end v2.

Rust stays the default for every build and every dependent. Nothing Rust is
deleted.

## What changed

Both crates use the seed pattern from Beacon, semver and the toolchain
pilots (`docs/parallel-results/self-host-toolchain-pilots.md`):

- `crates/<c>/seed/` is a package `<c>-seed` with the same lib name, built
  from the same `src/lib.rs` with no build script, so it is plain Rust. The
  root package and `vesper-hybrid` use the seeds (root patch in its own
  commit). `vesper-nexus`'s fmt dev-dependency also moves to the seed:
  the root package links nexus, and the port-backed `vesper-fmt` builds its
  object with `//:bin/vesper`, so Bazel would see a cycle.
- The original package builds the Rust code by default. Under the Cargo
  feature `vesper-port`, `build.rs` links the Vesper object through
  `vesper_port_harness::Port::build`, which sets `cfg(vesper_port)`, and the
  public functions call the port first.
- `vesper/src/lib.ves` holds each port. Each is one file, because consts
  cannot be imported across modules (gap 2 below). fmt is 1,401 lines and
  highlight is 798.
- `src/port.rs` decodes the port's results (fmt: the error codes and the
  BLAKE3 words; highlight: one packed word per span).
- Under the port, fmt's BLAKE3 digests in `format_for_verification` go
  through `@vesper-std/hash-sys`: `proof_source.rs` calls
  `crate::port::blake3_bytes`. The `.vsys` block is copied into the port's
  root module (gap 6), bound to `packages/vesper-std/hash/sys/shim.rs`.
- `tests/differential.rs` (cfg `vesper_port`) checks the port against the
  seed (renamed dev-deps `fmt-seed` and `highlight-seed`). Every result,
  errors included, must print the same.
- `tests/port_memory.rs` checks that the port frees everything it
  allocates.
- `port/test-port.sh` runs the crate's tests on the Rust, port and seed
  builds. It then builds three deliberately divergent ports (feature
  `vesper-port-divergent`, selected with `VESPER_FMT_DIVERGE` or
  `VESPER_HIGHLIGHT_DIVERGE`) and requires a named test to fail on each.
  Both scripts are appended to Linux port shard 7 in
  `scripts/test-port-harness.sh`.
- Bazel builds each port object and its divergent objects
  (`crates/<c>/BUILD.bazel`). The root `BUILD.bazel` gains `front_package`,
  `std_hash_package` and `std_hash_sys_package`. The port tests run in Cargo
  (shard 7), as for the other feature-gated ports.

### vesper-fmt

The port transliterates `src/lib.rs` and `src/doc.rs` charge for charge, so
the work budget refuses at the same unit as Rust.

- The front end's lexer feeds the gate.
- The gate counts expression chains and nesting and charges the same
  preflight.
- The layout builds the same document nodes.
- The renderer packs its work-stack entries into words.
- `format` runs Rust's sequence: gate the source, format once, gate again,
  gate the output, compare, then format the output again to check
  idempotence.

What the port hands back to the Rust code:

- **Macro sources:** sources in the macro profile (`m!` after a name, or a
  quotation keyword) have a different token cap and an opaque-region rule,
  so the port returns code 10 and the Rust code formats them. 64 of the
  repository's 900 `.ves` files are macro sources. The differential test
  checks that every source handed back is one.
- **Large sources stay in the port:** the gate lexes with lane 10's
  `lex_with_limits` at the hard ceilings and one diagnostic, as
  `guarded_lex` does, so sources past the front end's defaults (65,536
  tokens, 1 MiB) format through the port and the 131,072-token cap refuses
  at the same point (`large_sources_format_like_rust`).
- **Lane 5's else-if rule:** the gate keeps lane 5's per-level `if` bases
  (#1163), so an else-if chain of any length counts like one `if`, and the
  crate's `else_if_chains` test passes on all three builds.
- **Recovery and the proof model:** `format_recovering` and the proof model
  stay in Rust. They call the same `format_source`, so the proof model runs
  over the port's output under the port build.

Differential coverage, all passing:

- All 900 repository `.ves` files: 895 format, and each canonical output
  formats to itself on both builds.
- The same files with every whitespace run scrambled.
- 1,500 edited copies from a fixed seed (`VESPER_FMT_FUZZ` sets the count).
- Sweeps of the work, output, document-node, token, depth and input limits
  across the point where each starts to refuse, on five sources.

The crate's own tests also pass under the port: format (27), recovery,
macro_source, proof_model and proof_lean.

Divergent builds:

| Area | Change | Fails | Still passes |
| --- | --- | --- | --- |
| doc | 99-column ribbon | `repository_sources_format_byte_identically` | `limit_sweeps_fail_at_the_same_point` |
| gate | commas no longer end a chain | `repository_sources_format_byte_identically` | `limit_sweeps_fail_at_the_same_point` |
| layout | a trailing comma no longer keeps a list vertical | `repository_sources_format_byte_identically` | `fuzzed_sources_give_the_same_result` |

### vesper-highlight (Tier 0 lexical classes)

- **Token mode:** the port lexes with the front end and classifies each
  token. This covers keywords, contextual words (`loop`, `for`, effect
  names compared by interned symbol), wire scalars, effect rows and modal
  starts.
- **CST mode:** the port also walks the parser's events, so tokens under
  `TYPE` nodes are types.
- **Results:** spans come back as one word each (end, class and flags).
  `verify_lossless_spans` still runs in Rust on the result.
- **Limits:** the port lexes with the Rust code's ceilings: the hard token
  ceiling, plus the hard diagnostic ceiling in token mode and the parser's
  default 128 in CST mode. Only when the lexer stops early does it give
  `Refused`, and the Rust code classifies the flat stream. Rasterizing (`shot`, chroma and HTML)
  stays in Rust.

Differential coverage, all passing:

- Every repository source in both modes.
- Every prefix of the snippet corpus.
- 1,500 fuzzed sources.
- A check that sources past the ceilings fall back to Rust.

Divergent builds (each fails `repository_sources_classify_identically`, and
`sources_past_the_front_end_ceilings_are_classified_by_rust` still passes):

| Area | Change |
| --- | --- |
| effects | `handle E` no longer names an effect |
| words | `loop` is never a keyword |
| types | tokens inside `TYPE` nodes are not types |

## Numbers

All numbers are from release builds with the optimized compiler, x86_64
Linux, in the cloud container.

- **ABBA:** vesper-bench in `examples/abba.rs`, with Rust as A and Vesper
  as B.
- **Instruction counts (Ir):** callgrind, measured as the difference
  between runs of N batches and 1 batch, divided by N - 1.
- **Workloads:** "largest" is the largest `.ves` file in the repository,
  `crates/vesper-ebpf/vesper/src/verifier.ves`. "corpus" is every
  repository `.ves` file. For fmt, the corpus leaves out the 64 macro
  sources, which the port hands to Rust.

| Workload | Vesper/Rust time | Rust Ir | Vesper Ir | Ir ratio |
| --- | --- | --- | --- | --- |
| fmt largest | 0.49x (31.9 ms vs 64.5 ms) | 373.4M | 276.3M | 0.74x |
| fmt corpus | 0.53x (746 ms vs 1,423 ms) | 10,759M | 8,385M | 0.78x |
| highlight largest, tokens | 1.12x (3.19 ms vs 2.88 ms) | 10.7M | 32.8M | 3.07x |
| highlight largest, CST | 0.32x (5.19 ms vs 16.3 ms) | 119.5M | 55.6M | 0.46x |
| highlight corpus, tokens | 1.83x (93.4 ms vs 51.6 ms) | 339.6M | 1,028.6M | 3.03x |
| highlight corpus, CST | 0.37x (168 ms vs 463 ms) | 3,264M | 1,703M | 0.52x |

These are after lane 10's lexer speedups (#1166). Before them, on base
`e29e63d`, the same workloads were 0.49x, 0.63x, 1.51x, 0.39x, 2.58x and
0.45x of Rust's time.

What the numbers say:

- **fmt:** the formatter is faster than Rust. Rust builds a rowan tree and
  the port works on flat tables, and that gap outweighs the front end's
  slower lexer.
- **highlight CST mode:** also faster, for the same reason.
- **highlight token mode:** slower (1.1x to 1.8x). It is nearly all
  lexing, and the front end's lexer is still slower than Rust's.

Callgrind reports the port's functions as anonymous `vesper_function_N`, so
the profile does not show where the port's time goes (gap 7).

## Stage 1 guard

This PR does not touch `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**`: `git diff --stat` over those paths is empty.
The bootstrap-stage1 workflow runs on the branch; its digests are recorded
in the PR.

## Compiler gaps

1. **No glob imports.** `use self::kinds::*;` gives E0002 "expected an
   imported item name". Workaround: explicit lists, which then hit gap 2.
2. **Consts cannot be imported from another module.** With
   `pub const K: u64 = 3;` in `kinds.ves`, `use self::kinds::{K};` gives
   E0111 "module self::kinds has no item 'K'". Workaround: each port is one
   file.
3. **`match` and blocks are not operands.**
   `a && match x { 0 => true, _ => false }` gives E0002 "expected
   expression", and so does a `{ ... }` block as an operand. Workaround: small
   helper functions (`row_open` and `modal_start` in the highlight port).
4. **Every `pub` item of the library is exported, consts included.** A
   `pub const` becomes a broken `const$`-prefixed binding name. Workaround:
   only the entry points are `pub`.
5. **Foreign struct types get mangled names in the generated binding.**
   Returning `@vesper-std/hash::blake3::Digest` gave an unusable Rust type
   name. Workaround: the port returns its own `Hash32 { w0..w3 }`.
6. **`.vsys` imports are read only from the root package's root module.** A
   dependency's `.vsys` library (hash-sys's `Blake3`) is not inherited.
   Workaround: the fmt port copies hash-sys's `// | sys library Blake3`
   block. Also, `vesper check` on the package fails on the copied `.vsys`
   block, while `vesper build --lib` succeeds.
7. **Callgrind shows anonymous symbols** (`vesper_function_N`) for port
   code, which makes profiling ports slow.
8. **Slices are second class.** `let s = out.as_slice();` is refused, so
   every call site passes `in out.as_slice()` inline.
9. **`vesper fmt` refuses deep boolean chains with DepthLimit.** A
   15-operand `||` chain inside an else-if arm (the control-keyword test in
   highlight's `classify`) is over the nesting limit of 24. This is the
   formatter itself, Rust and port alike. Workaround: the
   `control_keyword` and `declaration_keyword` helpers. Lane 5 (#1163)
   relaxed only the else-if case.

## Not in this PR

- Macro-profile formatting, `format_recovering` and the proof model stay
  Rust.
- Rasterizing in vesper-highlight stays Rust.
- The compiler does not switch to either port. Both seeds stay the default
  until the owner moves them.

Unrelated to this PR: in this checkout, the Cargo run of
`tests/self_host_front_end.rs::differential_over_repository_sources` fails
on `crates/vesper-port-pilot/vesper`, which this PR does not touch. The
Stage 1 parser gives a different node count there. The Bazel target does
not read that directory.
