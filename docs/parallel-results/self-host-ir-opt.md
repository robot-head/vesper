# Self-hosting SH3 lane 9: `ir_opt` in Vesper

Claim #1248. Package `packages/vesper-ir-opt` (`@vesper/ir-opt`), test
`tests/self_host_ir_opt.rs`. Base: `main` at `be43fc4` (with `@vesper/egraph`,
#1273) plus SH3 lane 1's part 1 (#1259, head `8ac0f94`), which this PR stacks
on. Rust stays the default; `ir_opt::optimize` and the verified pipeline are
unchanged.

## What changed

- `@vesper/ir-opt` ports all of `src/ir_opt.rs` (638 lines) onto
  `@vesper/place-ir`'s tables:
  - **Memory forwarding** (`forward.ves`): per block, the places observed by
    reads of locals, writes and plain binds (numbered by first appearance,
    keyed by their dotted text as the Rust `PlaceGraph` keys them), the
    `disjoint` pairs the place graph allows, the state terms per token
    (`Start`, `Write` for a scalar `=` write or plain scalar bind, `Barrier`
    otherwise), one `(union v (Read s p))` per scalar sink read, twelve rounds
    of `@vesper/egraph`'s `run_rounds` with its memory rules, then each read
    replaced by the first earlier scalar of its type in its class (the
    scalar-only replacement rule), aliases resolved through chains, replaced
    reads dropped and every operand and terminator value rewritten.
  - **`@fip` constructor reuse** (`reuse.ves`): `fip_reuse_pass` whole:
    reachability, Kahn order, joins (identical slots, then same-key merges
    into join slots), ranks, `Box::new` reuse, caller-slot calls in tail
    position or on a reuse slot, `flows_to_return`, and the trailer's call
    and join slot tables rewritten.
  - **`FIP_RULES`**, the second egglog program, which `@vesper/egraph` does
    not cover: it unions `AllocRef a` and `SlotRef s` whose `(key, rank)` tags
    agree. Each object has one tag and a slot's rank is unique per key in its
    block, so the e-classes are exactly the tag classes, and a pairing is
    confirmed iff the allocation's tag equals its slot's. The port compares
    the tags; no relation join is needed.
- `BoxReuse` payloads are new pool entries, which `@vesper/place-ir` cannot
  make: the entry appends pool rows and returns their `(bind dest, slot
  index, size)` ahead of the program words; the Rust side extends the pool
  before decoding.
- What the world answers for the reuse pass (`fip::active`, `box_inner`,
  `pattern_box_slots`, `slot_key`, `caller_slot_functions`) is written by the
  harness as facts after the program, the way the inliner lane does;
  `slot_key` is `pub(crate)`, so the test transcribes it. `slot_key` errors
  come back as indices into the harness's messages.
- `ir::thread_tokens` and `ir_check::validate`, the end of `optimize`, stay
  with the caller (the test runs the Rust ones on the decoded program).
- BUILD.bazel: data and the `long` size for `test/self_host_ir_opt`.

## Differential

Inputs: every corpus world's native-path IR
(`ffi_lowering::drops::lower_world`, what `ir_swap::optimize` receives in
src/native.rs), one census IR mutant of each (`CHECK_HARNESS_MUTANTS`), and,
because the corpus has no `@fip` code, the fip checker's 39 test sources
(`packages/vesper-check-fip/tests/cases.txt`) that check clean, each with its
five mutants. Compared: the result (the count `optimize` returns, or the
error) and the output `Program`'s `Debug` text, after Rust's
`thread_tokens` + `validate` on the decoded program.

| Test | Inputs | Differ | Replaced reads + reuse matches | Errors (all matched) |
|---|--:|--:|--:|--:|
| `optimize_matches_rust` (whole pass) | 666 | 0 | 10,066 | 113 (3 egglog refusals) |
| `fip_reuse_matches_rust` (`fip_reuse_pass` alone) | 666 | 0 | 70 | 0 |

The errors come from mutants and are compared as text, except the 3 egglog
refusals of a block (an undefined or twice-defined state token), compared by
the prefix `effect equality saturation:` only, since egglog's own message is
not reproduced.

The harness fails when the port is wrong: with 1 round instead of 12 and
the candidate type filter removed, 14 of 111 inputs (`IR_OPT_STEP=6`)
differ.

Run time: about 9 minutes per mode for every input in the port-compiler
profile, nearly all in Rust's egglog. The test runs every 24th corpus input by
default (`IR_OPT_STEP`, 1 for all; the `@fip` cases always run), and Bazel
gives it the `eternal` size: CI builds tests unoptimized, where every 12th
input took 34 minutes.

## Numbers

ABBA, port-compiler profile, Linux x86-64, every 4th corpus world (74
worlds that optimize cleanly), best of six:

| | Time | vs Rust `optimize` |
|---|--:|--:|
| Rust `ir_opt::optimize` (egglog per block, then thread + validate) | 124.4 s | 1x |
| Rust's thread + validate alone | 22.9 s | |
| Rust pass alone (difference) | 101.5 s | 1x |
| Vesper read + pass + write | 0.37 s | 0.0036x of the pass |
| encode 0.33 s + Vesper 0.37 s + decode 0.19 s | 0.89 s | 0.0087x of the pass |
| Vesper whole (encode, pass, decode, Rust thread + validate) | 22.7 s | 0.18x |

Rust's cost is egglog building and parsing one program per block; the port
builds the terms directly (the e-graph lane measured its memory run at
0.002x of egglog). Callgrind was not run: under valgrind the JIT compile of
the package and egglog over the corpus take hours; the wall ratio is three
orders of magnitude.

## Stage 1 guard

No file under src/ changed: `ir_opt::optimize`, `src/ir.rs`,
src/verified_codegen/**, lean/**, bootstrap/** and packages/vesper-compiler/**
are untouched, and nothing is installed into `ir_swap` yet, so the verified
artifacts cannot change. The bootstrap-stage1 workflow runs on the branch.

## Compiler gaps

None blocked the port; it compiled as written. Workarounds worth a gap entry:

- **No new payloads in `@vesper/place-ir`.** A pass that makes a `FipOp`
  needs a side channel (here: appended pool rows plus their fields). A
  structured `fip` pool kind would remove it (lane 12 needs the same for
  `SeqOp::Hoisted`).
- **`fip::slot_key` is `pub(crate)`.** The harness transcribes it to write
  facts; a `pub` wrapper (outside this lane's paths) would remove the copy.
- **The Vesper package assembler does not resolve imported constants.**
  `@vesper/check-names-packages` (SH2 lane 6) reports E0111 for a `pub
  const` imported from another module (`use self::view::{NONE}`) or another
  package's root (`use @vesper/egraph::{V_REF}`), which the Rust assembler
  accepts; tests/self_host_check_names_packages.rs assembles every corpus
  package, so each file of this package keeps its own copies.
- **No sort in the package.** Call slots and join blocks are ordered by
  selection over short lists; `@vesper-std/sort` would do for longer ones.

## Left

- **Hybrid swap** (part 2): the `opt` hook in `crates/vesper-hybrid` (module
  `ir_opt`) needs the hybrid's Place IR encode and decode, which SH3 lane 1's
  part 2 adds after #1259 merges. Then the full suite on the hybrid build and
  the scoreboard row.
- `thread_tokens` and `validate` in Vesper: lane 13 ports `thread_tokens`;
  the IR check's Vesper port is SH2 lane 5's (#1232).
- The reuse facts from Vesper (`@vesper/check-layout` and
  `@vesper/check-fip` hold the layout and `Box` questions).
