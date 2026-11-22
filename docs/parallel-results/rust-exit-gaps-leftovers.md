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

## Part 2: `vesper-blake3-sys` deleted

The crate held no logic: its `build.rs` compiled the vendored BLAKE3 C
sources into `vesper_std_blake3`, the link name of `@vesper-std/hash-sys`'s
`.vsys` library. That build is now part of the package:

- `[package] objects` (lane 7, #1548) also takes C sources (`.c`). Unix
  compiles them with the linking C driver at `-O2`; Windows with `cl.exe`,
  else `clang`. The owner's rules stay: objects need a `link = "static"`
  `.vsys` library of the same package, link only when an executable calls
  it, and `jit`/`run`/`test` refuse them.
- An entry takes no flags, as agreed with lane 7. Each wrapper in
  `hash/sys/c/b3-*.c` includes one upstream file after
  `vesper_b3_rename.h` (the 31 `blake3_*` to `vesper_b3_*` renames the
  crate passed with `-D`). The x86-64 kernels set their ISA with a
  `target` pragma, so code the CPU may lack is still reached only through
  the run-time dispatch. Per-target names (`b3-sse2-@ARCH@.c`,
  `b3-neon-@ARCH@.c`) pick the kernels a target builds.
- The library moved from inline `// @export vsys` blocks (one in hash-sys
  and a copy in each port) to `hash/sys/blake3.vsys`, named by the
  manifest's `[package] vsys`, so every library build that loads hash-sys
  imports it.
- `vesper-port-harness` compiles the `[package] objects` of a port's
  package closure with `cc` into the port's own archive. `vesper-fmt`,
  `vesper-flux` and `vesper-loom-cas-port` link BLAKE3 that way, and their
  bindings no longer name a Rust `blake3_shim`. Bazel passes
  `//:std_hash_sys_package` (which now carries the `.vsys` file and the C
  sources) to the build scripts.
- `tests/std_hash.rs` builds the `.vsys` driver as an executable (the
  official vectors and every split, checked against the `blake3` crate)
  in place of the JIT run over the crate's symbols, and checks that
  `vesper run` points to `vesper build`. The evaluator test is unchanged.
  The crate's refusal oracle moved there too: `cc` builds the host's
  `objects` with a generated C driver, and a 31- or 33-byte key, nine
  invalid UTF-8 contexts (each also leaving the one-shot output's `7`
  sentinel as it was) and eight valid ones are checked against the crate.
- Bazel does not lay path dependencies out beside the crate, so the build
  scripts also get hash-sys's manifest in `VESPER_PORT_OBJECT_PACKAGES`.

Checked: gcc and clang give BLAKE3("abc") =
`6437b3ac…5bd9d85` and the same 100 KB digest, with no unrenamed global
symbols.

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
