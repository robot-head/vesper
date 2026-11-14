# Source Strata: lexical regions and lifetime checking (§15)

Wave seven, lane 29. Claim: #352. Base: `283c811` (descendant of the required
baseline `c025b9a`). Branch: `claude/project-thread-jlrqqc`.

## What was built

Vesper source now has lexical regions. A `region r { ... }` block binds the
lexical capability `r : Cap_linear[Region]` for its body, region-scoped
allocation goes through that capability, the checker enforces the §15.2
lifetime and alias rules, and accepted programs run on the evaluator and on
the native JIT, both backed by `vesper-alloc` regions.

```text
region r { body }          // bound: 1 MiB
region r(4096) { body }    // explicit bound in bytes, 1 B ..= 1 GiB

r.box(value)      -> RegionBox[r, T]   allocate in r
r.get(b)          -> T                 read a copy payload
r.set(b, value)   -> Unit              overwrite a copy payload in place
r.take(b)         -> T                 move an affine/linear payload out
r.checkpoint()    -> Watermark[r]      linear watermark
r.rewind(mark)    -> Unit              discard every allocation after mark
r.commit(mark)    -> Unit              drop a watermark without rewinding
r.used()          -> u64               bytes allocated in r now

fn alloc(x: i64) -> RegionBox[r, i64] eff [r: Region] { r.box(x) }
fn read(b: RegionBox[r, i64]) -> i64 eff [r: Region] { r.get(b) }
```

`region` is a contextual keyword (only `region <name> {` or
`region <name>(` starts a block), so existing identifiers named `region`
still parse. `vesper fmt` formats the new syntax with no change.

### Representation (no new expression, type or IR schema)

* The parser encodes a block as `ExprKind::Handle` of the built-in linear
  effect `Region`, labelled `r`, with no operation clauses and a return
  clause binding the reserved name `$region` (or `$region:<bytes>`), which no
  source text can spell. This is the source form of `Region::scoped(λr. e)`.
* `RegionBox[r, T]` and `Watermark[r]` are `Type::Named` with the capability
  itself as first argument, `eff [r: Region]` as a `Type::Row`. Every existing
  capability rule (escape through results and closures, renaming at calls,
  row discharge) applies to them unchanged.
* Region operations are ordinary labelled effect calls in the Place IR
  (`Op::Call { callee: "Region::box", capability: Some("r") }`). No IR
  operation was added.
* The `Region` effect declaration is added to a module's World only when the
  module uses regions, so every existing program checks to the same World.
  A source declaration of an effect named `Region` is rejected (E0797).

### Checker rules (`src/regions/check.rs`, hooks in `src/check.rs`)

| Rule (§15.2) | Check | Code |
| --- | --- | --- |
| [T-RegionAlloc]: allocation needs `r` in Σ | `r.op` only through an active region capability; the use is recorded in the row, so a function allocating in a caller's region must declare `eff [r: Region]` | E0401 |
| [T-RegionScope]: `r ∉ free_caps(τ_out)` | a block whose result type mentions its own capability (boxes, watermarks, tuples of them, closures capturing `r`) is rejected | E0791 |
| No use after the region ends | follows from the above: once the block ends no expression can name `r`, and every access needs `r` | E0201 |
| Signatures name only received regions | `RegionBox[r, T]` / `Watermark[r]` must name a capability in scope: a function's own `eff [r: Region]` or an enclosing block; data declarations cannot hold region references | E0797 |
| Region branding | a box or watermark is accepted only by the capability of the region that made it, including through function parameters, whose region labels are instantiated per call | E0792 |
| Substructural modal preservation | `RegionBox[r, T]` has the mode of `T`: copy payloads give copy handles (free intra-region aliasing, `get`/`set`), affine/linear payloads give unique handles consumed by `take` | E0793, E0794, E0303 |
| Linear payloads block region exit | a linear box is an owned linear binding: the block cannot end until it is taken and consumed | E0302 |
| Linear payloads block rewind | `rewind` is rejected while a linear box of the same region is live | E0795 |
| Use after rewind | a box bound after `let m = r.checkpoint();` in the same block is dead after `r.rewind(m);` | E0796 |
| Watermarks are linear | must be rewound or committed exactly once | E0302, E0303 |
| Reserved forms | hand-written `handle r: Region`, `Region::op(...)` path calls, unknown operations and wrong arity | E0797, E0790 |

A pure function may use a local scratch region: the block discharges `r` from
the row, so `fn scratch(x: i64) -> i64 eff [] { region r(64) { ... } }`
checks (§15.2 "Vesper Loom Pure Scratchpads").

### Evaluator (`src/regions/eval.rs`, hooks in `src/runtime.rs`)

