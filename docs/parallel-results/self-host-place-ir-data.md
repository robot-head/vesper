# Self-hosting SH3 lane 1: Place IR data, words and the IR swap point

Claim #1240. Stacks on #1232 (SH2 lane 5, the IR encoder and
`@vesper/ir-check`). Prompt: `docs/agent-prompts/self-host-sh3/01-place-ir-data.md`.
Rust stays the default compiler; nothing here changes a default build or
deletes Rust code.

## What changed (part 1)

- `packages/vesper-place-ir` (`@vesper/place-ir`): `ir::Program` as flat
  Vesper tables. Functions, blocks and instructions are rows over one operand
  pool of words; each word has a tag (raw number, name symbol, optional name,
  type id, path, payload pool entry, value, block). Types are
  `@vesper/check-data` ids and names its interned symbols (the census
  design; no `Box` trees). `read_program` reads the program section after
  `read_ctx`; `dump_program` writes the canonical dump; `put_program` writes
  names, types and the program back as words.
- `tests/common/ir_words.rs`: the Rust half. `program_input` (world and
  program as one input), `decode_program` (words back into a `Program`),
  `dump` (the same canonical dump) and the format's documentation. It builds
  on #1232's `check_words::Enc` for names, types and the world (imported, not
  moved, since #1232 is still open).
- `tests/self_host_place_ir.rs`: the round trips below, over every corpus
  world at every native-path pass boundary, plus an ignored cost
  measurement.
- `src/ir_swap.rs`: the swap point. `IrPasses` holds optional `lower`,
  `opt`, `inline` and `hoist` hooks; src/native.rs, src/native_export.rs and
  src/native_session.rs call `ir_swap::{lower, optimize, inline, hoist}`
  instead of the Rust passes. Nothing installs a hook in the default
  compiler, so every call runs the Rust pass. `ffi_lowering::drops` gains
  `insert_drops`, so a `lower` hook's program still gets the FFI handle drops
  from Rust.

### Payloads

`SeqOp`, `VectorOp`, `TiledOp`, `FipOp` and the expressions of
`Op::Closure`/`Op::Handle` stay Rust values in a payload pool. The words
carry the pool index; the Vesper pool row carries the kind and, for an
operation, its `Debug` text as a name, so a pass can tell `Get` from `Push`.
A Vesper pass can copy, move and drop pool references but cannot make new
payloads. A lane that must make one (bounds hoisting makes
`SeqOp::Hoisted` and `BoundsGuard`) adds a structured pool kind. Literals,
patterns, arguments, places and the `fip` side tables are fully structured.

## Results

Every corpus world (288, Cargo corpus) at six boundaries: lowering, `ir_opt`,
the inliner, drop elaboration and hoisting with optimize on, and drop
elaboration with optimize off (1,728 programs).

| Check | Differences |
|---|---|
| Rust round trip (`Program` -> words -> `Program`, `Debug`-equal) | 0 of 1,728 |
| Vesper reader (Vesper dump = Rust dump, 10.2 M lines) | 0 of 1,728 |
| Vesper writer (Rust -> Vesper tables -> words -> `Program`, `Debug`-equal) | 0 of 1,728 |
| Programs the format cannot encode (literal types over 64 bits) | 0 |

Cost of a swap over the 288 lowered programs (optimized test build, best of
five, one machine):

| Step | Time | vs `ir::lower_world` |
|---|---|---|
| `ir::lower_world` (Rust) | 785 ms | 1.00x |
| Encode world + program (Rust, 86 MB of input) | 631 ms | 0.80x |
| Vesper read + write | 98 ms | 0.13x |
| Decode (Rust) | 254 ms | 0.32x |

The encoding dominates: most of the input is the world section (every
expression type) that check-data's decoder reads first. An IR lane should
subtract encode + decode (about 1.1x of lowering here) from its pass time,
and the ABI v4 tree input (docs/parallel-results/self-host-tree-abi.md) is
how that cost goes away.

The tests default to every fourth world (`PLACE_IR_STEP`, 432 programs; the
pass harness test takes every sixteenth of those),
because running the passes over the whole corpus takes over half an hour in a
debug build. `PLACE_IR_STEP=1` runs every world (the numbers above).

## Stage 1 guard

`src/ir.rs`, `ir_opt::optimize`, src/verified_codegen/**, lean/**,
bootstrap/** and packages/vesper-compiler/** are unchanged. The verified
backend (src/verified_codegen/coverage.rs) keeps its own
`ir::lower_world` + `ir_opt::optimize` and never calls `ir_swap`; the
evaluator (src/runtime.rs) still calls `ffi_lowering::drops::lower_world`
directly. The bootstrap-stage1 workflow runs on this branch; its digests are
in the PR.

## Compiler gaps

None new. The package compiled and matched on the first run. Known gaps it
works around:

- Constants cannot be imported across packages yet (check-data gap 1;
  #1239 is open), so the tag and opcode constants are copies of ir_words.rs.
- check-data's type literal is a sign and a 64-bit magnitude, so a program
  with a wider literal type would stay in Rust (none in the corpus).

## Part 2: the pass harness and the hybrid identity hooks

- `ir_words::compare_pass(world, input, rust, vesper)`: the harness lanes
  9-13 call with their pass. It runs the Rust pass and the Vesper pass (input
  bytes to `put_program` words) on the same input and compares the outputs'
  canonical dumps, `Debug` equality and, below `IR_CHECK_CAP` (20,000)
  instructions, `ir_check::validate` on each. (One inlined package took the
  IR check 247 s, hence the cap.) `through_vesper` and `program_only_input`
  are the building blocks (a pass without a world, like hoisting, gets an
  empty world section).
- `tests/self_host_place_ir.rs` `pass_harness_agrees_on_identity_and_catches_a_pass`:
  the identity on both sides agrees, and `ir_opt` against the identity is
  caught (it differs on the worlds it changes).
- `tests/self_host_place_ir_jit.rs`: every corpus `main` through the native
  JIT with optimize on and off, in two child processes: Rust passes, and
  `ir_swap` identity hooks that send the program through `@vesper/place-ir`
  after every pass (lowering, `ir_opt`, the inliner, hoisting). 352 runs
  (176 mains, 286 returning a value) give the same results.
- The hybrid build installs the same identity hooks
  (`crates/vesper-hybrid/modules/place_ir`, `VESPER_IR_PASSES` in
  src/hybrid.rs). The hybrid differential harness (538 files x check / IR /
  object) finds 0 differences against the default compiler, and
  `VESPER_HYBRID_STATS=1` counts the round trips. Lanes 9-13 replace a hook
  with their Vesper pass. No divergent feature was added for this module:
  the pass harness's `ir_opt` check covers that the comparison can fail.

## What is left

- Callgrind numbers for the round trip.
- Structured payload kinds when a lane needs to make new operations.
