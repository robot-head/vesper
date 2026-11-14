# Source Strata rest: Lean model, aggregate native payloads, executables (§15)

Wave eight, lane 8. Claim: #457. Base: `1094e64` (descendant of the required
baseline `ea1ed29`). Branch: `claude/project-thread-dho5uo`. Builds on wave
seven lane 29 ([strata-source-regions.md](strata-source-regions.md)).

## What was built

1. **A Lean model of the region rules with a soundness theorem**
   (`lean/StrataRegions.lean`).
2. **Aggregate payloads on the native backends**: tuples, structs, enums
   (including generic enums such as `Option[(i64, i64)]`) and strings are
   stored in regions as their native layout, not only machine words.
3. **A region runtime in AOT executables**: programs that use regions build
   with `vesper build` (and `vesper emit object`) and run, needing only the C
   library.
4. **Generic functions over region labels** (`fn f[r: Region]`): not built;
   it needs a parser change outside this lane's paths (see Remaining gaps).

### One accounting model on every backend (`vesper.region.accounting/1`)

So that `r.used()` and bound exhaustion agree between the evaluator, the JIT
and executables, every backend charges a region the same bytes
(`src/regions/store.rs`):

* a payload of type `T` occupies its native layout size (`crate::layout`),
  a multiple of 8 and at least 8 bytes;
* a block of capacity `C` is one chunk of `C` bytes; an allocation fails when
  it does not fit after the bytes in use;
* `take` of the newest unprotected payload gives its bytes back, and the
  moved payloads below it follow (stopping at the newest watermark);
  `rewind(m)` restores the bytes and allocations recorded at `m` and
  invalidates newer watermarks; `commit(m)` drops `m` (also an older one) and
  then gives back the moved tail;
* at most 2^20 tracked allocations and 256 outstanding watermarks.

These are `vesper_alloc::Region`'s own rules with one chunk, 8-byte alignment
and byte payloads (`alloc_bytes`, `release_bytes`); the evaluator and the JIT
use that allocator directly. Before this lane the evaluator scaled its bound
by `size_of::<Value>()` and reported `used()` in word units; it now charges
real layout bytes. The evaluator takes the payload type from the call's
checked result type `RegionBox[r, T]` (the IR resume frame) and falls back to
the value's own type when `T` is not concrete, recovering generic data
arguments from the fields the value holds; an argument the value does not
determine (the `T` of `None` seen without a concrete type) counts as one word,
so in that case only the evaluator may charge less than the native backends.
A rewind drops the evaluator values of the allocations it discards at once.

### Native payloads (`src/regions/native.rs`)

The runtime calls changed from one-word `box/get/set/take` to a word API:
`vesper_region_alloc(id, size) -> handle`, `vesper_region_store(id, handle,
index, word)`, `vesper_region_load(id, handle, index) -> word`,
`vesper_region_release(id, handle)`, plus the unchanged `open`, `close`,
`checkpoint`, `rewind`, `commit` and `used`. `box` allocates and stores the
payload word by word, `get` copies it into a fresh stack slot (aggregates) or
returns the word (scalars; `f64` as its bit pattern, narrow integers
re-extended), `set` overwrites in place, and `take` copies out and releases.
No `unsafe` was added: the JIT runtime copies words through
`Region::bytes`/`bytes_mut`.

### Executables (`src/regions/aot_runtime.rs`)

`vesper build` links one object with `cc`, so the runtime cannot come from the
`vesper` binary. Instead, the first region operation an object module lowers
defines the ten `vesper_region_*` functions (plus four helpers and six fault
messages) as **local** symbols of that object, generated as Cranelift IR.
They implement the accounting rules above over a malloc'd record (chunk,
cursor, slot array, watermark array, serial) and import only `malloc`,
`realloc`, `free`, `memset`, `memmove`, `write` and `exit`. A box handle is
its slot index plus the slot's serial, so a stale handle (rewound, moved out)
is detected, never read. A fault prints `vesper: native region fault: ...`
and exits with status 70; a freestanding target (`*-none-*`) is rejected with
a typed error because the generated runtime needs the C library. No hunk in
`src/native*.rs` or `src/main.rs` was needed: the code lives in the
`regions` child module of `native.rs`, and the link command is unchanged.

### Lean model (`lean/StrataRegions.lean`)

A first-order language with `region e`, `box r e`, `get r e`, `let`, `+`,
labels as de Bruijn levels and a store that is a stack of append-only
regions; `region` pushes a region and pops (frees) it. The typing relation
encodes [T-RegionScope] (a block's result type must be well formed without
its own label, E0791), [T-RegionAlloc] (allocation and access need a live
label, E0401/E0797) and branding (E0792). The fuel-bounded evaluator returns
`wrong` on every unsafe access. Proved, with no `sorry`, `admit` or new
axioms (the key theorems depend on `propext`, `Classical.choice` and
`Quot.sound` at most; the examples on none):

