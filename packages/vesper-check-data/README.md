# @vesper/check-data

The checker's data in Vesper, and the differential harness every SH2
checker port uses (self-hosting wave SH2, lane 4, #1176). The design comes
from the census ([report](../../docs/parallel-results/self-host-census.md)):
types are `u32` ids in a hash-consed arena, names are interned, and tables
are flat with sorted rows. Keep it: it is why the census samples ran at
0.34x to 0.87x of Rust's time. Rust stays the default checker; nothing here
replaces Rust code.

## Tables

| File | Rust | Vesper |
|---|---|---|
| `src/names.ves` | `String` names | `Names`: interned symbols, symbol 0 is `""` |
| `src/types.ves` | `ast::Type`, `Effects`, `substitute`, `numeric::default_type`, `Display`, `Debug` | `Types` arena (`TypeNode`, `RowNode`, `EffNode`), `display`, `debug_type`, `substitute`, `substitute_row`, `default_type` |
| `src/world.ves` | `check::World`: `adts`, `effects`, `types`, `partial_ops`, `overflow_ops`; `World::constructor`, `mode`, `field`; `vector::layout_words` | `World` (`Adt`, `Generic`, `Variant`, `Field`, `EffectRow`, `SpanRow`), `constructor`, `mode_of`, `field_step`, `vector_words`, `effectful_row`, `World::expr_type`, `is_partial`, `may_overflow` |
| `src/diag.ves` | `ast::Diagnostic` | `Diags`: rows over one text buffer, `digest` |
| `src/input.ves` | | `read_ctx`: the harness input decoder |
| `src/util.ves` | | `fold` (FNV-1a), table accessors, decimals |

Read a table through its accessors (`Types::node`, `World::adt`, ...), not
its raw fields, so lanes can change a layout without touching callers.

**Adding fields (lanes 5-14).** Each lane adds the `World` fields and the
decoder sections it needs in its own PR, below a comment naming the lane,
and appends their words at the end of `Enc::world` in
`tests/common/check_harness.rs` and of `read_world` in `src/input.ves`
(append only, so earlier lanes' inputs stay valid). Merge `main` right
before merging and re-run `tests/self_host_check_data.rs`.

**Constants.** A `const` cannot be imported from another module yet (gap 1
of the [report](../../docs/parallel-results/self-host-check-data.md)), so
each module declares the kind and mode constants it uses (`K_NAMED = 1`,
`M_AFFINE = 1`, `NONE32 = u32::MAX`, ...). Keep the copies equal.

## The differential harness

`tests/common/check_harness.rs` is a module, not a test binary. Include it
in a lane's test file:

```rust
#[path = "common/check_harness.rs"]
mod check_harness;
use check_harness::*;
```

What it gives you:

- `corpus()`: every checked corpus world (single files under
  `tests/fixtures` and `examples`, every Loom package under `packages` and
  `crates`), as `Case { name, module, world }`, loaded once per test
  binary. `CHECK_HARNESS_STRIDE=n` keeps every n-th file and package (for
  callgrind).
- `world_types(&world)`: every type the world mentions, once each.
- `Enc` and `encode(&world, |enc, tail| ...)`: the input. `Enc::world`
  writes names, types (post-order) and the world sections; the closure
  appends the task's own words (`enc.ty(&t)`, `enc.name(s)` intern as they
  go). The Vesper side calls `read_ctx(inout r, in b)` and reads the task
  from `r`.
- `check_data()` / `Package::load(dir)`: a checked package, and
  `.entry(name, optimize)` its JIT entry taking one `in Str`. An `Entry`
  returns a word (`.word`, for a digest), a `ChunkSeq[u64]` (`.words`), or
  text packed by `pack_text` in `src/lib.ves` (`.text`).
- `differential(label, rust, vesper)`: runs both over the corpus and
  counts the worlds whose texts differ, printing the first differing line.
  Prefer texts to digests while porting: a difference names its line.
- `fold`, `fold_bytes`, `diag_digest`: the digest folds, bit for bit the
  Vesper `fold` and `Diags::digest_row`. Compare every diagnostic's code,
  span, message, related labels and help.
- `ir_cases(m)` and `ir_mutant(&program, m)`: each world's lowered IR and
  up to five of the census's mutants (`CHECK_HARNESS_MUTANTS`), which make
  the IR checker's diagnostics fire.
- `abba(label, rust, vesper)`, `time(f)`, `speed()`: ABBA timing, best of
  six runs per side, gated on `CHECK_HARNESS_SPEED=1` in ignored tests.

A minimal lane test:

```rust
#[test]
fn my_module_matches_rust() {
    let pkg = Package::load("packages/vesper-check-mine");
    let f = pkg.entry("check_mine", true);
    let bad = differential(
        "mine",
        |c| rust_text(&c.world),
        |c| f.text(&encode(&c.world, |e, tail| my_task(e, tail, &c.world))),
    );
    assert_eq!(bad, 0);
}
```

Under Bazel the corpus is whatever the test target's data declares (the
root package's `tests/fixtures`, `examples` and `packages` files), so it is
smaller than Cargo's; Cargo runs the whole corpus on all four platforms.

## Tests

- `tests/self_host_check_data.rs`: `Display`, `Debug`, `default_type` and
  `substitute` of every corpus type, and the `World` lookups, against Rust
  at 0 differences; the JIT with the optimizer off agrees with it on.
- `tests/self_host_check_harness.rs`: the corpus loads, the decoder rebuilds
  every table, answers depend on the input, the IR mutants change programs.
