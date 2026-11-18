# Port wave P3a lane 2: source atomics (F12, #1037)

Atomics half: PR #1052 (merged, base `3c075a4`). Scoped-worker half: the
follow-up PR on the same claim, stacked on the Pulse pool part 1 (wave-nine
lane 22, #599, PR #1058). It is described under "Scoped workers" below.

## What changed

- **Manuscript (flagged).** New subsection §15.4 "Source Atomics
  (`Atomic[T]`, `MemoryOrder`)" in `docs/manuscript/content.typ`. The
  manuscript used atomics only inside Rust listings (§13 ShardSlab,
  Resonance's `RtAtomicSlot`), so this is a new source rule: cell kinds,
  mode, operations, orders, effects and backends.
- **Surface.** `Atomic[T]` for `bool`, `u8`-`u64`, `usize` and `i8`-`i64`;
  `Atomic::new(v)`, `load`, `store`, `swap`, `compare_exchange(current, new,
  success, failure) -> Result[T, T]`, `fetch_add`/`fetch_sub` (integers,
  wrapping), `fetch_and`/`fetch_or`/`fetch_xor` (integers and `bool`) and
  `into_inner(sink self)`. Orders are literal
  `MemoryOrder::{Relaxed, Acquire, Release, AcqRel, SeqCst}` with the C++20 and
  Rust meanings. Method syntax (`c.fetch_add(1, MemoryOrder::Relaxed)`) and
  path syntax (`Atomic::load(in c, ...)`) both work.
- **Mode.** `Atomic[T]` is affine (never `copy`); every operation takes the
  cell by `in`, so workers will share it through `in` borrows. Plain fields
  keep their race freedom from the existing borrow rules.
- **Effects.** None. Atomic operations are total. `Atomic::new` allocates like
  `ChunkSeq::new` (ambient, as the std README notes for all allocation).
- **Representation** (`src/atomics.rs`). A prelude, injected only when a
  module mentions `Atomic` or `MemoryOrder` and declares neither, defines
  `struct Atomic[T: mod copy] { cell: ChunkSeq[T] }` holding exactly one
  element, `copy enum MemoryOrder` and the methods. Every copy of the
  one-word sequence handle that an `in` borrow passes names the same heap
  element, and the existing drop glue frees it. No new Place IR form, so
  `src/ir.rs` is untouched (Stage 1 guard).
- **Prelude bodies** only type the calls. Every operation that would update
  the cell recurses instead, which keeps the inliner from copying a body in
  place of the call (it refuses recursive callees) and makes a backend that
  does not intercept the call overflow its stack rather than silently drop an
  update.
- **Checker** (`atomics::check_world`, `atomics::check_program`, hooked next
  to `seq`/`strings` in `check_core`):
  - `E0820`: an order argument that is not a literal `MemoryOrder`, a `load`
    (or `compare_exchange` failure order) with `Release`/`AcqRel`, a `store`
    with `Acquire`/`AcqRel`.
  - `E0821`: `Atomic[T]` for any other `T`, or `fetch_add`/`fetch_sub` on
    `Atomic[bool]`.
  - `E0822`: an `Atomic { cell: .. }` literal, or any read or projection of
    the private `cell` outside the prelude (checked on Place IR, where every
    read names its root's type).
- **Evaluator** (`runtime.rs`, `atomic_call`): intercepts `Atomic::<op>` and
  updates the element of the shared `Rc<RefCell<Vec<Value>>>` cell.
- **Native** (`native.rs`, `Codegen::atomic_call`): intercepts the
  call (JIT, executables, exports and sessions share the path) and emits one
  `atomic_load`, `atomic_store`, `atomic_rmw` (`xchg`, `add`, `sub`, `and`,
  `or`, `xor`) or `atomic_cas` at the kind's width (`bool`/`u8`/`i8` are one
  byte, `u16`/`i16` two, `u32`/`i32` four), on the element at offset 0 of the
  sequence data (word aligned; every supported target is little-endian), with
  canonical lanes in and out. `compare_exchange` builds its `Result` from the
  CAS result and one compare. Cranelift atomics are sequentially consistent,
  which satisfies every order; the order arguments need no code.
- **Embedding** (`native::WordFunction`): compiles a function whose
  parameters and result are words (`in` aggregates are addresses) and calls
  it from any thread. Until scoped workers land this is how host Rust shares
  an atomic with compiled Vesper (and how the contention tests run).
- **Other backends.** The verified backend refuses the cell's `ChunkSeq` with
  its typed `Unsupported::Type` error (no edit to `src/verified_codegen/**`).
  The Wasm backend refuses the prelude's sequence operations
  (`unsupported Operation in 'Atomic::new<T=u64>'`). eBPF programs cannot
  name the type usefully (no sequences).

## Tests

`tests/atomics.rs` (10 tests, plus one ignored measurement):

- `every_integer_kind_agrees_with_the_model`: for all ten integer kinds, a
  program runs every read-modify-write over edge operands with rotating
  orders, a failing and a succeeding `compare_exchange`, `store`/`load` and
  `into_inner`, in the evaluator and the JIT (optimized and not), as x86-64
  and AArch64 objects, and as an executable where `cc` links one; all match
  `AtomicOp::rmw`.
- `the_model_matches_std_atomics`: `AtomicOp::rmw` against `AtomicU8` and
  `AtomicI8` on edge values.
- `bool_cells_agree_with_the_model`.
- `counters_under_contention_lose_no_update`: four host threads each run
  20,000 increments in compiled Vesper (optimized and not) through
  `WordFunction`: `fetch_add` with `Relaxed`, `AcqRel`, `Release`, `Acquire`,
  `fetch_sub` of `-1` with `SeqCst`, a `load` + `compare_exchange` retry loop,
  a wrapping `Atomic[u8]` counter and an `Atomic[bool]` toggled an even number
  of times. No update is lost.
- `optimized_native_code_holds_one_atomic_per_operation`: the CLIF has
  `atomic_rmw.i64 add/sub`, `atomic_rmw.i8 add/xor`, `atomic_cas.i64` and
  `atomic_load.i64`.
- `every_allowed_order_is_accepted`, `invalid_orders_kinds_and_the_private_cell_are_refused`
  (every `E0820`-`E0822` case), `an_atomic_cell_is_never_copy`,
  `a_user_atomic_type_takes_precedence`, `the_verified_backend_refuses_atomics`.

Platforms: the four required checks run the suite (contention included:
`std::thread::scope` and JIT code need no C toolchain; the executable leg
runs where `cc` exists).

## Measurements

`cargo test --release --test atomics -- --ignored --nocapture measure`, 4-vCPU
cloud host, 2,000,000 `fetch_add(1, Relaxed)` per thread, three rounds:

| Threads | Vesper ns/op | Rust ns/op | Ratio |
|--:|--:|--:|--:|
| 1 | 10.0-11.5 | 9.1-9.6 | 1.11-1.20x |
| 4 (contended) | 21.3-22.0 | 17.6-18.6 | 1.15-1.25x |

The gap is the cell indirection: each operation loads the struct word, then
the sequence header's data pointer, before the atomic. Cranelift does not
hoist those loads out of the loop. 

### 4-worker parallel sum

`cargo test --release --test scoped_workers -- --ignored --nocapture measure`,
same 4-vCPU host, `VESPER_WORKERS=4`: the sum of 2^24 `u64`s split into four
iterations, against Rust summing the same `Vec<u64>` on four scoped threads.
A Vesper time per sum is (20 sums - 0 sums) / 20 in one JIT run, so the fill
and the compile cancel out. Three rounds:

| | 4 workers | 1 thread | Speedup |
|--|--:|--:|--:|
| Vesper (optimized JIT) | 13.9-17.9 ms | 29.2-30.1 ms | 1.7-2.1x |
| Rust (`fold`) | 3.9-4.2 ms | 10.4-10.5 ms | 2.5-2.6x |
| Ratio | 3.5-4.2x | 2.8-2.9x | |

The pool adds little: the parallel gap (3.5-4.2x) is mostly the
single-thread gap (2.8x), which is the loop body, not the workers. Rust's
`fold` over a slice is vectorized with no bounds checks, while the Vesper
loop does one `try_get` (bounds check plus header load) per element, the
same gap the bench port reports (no bounds-check elimination or fixed
arrays). The rest is the shared memory bandwidth of four workers on a
4-vCPU guest, which Rust's faster loop saturates sooner.

## Decisions

- One generic `Atomic[T]` with methods instead of suffix-named functions
  (`atomic_fetch_add_u64`): methods are what the ported crates read like, and
  the native and evaluator intercepts get `T` from the call's type arguments.
- Literal orders only, so every invalid order is a compile-time diagnostic
  (Rust panics at run time instead).
- A heap cell rather than an in-place word: an `in` borrow may copy an
  aggregate argument, and closure captures copy values, so only an
  indirection keeps every borrower on the same word. It also reuses the
  sequence drop glue.
- The enum is `MemoryOrder`, not `Ordering`, which would collide with
  comparison orderings in ported code.

## Scoped workers

- **Surface** (manuscript §15.4 "Scoped Workers (`parallel_for`)"):
  `parallel_for(in data, n, body)` runs `body(in data, i)` for every `i` in
  `0..n` and returns after the last one. The signature is
  `fn parallel_for[A](in data: A, n: u64, body: fn @copy(in A, u64))`. It
  is a prelude function (`atomics::WORKERS_PRELUDE`), injected only when a
  module calls `parallel_for` and declares none of its own.
- **Sharing rule.** Workers share `data` only by `in`, so the borrow rules
  already keep plain fields race-free and an `Atomic` cell is the only
  thing they update together. `E0824` refuses `parallel_for(inout x, ..)`
  and a body whose first parameter is `inout` or `sink`
  (`tests/fixtures/scoped_workers/shared_inout.ves` is the fixture, checked
  through `vesper check` too). `E0823` refuses a body that is a closure,
  generic, or has a non-empty effect row: a worker runs the body on its own,
  with no handler, capability or capture from the caller.
- **Join.** The call returns only after every iteration, so no worker
  outlives the borrow and the call needs no effect. A body cannot fail
  (empty row), so there is no failure to propagate; a body that needs to
  report one sets an atomic flag.
- **Backends.** The prelude body is the sequential loop, which is a valid
  schedule: the evaluator and executables run it unchanged (no new Place IR
  form, no edit to `src/ir.rs` or the verified backend). The JIT intercepts
  the call in `Codegen::parallel_for_call` before function-value
  specialization, resolves the body to its compiled function (the optimize
  path renames the argument, so it reads the value's closure target), checks
  that its signature is two words in and at most one word out, and calls
  `vesper_parallel_for(body, data, n)`. Anything else falls back to the
  sequential loop.
- **Runtime** (`src/atomics/workers.rs`). A process-wide Pulse pool
  (`Pool::start` from #1058, one worker per hardware thread, or
  `VESPER_WORKERS`) started on first use. A call splits `0..n` into at most
  four chunks per worker, spawns all but the last, runs the last on the
  calling thread and waits on a latch. A call from inside a worker (a
  nested `parallel_for`) runs on that worker, so nesting never deadlocks a
  pool whose workers are all waiting. A refused spawn runs inline.
  A sequence fault recorded on a worker (the fault state is per thread)
  moves to the calling thread after the join, so the JIT run fails as it
  would sequentially. Data or bodies that reach a `TiledSeq`, `Box` or
  region value (`atomics::thread_affine`), whose JIT runtimes keep
  per-thread registries, run on the calling thread instead.

Tests (`tests/scoped_workers.rs`, 8 tests plus one ignored measurement):
a parallel sum, scalar data with empty and one-iteration loops, and nested
loops in the evaluator and JIT (optimized and not), x86-64 and AArch64
objects and an executable; two iterations that wait for each other finish
only if the JIT runs them at the same time (optimized and not); the runtime
runs each iteration exactly once on more than one thread; `E0824` and
`E0823` cases; a user `parallel_for` takes precedence.

Measurement: see "4-worker parallel sum" below.

## Remaining

- **Executables on the pool.** `vesper build` executables run
  `parallel_for` sequentially; they need the pool runtime linked into the
  generated C driver (or a freestanding equivalent).
- **Wider bodies.** Bodies with effects (a `Fail` row joined into the
  caller's handler), closures with captures, and `spawn`-style scopes with
  different bodies per task (`@vesper-std/sync`) are follow-ups.
- **Cell address hoisting**: an `Atomic` stored in place (one indirection
  instead of two), or loop-invariant hoisting of the header load, would close
  most of the 1.1-1.25x gap.
- **What the P3b crates still need:** Kinetic's threaded `RigidWorld` and
  Flux's multi-worker executor can start on `parallel_for`. Resonance's rings
  need atomics on elements of a sequence (`ChunkSeq[Atomic[u64]]` works but
  costs one allocation per element; a `AtomicSeq[T]` with indexed operations
  would not). Quarry's latches need `compare_exchange` (here) plus a
  `spin_loop` hint and a park/unpark primitive from Pulse.