* `soundness`: a well-typed expression never evaluates to `wrong`; its value
  has its type in the final store, which extends the initial one with the
  same live regions;
* `lexical_lifetimes`, `never_wrong`: corollaries;
* `escape_freedom`: a block's value holds no handle into the freed region;
* `dangling_reads_the_next_region`: without the escape rule an escaped handle
  silently reads the next region that reuses its label (a use after free no
  run-time check of that store catches), and `escaping_box_rejected` shows
  the typing rejects that program; `cross_region_rejected`, `nested_typed`,
  `nested_runs`.

This is a theorem about the model of the rules, **not** about the Rust
checker, the evaluator, the JIT or the generated runtime; the backends are
tested, not proved. Rust ownership in `vesper-alloc` is not evidence for the
source typing rules either.

## Tests

* `tests/regions_backends.rs` (new):
  * `evaluator_jit_and_executables_agree`: seven programs (structs and enums
    with `get`/`set`/`match`, byte accounting with a generic enum payload and
    rewind, linear `take` reclaiming bytes, `commit` of an older watermark
    with the moved-tail cascade, tuples in recursive activations, callees
    allocating aggregates in the caller's region, scalar payloads including
    `u8`, `bool`, `f64`) give the same value on the evaluator, the optimized
    and unoptimized JIT, and an executable built with `vesper build` (its exit
    status), on Unix hosts with `cc`;
  * `run_time_faults_agree`: bound exhaustion and a stale handle that escapes
    the straight-line rewind rule (bound through a tuple pattern) fail on all
    three, the executable with status 70 and the fault text;
  * `escape_attempts_keep_their_diagnostics`: block results with aggregate
    boxes and closures (E0791), cross-region access (E0792), use after rewind
    (E0796), signatures naming a region they do not receive (E0797);
  * object output carries the runtime and imports only the C library;
    freestanding targets are rejected;
  * the Lean model has no escape hatches, and with `VESPER_LEAN` set it
    compiles and every key theorem's axiom report is checked.
* Fixtures: `tests/fixtures/regions/*.ves`, canonically formatted
  (`vesper fmt --check` passes).
* `tests/regions.rs`: the two tests that pinned the old limits (aggregate
  payloads rejected natively, object output rejected) now check that they
  work.

## Checks

Run on Linux x86_64 (local Rust 1.97 and 1.100, Lean 4.24.0 from the release
zip):

* `VESPER_LEAN=.../lean cargo test --test regions_backends --test regions`:
  7 + 34 passed.
* `lean lean/StrataRegions.lean`: no errors or warnings.
* Workspace tests, `cargo +1.100.0 fmt --all -- --check` and
  `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  see the PR for the exact results.

## Decisions

* **Generated runtime rather than a linked library.** The link step stays
  `cc object -o exe -lm`, executables need no Vesper runtime library, and the
  symbols are local, so two objects never clash. The cost is a second
  implementation of the accounting rules, which the differential tests check
  against `vesper-alloc` on every run.
* **Local labels only in symbols, not in the ABI.** The runtime functions
  keep the JIT's `i64` signatures, so the codegen is identical for JIT and
  object output apart from linkage.
* **Faults end an executable.** The JIT records the first fault and reports
  it after the entry returns; an executable cannot return a typed error, so
  it reports and exits at the fault. Results agree on every program that
  does not fault.
* **Regions an abort skips.** As in the JIT (which closes them when the entry
  returns), an executable keeps such a region until the process exits.

## Remaining gaps

* **`fn f[r: Region]`**: a lower-case generic parameter is parsed as an effect
  row variable (`[e: Eff]`), so `[r: Region]` is a syntax error in
  `crates/vesper-syntax`, which this lane does not own; the checker would
  also need a hook in `check.rs` to turn such a parameter into a region
  capability. The existing form `fn f(b: RegionBox[r, T]) -> ... eff [r:
  Region]` already instantiates `r` per call. Callers must currently bind a
  region of the same name `r` lexically; renaming at the call (passing region
  `a` for `r`) is not supported.
* The Lean model omits label-polymorphic functions, modes and `take`, `set`,
  watermarks and rewind (E0795/E0796) and the byte bound.
* Linear payloads are not registered as linear obligations in the allocator
  (byte allocations carry no linear flag); the checker rules out abandoning
  or rewinding over them statically (E0302, E0795).
* The generated runtime is for 64-bit hosted targets; freestanding images
  (lane 27 of wave seven, PR #373) and Windows executables (PR #360) are not
  covered. Executables are exercised by the tests on Unix hosts only.
* Not in scope, as before: `RegionSlice`, `RegionRef`/`SlotArena`, region
  collections in source, `tiled(N)`, `@fip`, vmctx-pinned bump pointers and
  egglog bump coalescing.
