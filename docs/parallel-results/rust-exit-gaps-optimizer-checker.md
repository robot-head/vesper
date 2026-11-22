# Rust-exit gap wave 1, lane 4: the optimizer's checker within 1.5x

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1501. Base: `main` at `66255e17`. Lane prompt:
[`docs/agent-prompts/rust-exit-gaps-1/04-optimizer-checker.md`](../agent-prompts/rust-exit-gaps-1/04-optimizer-checker.md).

SH3 lane 10 (#1249) ported the AST arithmetic optimizer to
`@vesper/optimize`. Its certificate checker ran at 1.94x of Rust (callgrind
2.06x), above the owner's 1.5x bar. This lane brings the checker to 1.12x
under the flip gate and adds `optimize` to the gate, which passes. The
changes are in the port only; no compiler code changed.

## Result

`python3 scripts/ci/flip_gate.py optimize` on a 4-vCPU cloud host, release
build, `RUSTFLAGS=-C target-cpu=native`:

```text
flip-gate optimize: PASS (bar: 0 differences, every test, 95% upper <= 1.50x) | tests 2/2 pass | differential 0 differences | ABBA vesper/rust checker 1.12x [1.00x, 1.19x] n=12; producer 0.70x [0.64x, 0.74x] n=12; pass 0.79x [0.70x, 0.83x] n=12 | RUSTFLAGS=-C target-cpu=native
| `optimize` (PASS) | G1-4 | 0 differences; tests 2/2 pass | checker 1.12x [1.00x, 1.19x] n=12; producer 0.70x [0.64x, 0.74x] n=12; pass 0.79x [0.70x, 0.83x] n=12 | 2,867 | |
```

| Workload | Before (`66255e17`) | After |
| --- | --: | --: |
| `checker`: checker alone, decode subtracted | 1.58x [1.50x, 1.73x] | 1.12x [1.00x, 1.19x] |
| `producer`: producer alone, decode subtracted | | 0.70x [0.64x, 0.74x] |
| `pass`: producer and checker together, with decode | | 0.79x [0.70x, 0.83x] |

The "before" row is the same gate harness run on the `main` package
sources (`--only abba`, checker workload). On `main` the checker had already
come down from part 1's 1.94x to about 1.6x, through compiler work that
landed since.

Callgrind instructions, one round over the 171 proposals of the fixtures
and generated islands (`OPTIMIZE_SKIP_CORPUS=1`, `OPTIMIZE_SIDE=...`,
`--toggle-collect='*measured_side*'`; no `target-cpu=native`):

| | Rust | Vesper before | Vesper after |
| --- | --: | --: | --: |
| checker with its decode | 3.72 M | 13.76 M | 11.43 M |
| decode only | | 7.95 M | 7.72 M |
| checker alone | 3.72 M | 5.81 M (1.56x) | 3.71 M (1.00x) |

## The gate

`scripts/ci/flip-gate/optimize.toml` (new). A compiler module: its Rust
(`src/optimize/certificate.rs`, `src/optimize/witness.rs`, 2,867 lines) goes
with the Rust compiler's retirement (SH6 lane 2), not with this flip.

- **Tests:** `self_host_optimize` and `self_host_optimize_search`, release.
- **Differential:** `mutated_proposals_are_refused_for_the_same_reason`
  (every genuine proposal and its 59k mutants: answer and spent budget) and
  `every_visit_is_proposed_like_rust` (every sampled corpus visit:
  proposal, certificate, budget). Both pass, so 0 differences.
- **ABBA** (each harness does its own Rust, Vesper, Vesper, Rust epochs and
  prints `flip-gate pairs`; `OPTIMIZE_GATE_EPOCHS`, default 12):
  - `checker` (`tests/self_host_optimize.rs` `flip_gate`): `verify_spent` on
    prepared copies against the Vesper checker on the same input, with the
    decode-only entry on that input, timed in the same epoch, subtracted.
  - `producer` (`tests/self_host_optimize_search.rs` `flip_gate`):
    `propose_rust` against the Vesper producer, decode subtracted the same
    way.
  - `pass`: Rust's `propose_rust` on every visit plus `verify_spent` on
    every candidate it proposed, against the Vesper producer and checker on
    the same cases, both with their decode. This is the optimizer as it runs.
  - `checker-hook` (same harness, not in the gate): the checker with its
    decode, the cost of the hybrid's install gate per proposal today. Like
    bounds hoisting's `hook`, it measures the word boundary, which goes away
    once the producer hands the checker its proposal in Vesper.

## Where the instructions went

The JIT code has no symbols, so the profile used a local, uncommitted aid
in `native::WordFunction::compile_with` that wrote each function's address
and code bytes under `VESPER_JIT_MAP_LOCAL`. Callgrind's per-instruction
costs (`--dump-instr=yes`) were then mapped onto those functions and
disassembled.

Checker alone on `main` (5.81 M), against Rust (3.72 M):

| Where | Vesper | Rust |
| --- | --: | --: |
| `bind_source` (with the inlined term interning and type lookups) | 1.48 M | 0.58 M + 0.24 M `BTreeMap::get` |
| `verify`, `validate_terms`, `compare_input`, `compare_output`, `check_rule`, `call_parts` | 2.18 M | 0.86 M |
| `reserve` (budget growth model, not inlined) | 0.39 M | in `try_reserve_exact` |
| the JIT runtime's sequence tracking (`HashSet` of live headers, SipHash) | 0.73 M | |
| `malloc`, `free`, `realloc` | 0.30 M | 1.53 M |

What made those instructions:

1. **Overflow checks.** `checked_mul_u64` lowers to a 128-bit
   multiplication emulated in 32-bit halves, and every `checked_add_u64`
   builds an `Option` and materializes its flag as a `bool`. `reserve`
   multiplied twice per growth, and `work`, `metadata` and `names` (the
   budget charges, several per node) each did a checked add.
2. **The interning search** copied each candidate term (a 56-byte
   `TermRow`) out of the sequence to compare it, and charged one unit of
   work per compared term through the budget in memory.
3. **Per-case buffers.** Each check created eight sequences (the bound
   source's three, two `bind_source` stacks, the shapes and two compare
   stacks). Under the JIT harness every one is a `malloc` plus a `HashSet`
   insert and remove in the native runtime.
4. **Whole-row copies.** `try_get` of a `TermRow` (7 words) or a `ProofRow`
   (12 words) copies the row through the `Option` and again into the
   binding, about 45 instructions per term read (see Gaps).

## The fix

All in `packages/vesper-optimize` (`budget.ves`, `witness.ves`, `lib.ves`):

- **Budget charges without `Option`** (`budget.ves`): `work`, `metadata`
  and `names` compare the amount with the room left (`max - spent`), which
  cannot overflow and refuses exactly when the checked sum would exceed the
  limit (or overflow). `reserve`'s common case (room left) is a two-compare
  early exit; its growth uses plain arithmetic when every operand is below
  2^32 (sizes below 2^16), where no step can overflow, and the checked path
  otherwise (`grow_checked`, the old code).
- **Byte counts** (`witness.ves` `bytes_of`): `n * S_*` is a plain
  multiplication below 2^48 and the checked one above. It returns an odd
  sentinel on overflow, which no product of the (even) sizes equals.
- **Interning by key** (`intern_term`): the bound source keeps a `keys`
  column (tag, operation and children packed in one word; every bound term
  has the source's kind) and a `consts` column beside `terms`, and the
  search runs over them. The units of work are charged once
  (`work_units`), which leaves the counter where one charge per compared
  term leaves it: unchanged when the first charge refuses, at `max_work`
  when a later one does.
- **Reused buffers:** `bind_source`'s stacks live in `Bound`, and
  `verify` takes a `Scratch` (the bound source, the shapes and both compare
  stacks) that `run_cases` keeps across its cases.
- `validate_terms` iterates its terms with `for` (no `Option` per term).
- `saturating_add` compares instead of building an `Option`.

The checks run in the same order and charge the same amounts, so every
answer and every spent counter is unchanged: the 59,366 mutants (every
`WitnessError` but `Allocation`, budgets cut at twelve points) and every
sampled corpus visit give Rust's lines exactly.

The producer (`search.ves`) uses the same budget functions and got faster
with them (0.68x in part 2, 0.70x under the gate's native flags here).

## Stage 1 guard

No guarded path changed (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/stage1*`,
`packages/vesper-compiler/**`). The change reaches no compiler that Stage 1
builds: `@vesper/optimize` is linked only by the hybrid's `optimize` module.

## Gaps found

Minimal repros; each compiles today, the cost is in the generated code.

1. **`checked_mul_u64` lowers to a 32-bit-half 128-bit multiply** instead of
   one `mul` and the overflow flag (about 40 instructions):
   `fn f(a: u64, b: u64) -> u64 { match checked_mul_u64(a, b) { Option::Some(v) => v, Option::None => 0 } }`.
2. **`checked_add_u64`'s flag is materialized** as a `bool` and tested
   again (`mov 1; jmp; test; jne`) instead of branching on the carry; the
   `bool` materialization item of the native code size thread (#1426).
3. **`try_get` of an aggregate element copies the row twice** (through the
   `Option`, then into the binding), even when one field is read:
   `copy struct R { a: u64, b: u64, c: u64, d: u64 } fn f(in s: ChunkSeq[R], i: u64) -> u64 { match s.try_get(i) { Option::Some(r) => r.a, Option::None => 0 } }`.
   Gap lane 1 (`Option` results in registers, #1498) owns this.
4. **Every scalar field takes a word.** `ProofRow` (eight `u32`, a `u8` and
   a `bool`) is 96 bytes, Rust's `Equality` 36, so every row copy moves 12
   words.
5. **The JIT harness's runtime tracks live sequences in a SipHash
   `HashSet`** (`src/seq/native_runtime.rs`), so each sequence costs a hash
   insert and remove there that the AOT runtime the hybrid uses (a counter)
   does not. Port speed tests run on the JIT, so they overstate allocation
   costs.

## Left

- The hybrid install gate's word boundary: decode is about twice the
  checker (`checker-hook` about 2.6x of Rust's checker alone). It goes away
  when the producer and checker meet in Vesper; until then, reusing the
  `Tree` across cases would cut it (`@vesper/ast` has no arena reset).
- With gaps 3 and 4 closed, `verify`'s proof loop and `compare_*` would
  lose most of their remaining copies; a packed `TermRow`/`ProofRow` in the
  port is the workaround if they stay.
- The hybrid differential (`crates/vesper-hybrid/tests/differential.rs`)
  runs on `main` only; the package's differentials above cover the same
  answers.
