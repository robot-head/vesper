# Port parity wave, lane 5: Aura

Claim: [#953](https://github.com/robot-head/vesper/issues/953). Plan:
[docs/agent-prompts/port-parity/05-aura.md](../agent-prompts/port-parity/05-aura.md).
Base: `main` at `cee8b1a` (a descendant of `69c79b9`).

**Update (#979).** At the owner's request the Vesper port replaced the Rust
implementation in the same PR: `src/reference/` and the `vesper-port`
feature are gone, `build.rs` always links the port (Bazel prebuilds the
object, as for `vesper-beacon`), and `bench-abba.sh` builds the Rust
baseline from commit `cee8b1a` in a temporary worktree. The text below
describes the measurements made before the switch.

**Result.** The Vesper build of `vesper-aura` now runs the `examples/abba.rs`
workloads in 0.42x (one-text), 0.58x (reorder) and 0.85x (encode-audit) of
the Rust build's time, down from 1.54x, 1.94x and 1.47x on `main`. Every
95% median interval lies at or below 1.00x, so the port meets the wave's
performance bar on the cloud x86-64 setup. Every port test, the leak test
and the five divergence checks pass locally; the four platforms run in CI
on the PR. By the owner's rule the Rust build stays the default and no Rust
code is deleted: switching the default is the owner's call.

The port beats Rust mostly by doing less work than the Rust build, not by
faster code generation. Read the comparison with that in mind (see "Why the
port is faster" below).

## Setup

- 4 vCPU Intel Xeon @ 2.10 GHz cloud container, Linux, Rust 1.97
  `--release`. The port is compiled by the in-tree `vesper` through
  `vesper-port-harness` (`optimize = true`, the inliner and
  `opt_level=speed` of #944).
- Wall clock: `aspect crate aura bench-abba` (`abba compare`), 200
  operations per run after a 50-operation warm-up, both builds as separate
  processes in Rust, port, port, Rust order per epoch. Per-lever runs use 15
  epochs. The before/after pair uses 21 epochs in one session.
- `examples/abba.rs` now prints a distribution-free 95% interval for the
  median ratio: the `d`-th smallest and largest per-epoch ratio, with `d` the
  largest value for which a Binomial(n, 1/2) variable is at most `d - 1` with
  probability 2.5% or less (`d = 6` for 21 epochs, 4 for 15). The
  `vesper_bench` "sample median regression" bounds it also prints collapse to
  a point for these samples, so they are not used here.
- Instructions: `valgrind --tool=callgrind` over `abba measure 40` (all three
  workloads, including tree building and the first mount and shutdown of
  each session, which are outside the timed body). Counts are exact.
  Function names come from a local, uncommitted build that named the
  compiler's internal symbols (`vesper_function_<i>`) after their source
  functions.

## Ratio before and after each lever

Wall clock, port/Rust median (15 epochs unless marked). The wall-clock
spread in this container is about ±20% per epoch, so the first two levers
are within noise; their instruction counts are the exact measure.

| Step | Commit | one-text | reorder | encode-audit | Port instructions | vs previous |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `main` (21 epochs) | `cee8b1a` | 1.54x (1.47x..1.70x) | 1.94x (1.77x..2.05x) | 1.47x (1.38x..1.62x) | 1,113.06 M | |
| `main` (15 epochs) | `cee8b1a` | 1.44x | 1.85x | 1.42x | | |
| 1. ABI v3, scratch in locals | `Aura port: export ABI v3 and scratch in locals` | 1.60x | 1.94x | 1.46x | 1,098.30 M | -1.3% |
| 2. Block copies and byte `eq` | `Aura port: block copies and byte eq ...` | 1.52x | 1.97x | 1.40x | 1,058.66 M | -3.6% |
| 3. Hot loops (edges, child indices, sorted fast path) | `Aura port: group edges once ...` | 0.92x | 1.07x | 0.95x | 601.17 M | -43.2% |
| 4. Snapshot on first use, plans read in place | `Aura port: build the recording snapshot ...` | 0.55x | 0.72x | 0.96x | 488.61 M | -18.7% |
| 5. Keys matched by merge, sequences sized up front | `Aura port: match old and new keys ...` | 0.44x | 0.57x | 0.88x | 452.05 M | -7.5% |
| After (21 epochs) | lever 5 | **0.42x (0.39x..0.46x)** | **0.58x (0.52x..0.64x)** | **0.85x (0.80x..1.00x)** | | |

The Rust build runs the same instruction profile in 364.28 M instructions.
The port's count includes the session setup outside the timed body, where
its first mount still binds slots in O(slots²) (see "What is left").

Per-operation times of the 21-epoch pair (median ns/op):

| Workload | Rust | Port before | Port after |
| --- | ---: | ---: | ---: |
| one-text | 434,133 / 427,505 | 658,531 | 179,413 |
| reorder | 461,708 / 474,395 | 862,653 | 273,476 |
| encode-audit | 139,002 / 148,530 | 211,308 | 129,181 |

(The Rust column gives the Rust build's time in the before run and in the
after run.)

## What changed in the port

All edits are inside `crates/vesper-aura/` (the Vesper package, the port
glue in `src/ported/`, the committed binding and `examples/abba.rs`). The
Rust build (`src/reference/`) is untouched.

1. **Export ABI v3.** `// @export abi 3` is in `vesper/src/apply.ves`,
   because the directive is read from file 0 of the package and files load in
   name order (see compiler gap 1). For Aura v3 changes nothing at run time:
   none of the 40 exports takes a copy struct `in` or `inout`, and the trees
   and transactions already cross as `in Slice[u64]`/`in Slice[u8]`, lent in
   place since v2. The binding gains the `repr` mirrors and nothing else.
2. **Scratch in locals.** `Tree.tmp/seen/stack/depths`, `Mounted.tmp`,
   `Prepared.work`, `Recorder.seen/stack/depths` and
   `Query.stack/flags/seen` became locals of the functions that use them, now
   that #945 frees a function's own `ChunkSeq` bindings. The handles are
   smaller, and the leak test still returns to its starting count.
3. **Block copies and `eq`.** `copy_mounted`, `copy_state` (one `extend` per
   run of live entries), the text pushes in `mount_next` and `push_text`, and
   the export copies `prepared_ops`/`query_items`/`query_values` use
   `extend`. `same_value` and `same_text` compare text with the §13 byte
   `eq`. Every byte range is checked before it is viewed, since an interval
   outside a sequence is a run-time fault, not `Fail`.
4. **Hot loops.** These are the profile's top costs on `main`:
   - `validate_final` scanned every edge for every widget it walked (26% of
     the port's instructions). It now groups the edges by parent once
     (counting sort, in edge order) and walks widget indices.
   - `MoveChild` and `InsertChild` made five passes over the edge array
     (`child_edge`, `child_count`, `remove_edge`, `child_edge`,
     `insert_edge`). One fused `child_scan` now finds the count and the
     position, and a move shifts only the edges between its old and new
     positions (`move_edge`). The resulting edge order is the same.
   - `validate` stores each child's node index (`Tree.kidx`), `mount_next`
     derives the mounted key order from the tree's instead of sorting again
     and stores mounted child indices (`Mounted.kidx`), and `detach`,
     `attach` and `teardown` use those indices instead of binary searches.
   - `sort_by_key` returns at once when the keys are already in order.
5. **Rust glue.**
   - `RecordingBackend` rebuilt its `RecordingSnapshot` (BTreeMaps of every
     widget, slot and subscription) after every change, even when nobody read
     it. It now builds it on first use after a change (`OnceCell`).
     `SimAura`'s thread checks call `check_thread` instead of `snapshot`, so
     they do not force a build.
   - `Session::prepare` reads the plan's fault, header and patches through
     `as_ref()` in place instead of copying them through
     `prepared_fault`/`prepared_header`/`prepared_ops`.
6. **Keys matched by merge, sequences sized up front.** `plan` matches old
   and new keys by merging their two sorted orders (`match_keys`), once,
   instead of a binary search per node in `mount_next` and per old node in
   `plan`/`teardown`. A prepared update and a decoded tree are built with
   their sequences sized to the tree (`prepared_for`, `tree_sized`), so
   `mount_next` and `decode` no longer grow them by doubling.

## Why the port is faster

- The Rust build clones its whole `RecordingSnapshot` (BTreeMaps) to stage
  every transaction and builds BTreeMaps of keys in `validate` and
  `plan::build`. The port stages flat arrays and, after this lane, never
  builds the BTreeMap snapshot on a commit nobody inspects. A caller that
  reads `snapshot()` after every commit pays one build per read, about what
  the port paid on every commit before.
- The edge grouping, the fused child scans and the merge join are
  algorithmic changes that the Rust build could also make. The Rust build
  takes bug fixes only, so its algorithms are unchanged.
- Code generation is not where the gap closed: levers 1 and 2, which use the
  Stage 1 builtins and ABI, removed 5% of the instructions; the algorithmic
  levers removed the rest.

## Profile of what is left

Callgrind, lever 5, `abba measure 40` (452.05 M instructions), top
self costs:

| Function | Share | Port or compiler |
| --- | ---: | --- |
| `view::validate` | 8.6% | Port: the per-node checks themselves; already one pass. |
| `recording::widget_index` | 8.4% | Port: binary search by serial, two per edge in `validate_final` and several per patch. A serial-to-index map would remove most of it. |
| `recording::child_scan` | 7.8% | Port: one pass over all edges per `MoveChild`/`InsertChild` (reorder makes 63 moves over 256 edges). Edges grouped per parent would make it O(children). |
| `memcpy` + `malloc`/`free` | 10.5% | Mostly the Rust side (`codec::tree`, `codec::transaction`, `Patch` values, test trees) plus Vesper sequence growth where no size is known (compiler gap 4). |
| `plan::mount_next` | 4.8% | Port. |
| `util::find_key` | 4.6% | Port: the child-key lookups in `validate`. |
| `apply::validate_final` | 3.8% | Port, after the grouping. |
| `view::decode` | 3.8% | Port, word-by-word reads of `in Slice[u64]`. |
| `recording::copy_state` | 3.0% | Compiler gap 2: the published state is copied back instead of swapped. |
| `recording::widget_slot_count` | 2.8% | Port: O(slots) per `BindSlot`, O(slots²) on a first mount (outside the timed body). |
| `util::insert_at`, `remove_at` | 1.4% | Compiler gap 3: element-by-element shifts. |

## Compiler gaps

Each was reproduced with the in-tree `vesper` at `cee8b1a`.

1. **`@export` directives are read from the first file in name order, not
   from `lib.ves`.** `src/main.rs` reads directives from "the root module
   (file 0 of a package)", but the package loader orders files by name, so
   in Aura `apply.ves` is file 0 and `// @export abi 3` in `lib.ves` is
   ignored without a warning. Repro: a package with `src/lib.ves` holding
   `// @export abi 3` and `pub fn get(in p: P) -> u64` for a copy struct
   `P`, plus any `src/a.ves`. `vesper build --lib` reports
   `vesper.native.export-abi/1`; moving the line into `a.ves` gives
   `export-abi/3`. Cost: none at run time, but a port that relies on v3
   mirrors silently gets v2 conversions.
2. **No move out of, or swap of, a non-copy field through `inout`
   (`E0305`).** `Recorder` publishes its staged state with a full
   `copy_state(in rec.stage, inout rec.st)` because
   `let t = h.a; h.a = h.b; h.b = t;` on `inout h: H` is refused ("cannot
   move 'h.a' through a borrowed parameter"). A `swap`/`replace` builtin (or
   allowing a move out that is refilled before exit) would remove one of the
   two state copies per commit: about half of `copy_state`'s 3.0%.
3. **No in-place shift within one sequence (`E0301`).**
   `s.copy_from(0, in s[1..n])` is refused ("copy_from cannot read from the
   sequence it writes"), so `insert_at`, `remove_at`, `insert_edge`,
   `remove_edge` and `move_edge` shift element by element. A `copy_within`
   (memmove) would make them one call each. Measured share: 1.4% plus part
   of `patch_children`'s 2.2%.
4. **Dropped local handles leak their sequences, and so does replacing a
   sequence field.** A local of a non-`copy` struct type that is dropped
   (for example a `Prepared` built on a path that returns another one) does
   not free the `ChunkSeq` fields inside it, and `p.m.nodes =
   ChunkSeq::with_capacity[MNode](n)` on an `inout p` leaks the old
   sequence. Both were caught by the port's leak test
   (`port_frees_every_allocation_it_makes`) during this lane. Minimal shape:
   `pub struct H { s: ChunkSeq[u64] }` with `fn reset(inout h: H) { h.s =
   ChunkSeq::with_capacity[u64](8); }`, or `let h = make_h();` dropped at the
   end of a function. With no `reserve`, a handle's sequences can only be
   sized when the handle is built, so the port restructured
   `do_reconciler_prepare` to build exactly the `Prepared` it returns. Cost
   now: the remaining doubling growth where the size is not known up front
   (part of the 10.5% allocator share).
5. **A receiver loan blocks reading the same sequence in its arguments
   (`E0410`).** `first.try_set(i, inc(seq_u64(in first, i)))` and
   `s.try_set(i, wrapping_add(get(in s, i), 1))` are refused ("IR access to
   's' overlaps active loan 's'"); the value must go through a `let`. No run
   time cost; an ergonomic gap (two-phase borrows).
6. **`SeqRef` has no slice view for packed scalars**
   (`crates/vesper-port-abi`). `SeqRef<u64>` offers `get`/`iter`/`to_vec`
   but no `as_slice`, so the encode and audit wrappers still copy `items` and
   `values` into a `Vec` to chunk them. Small (under 1%).

## Tests run

- `cargo test --release -p vesper-aura --features vesper-port` after every
  lever: the 28 integration tests (12 + 10 + 6), the 2 unit tests (one is
  the leak test) and the 9 doctests pass.
- `cargo test -p vesper-aura` (the default Rust build) and the vesper-aura
  section of `scripts/test-port-harness.sh` (the five divergent builds each
  fail their named test): see the PR.
- `cargo +1.100.0 clippy --all-targets -p vesper-aura --features
  vesper-port -- -D warnings`, and without the feature.
- Bazel (BuildBuddy), macOS AArch64, Windows x86_64 MSVC and the
  `aura-gtk.yml` workflow run in CI on the PR. The package's file set did
  not change, so `BUILD.bazel` is unchanged.

## Parity

On the cloud x86-64 setup, the ABBA 95% median intervals are 0.39x..0.46x,
0.52x..0.64x and 0.80x..1.00x, all below the 1.05x bar. Functionality: the
port's tests, the leak test and the divergence checks pass locally, and the
other three platforms are checked by the PR's CI. If CI is green, the port
meets the parity bar of the wave plan. The default stays Rust until the
owner decides otherwise.