A block's region is a `vesper_alloc::Region` opened on its first operation
(keyed by the block's runtime capability identity) and closed when the
block's handler frame returns. Payloads are evaluator values stored in region
memory with `alloc_value`; handles are `(region, index)` pairs checked by the
allocator's region identity and serial, so a stale or foreign handle is a
runtime error, never a read of reclaimed memory. The evaluator's byte bound is
scaled by `size_of::<Value>() / 8` so a block holds as many word payloads as
on the native backend, and `used()` is reported in the same word units, so
both backends return the same count. A region left by an abort is finalized when the
`Runtime` is dropped.

### Native (`src/regions/native.rs`, `src/regions/native_runtime.rs`)

Region blocks lower to runtime calls: `vesper_region_open(capacity)` on
entry returns a fresh run-time region id, which the block keeps in a variable
of its handler; every operation passes that id to `vesper_region_<op>(id,
...)`, and the normal exit calls `vesper_region_close(id)`. Because the id is
lexical evidence rather than a lookup by block, recursion gets one region per
activation and an abort out of an inner activation of the same block cannot
redirect the outer activation's operations. Payloads are machine
words (integers, `bool`, `Unit`, `f64` as bit patterns) held in a per-thread
table of `vesper_alloc::Region<i64>`; no `unsafe` code is involved. A failing
call records a typed fault; the JIT driver calls `native_runtime::finish()`
after the entry returns, which reports the fault as an error and closes any
region an abort left open. Aggregate payloads and object (AOT) output are
rejected with typed errors.

## Shared-file hunks

All additive; each is explained here and kept small.

* `crates/vesper-syntax/src/parser.rs`: `region` block parsing and the public
  `REGION_EFFECT`, `REGION_MARKER`, `MAX_REGION_CAPACITY` constants.
* `src/check.rs` (lane 29 owns it this wave): region prelude and reserved
  name in `World::collect`, signature normalization, `RegionBox` mode,
  `valid_type`, `expect_type` diagnostics, `let` annotations, the label-call
  and path-call hooks, region blocks in `handle`, the post-check rewind rule,
  `Row`/`Row` compatibility, `Row` capability renaming, and renaming region
  parameter types in the post-argument constraint check.
* `src/runtime.rs` (lane 28's evaluator): one module include, one field, the
  `perform` dispatch and the handler-return close.
* `src/native.rs` (lane 23): one module include, region handles in
  `scalar()`, JIT symbol registration, `finish()` after the JIT call, the
  enter/exit calls around a handler body, and the `perform` dispatch.
* `src/layout.rs`: region handles are one-word scalars.
* `src/lib.rs`: `pub mod regions`.
* Root patch (own commit): `vesper-alloc` as a dependency of the root
  package, and the regenerated `Cargo.lock`. No new workspace member.

## Checks

Commands run on the branch (Linux x86_64, local Rust 1.97 and 1.100):

* `cargo test --test regions` — 33 passed: positive programs run on the
  evaluator and the JIT with equal results (allocation, functions taking and
  returning boxes, copy aliasing with `set`, rewind reclaiming bytes, commit,
  nested regions with `f64` and `bool` payloads, one region per recursive
  activation, a pure scratch function, an abort out of a block, a linear
  payload through `take`, run-time bound enforcement, typed native errors),
  and a negative test for every rule above.
* `cargo test --workspace --locked --no-fail-fast` — everything passed except
  suites that need tools this cloud host lacks: `lean4lean42_acceptance`,
  `oracle42_acceptance`, `oracle42_runner` and `replay42_kernel` (no `lean`
  on PATH), and `vesper-loom-build` `build`/`cli` (its build script locates
  `Cargo.lock` from the target directory, which was outside the checkout).
  An earlier run caught a real regression, the Pact bridge fixtures' own
  `effect Region`, fixed by keeping user-declared `Region` effects when no
  region syntax is used.
* `cargo +1.100.0 fmt --all -- --check`, `cargo +1.100.0 clippy --workspace
  --locked --all-targets -- -D warnings` — see the PR.

## Decisions

* **Capability, not lifetime.** Regions reuse lexical capability labels, as
  §15.1 asks ("tie region handles to lexical capability names in Σ —
  requiring zero `'a` lifetime annotations"). This is why no `ExprKind`,
  `Type` variant or IR operation was needed.
* **Access through the capability.** Reading a box needs `r` (`r.get(b)`),
  not just the box. Combined with escape freedom this makes use after the
  region ends impossible even for a handle hidden inside a closure environment.
* **`take` only for unique payloads, `get`/`set` only for copy payloads.**
  Copy handles may alias; moving out of an aliased box would leave other
  aliases stale.
* **Rewind rule.** The linear-payload rule (E0795) is conservative: any live
  linear box of the region blocks a rewind, also one allocated before the
  watermark. The use-after-rewind rule (E0796) is a straight-line analysis of
  one block: a box bound after the checkpoint dies at the rewind, and so does
  a `let` alias of such a box, while an alias of an earlier box survives; other paths are caught at run time by the allocator's stale-handle
  check.
* **Error codes** E0790-E0798 are new and unused elsewhere.

## Remaining gaps

* Source typing rules are implemented by the checker; Rust ownership in
  `vesper-alloc` is not evidence for them, and no Lean model of the region
  rules was written in this lane.
* Native payloads are machine words only; aggregate payloads run on the
  evaluator. Native object (AOT) output of programs that use regions is
  rejected because no region runtime library is linked into executables yet.
* Native region evidence follows the backend's current scheme (effectful
  callees are specialized into their caller, which is limited to 64 inline
  frames); the planned capability-passing ABI (lane 23) will pass the region id
  as an argument instead of through the inlined handler variable.
* The evaluator allocates every payload with `alloc_value`; linear payloads are
  not registered as linear obligations in the host region (the checker rules
  them out statically).
* Not in scope: `RegionSlice`, `RegionRef`/`SlotArena` cyclic graphs, region
  collections in source (`RegionVec`, `RegionString`), `tiled(N)`/AoSoA,
  `@fip`, ShardSlab-backed `eff [Alloc]`, Cranelift vmctx-pinned bump
  pointers and egglog bump coalescing (§15.3), affine finalizer elision, and
  generic functions over region labels (`fn f[T](...) eff [r: Region]` is
  checked like any labelled function but region labels are not generic
  parameters).
