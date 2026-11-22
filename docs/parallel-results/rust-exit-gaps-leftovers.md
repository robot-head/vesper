# Rust-exit gap wave 1, lane 12: leftover modules and sys crates

Claim #1510. Prompt: `docs/agent-prompts/rust-exit-gaps-1/12-leftovers.md`. Base: `main` at `66255e17`.

## Part 1: `ir_opt_policy` in Vesper

`src/ir_opt_policy.rs` is the default compiler's `opt` path. It runs the
unchanged, Stage 1 guarded `ir_opt::optimize` only on the blocks where memory
forwarding can remove a read. Its screen has two steps:

- `block_may_improve`: some sink read of a local has an earlier instruction on
  the same place.
- `block_can_improve`: each scalar sink read's state is walked back across
  writes to disjoint places.

The policy feeds the surviving blocks through `ir_opt::optimize` as
surrogates, then runs `fip_reuse_pass`, `thread_tokens` and `validate` on
every function.

**Port.** The new package `packages/vesper-ir-opt-policy` holds:

- `src/screen.ves`: both screens on @vesper/place-ir's tables. It follows the
  Rust walk exactly, including where the block is kept so that the pass
  reports its own errors. Those cases are a read without state evidence, a
  walk through a state the block does not define, a cycle, and two steps from
  one state.
- `src/lib.ves` (`policy_words`): @vesper/ir-opt's `ir_opt_words`, with
  forwarding run only on the blocks the screen keeps.

@vesper/ir-opt already works per block, so a mask over block rows replaces the
Rust surrogates. Its only change is `forward_program_kept`, which is
`forward_program` with that mask. An empty mask keeps every block, so
`forward_program` itself is unchanged.

The Rust policy also has worker threads and an opt-in cache, and neither has a
counterpart here. The hybrid calls the pass once per compilation, and the
cache is off by default.

**Swap.** The hybrid's `opt` hook (`crates/vesper-hybrid/modules/ir_opt`) now
calls `policy_words` instead of `ir_opt_words`. When the words cannot encode a
program, or egglog would refuse a block, the hook falls back to Rust. That
fallback is now `ir_opt_policy::optimize`, the default compiler's path. The
`divergent-ir-opt` edit (build.rs and BUILD.bazel) follows the renamed call.

### Differential (`tests/self_host_ir_opt_policy.rs`)

| Test | Inputs | Differences |
|---|---|--:|
| `screen_matches_rust`: per block, the package's screen against `ir_opt_policy::block_can_improve` | 670 (every corpus world's native-path IR plus the IR mutants); 691,308 blocks, 45,966 kept | 0 |
| `policy_matches_rust`: `policy_words` plus Rust's `thread_tokens`/`validate` against `ir_opt_policy::optimize` (count or error, and the program's `Debug` text) | 28 (`IR_OPT_STEP=24` slice; egglog dominates) | 0 |
| `tests/self_host_ir_opt.rs` (unchanged, after the `forward_program_kept` hunk) | its slice | 0 |

### Speed (`policy_cost`, `port-compiler` profile, ABBA, 14 corpus worlds)

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Native optimizer policy: Rust's `ir_opt_policy::optimize` against the package with encoding, decoding and the Rust tail | 1.47 s | 1.70 s | **1.15x** |
| @vesper/ir-opt without the screen against with it | 1.92 s | 1.91 s | 0.99x |

The port is within the owner's 1.5x bar.

The screen saves little in Vesper. @vesper/ir-opt's per-block e-graph is
already cheap on blocks with no candidate read, while egglog was not; that gap
is what the Rust policy exists for. Most of the Vesper time is encoding,
decoding and the Rust tail (`thread_tokens`, `validate`).

`ir_opt_policy` belongs to the compiler (class `compiler`). Its Rust goes
with the Stage 2 retirement (SH6 lane 2). Until then the hybrid runs the
Vesper policy.

### Stage 1 guard

Part 1 leaves all of these untouched: `src/ir_opt.rs`, `src/ir_opt_policy.rs`,
`src/verified_codegen/**`, `src/ir.rs`, `bootstrap/` and
`packages/vesper-compiler/`. The only Rust hunk outside a test is the hybrid's
hook, which the default compiler never installs, so `bootstrap/stage1.lock`
cannot move.

## Part 2: `vesper-blake3-sys` (waits on lane 7)

The crate holds no logic. Its `build.rs` compiles the vendored BLAKE3 C
sources into `vesper_std_blake3`, the link name that `@vesper-std/hash-sys`'s
`.vsys` library already imports. The `.vsys` import exists. What is missing
is a toolchain step that builds a package's C.

Lane 7 (#1504) adds the `objects` field for assembly in its part 2. I asked
there (issue comment) for that field to cover what BLAKE3 needs:

- C sources.
- Per-file ISA flags.
- Per-target file sets and defines.
- Symbol renames.
- The same objects from `vesper-port-harness` for Rust hosts.

Once that lands, `vesper-fmt`, `vesper-flux` and `tests/std_hash.rs` drop the
crate and the crate is deleted.

## Part 3: what is left of `src/database` (blocked on the Vesper LSP server)

SH5 lane 16 ported the salsa invalidation itself to `@vesper/memo`, with 0
differences. What stays in `src/database.rs` is the query bodies:

- `parse_file`, `check_file`, `guarded_analysis`.
- The signature extraction, a canonical JSON of the Rust AST headers plus a
  BLAKE3 digest.
- The recovery queries.

All of them call the Rust parser and checker for `src/lsp`. They move when the
Vesper language server calls the checker ports in place of `@vesper/memo`'s
oracle, which SH5 lane 16's report names as left. Porting the bodies now would
mean a second Vesper front end inside this lane.

## Part 4: the tour playground (blocked: website and the wasm seed)

`src/browser_tour.rs` is the request glue of `website/wasm`, the whole Rust
compiler built to wasm:

- JSON request and response.
- Diagnostics with UTF-16 offsets.
- The entry checks around `check_source`, the evaluator, `vesper_fmt` and
  `vesper_highlight`.

Swapping it means editing `website/**`, which the lane rules forbid. It also
means a compiler that runs in browser wasm, which is the wasm seed (SH6 lane
2, #1397).

`src/source_analysis.rs` is not tour code. It holds compiler helpers
(`contains_function`, `needs_nan_canonicalization`, `closure_capabilities`)
that `native`, `ir_inline`, `runtime` and `pact_frontend` use, so it goes
with the compiler.

## New gaps

None in the compiler. The toolchain gap (a package's C objects) is lane 7's
part 2.
