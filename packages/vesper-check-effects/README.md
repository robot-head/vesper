# @vesper/check-effects

The capability effect rules of `src/capability_effects.rs` in Vesper
(self-hosting wave SH2, lane 8, #1180): `ConstTime`, `Realtime`,
`DetFloat`, `ConstDenorm`, the realtime bus `RtBus`, and the secret
discipline with its interprocedural facts (secret-parameter inference,
inferred `inout` outputs, secret resumes). It builds on
[`@vesper/check-data`](../vesper-check-data/README.md) and keeps the census
design: types are `u32` ids, names are interned symbols, and every table is
flat. Rust stays the default checker; nothing here replaces Rust code.

| File | Rust | Vesper |
|---|---|---|
| `src/prog.ves` | `world.functions` (with bodies), `world.effects`, `world.calls` | `Prog`: an index graph of expressions and patterns over one word pool, function, parameter, effect and call tables, `read_prog` |
| `src/keys.ves` | `Summaries`, `BTreeSet`s of facts | `Keys` (sorted `u64` sets), `Sums` |
| `src/walker.ves` | `Walker` and its helpers | the walk: scopes as one `Local` stack with marks, the call graph as a flat edge list, `secret_type` memoised by type id |
| `src/check.ves` | `summarise`, `check_world`, `realtime_recursion` | the fixpoint, both walks, the cycle check, a stable merge sort and dedup of the diagnostics |
| `src/lib.ves` | | the entry `check_effects(in input: Str)` |

## Input

The harness world (`Enc::world`) followed by the task words of
`encode_task` in `tests/self_host_check_effects.rs`: the expression,
pattern and pool tables, then every function (key, name, span, result
type, declared row, the row `float_math::carried_effects` gives it, body
and parameters), every effect declaration with its operations, and the
call instantiations. `prog.ves` documents the node layout.

## Tests

`tests/self_host_check_effects.rs` compares one text line per diagnostic
(code, span, message, help and related labels) with
`capability_effects::check_world`:

- the corpus (every clean corpus world);
- the capability checker fixtures and the float, vector and verifier
  fixtures, whatever their other diagnostics; every diagnostic code of the
  module fires there;
- five capability mutants of all of those worlds (`ConstTime`, `Realtime`,
  `DetFloat`, `DetFloat + ConstDenorm`, all four added to every function's
  row);
- the JIT with the optimizer off.

The ignored `effects_abba` and `effects_callgrind_side` tests measure it
against Rust.
