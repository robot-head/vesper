# Rust exit SH6 lane 5: Prism and Prism-H3

Claim: [#1400](https://github.com/robot-head/vesper/issues/1400). Base:
`main` at `ab965f2` (2026-11-21). The earlier claims this lane continues
(#911, #951, #1067) were already closed.

| Part | Scope | State |
|---|---|---|
| 1 | `vesper-prism`: close the speed gap, then flip (the Vesper port is the only build, the Rust implementation deleted) | this PR |
| 2 | `vesper-prism-h3`: close the encode and message gaps, flip the ported modules | next |
| 3 | `vesper-prism-h3` drivers (`connection.rs`, `client.rs`, `transport.rs`, `warp.rs`, `prism.rs`, `session.rs`) on the ported Warp | after part 2 |

## Part 1: vesper-prism

### Flip-gate verdict

Run by hand (the recipe in `docs/maintenance/rust-exit-plan.md`; SH6 lane 1's
`scripts/ci/flip_gate.py` has not merged yet).

1. **Full function.** Every test of the crate passes against the Vesper build,
   which is now the only build: `tests/framing.rs`, `responses.rs`,
   `routing.rs`, `connection.rs`, `malformed.rs` (its outcome digest is the one
   the Rust build produced), the new `byte_classes.rs`, the unit test in
   `src/connection.rs` and the doctests. `vesper-prism-h3`'s whole suite
   (QPACK corpora, HTTP/3 and WebTransport end to end, the quinn and
   wtransport interop tests) passes on top of it. Each of the five divergent
   libraries still fails its module's tests (`test-port.sh`).
2. **Speed.** `taskset -c 3 sh crates/vesper-prism/bench-abba.sh 21 400`:
   the last Rust build (`ab965f2`) against this branch, 21 ABBA epochs of 400
   operations, pinned to one CPU:

   | Workload | Rust | Vesper | Ratio | 95% interval | Bar (upper end <= 1.5x) |
   |---|--:|--:|--:|---|---|
   | decode | 63.3 µs | 60.4 µs | 0.96x | 0.92x..1.01x | pass |
   | route | 13.5 µs | 18.0 µs | 1.34x | 1.28x..1.41x | pass |
   | encode | 13.7 µs | 15.7 µs | 1.15x | 1.10x..1.22x | pass |
   | connection | 76.6 µs | 85.5 µs | 1.11x | 1.05x..1.13x | pass |

3. **Tests.** The crate's tests are integration tests of its public API. They
   ran against both builds before and run against the only build now; no
   test was deleted (the deleted Rust had no tests of its own).

Verdict: **flip.** Rust deleted: `src/reference/` (1,707 lines: decoder 414,
connection 469, response 339, router 305, target 172, module 8).

What stays Rust in the crate: the public types and their `Display` impls
(`src/lib.rs`, `connection.rs`, `response.rs`, `router.rs`, `target.rs`), the
forwarding adapters in `src/forward/` (they turn Vesper's buffers and ranges
into the API's `Vec`s and `String`s, keep each route's generic payload `T`,
and do the `io::Write` call), the generated bindings in `port/`, and
`service.rs`, the middleware chain (a `Middleware` trait object called per
request, 113 lines). These are the boundary to the crate's Rust users
(`vesper-prism-h3` and its interop crate); they go when those users are
Vesper.

### The baseline

Before this lane, on the same container (4 vCPU, unpinned, 15 epochs of 200):
decode 1.28x (1.18-1.49), route 2.35x (1.86-3.27), encode 1.87x (1.46-2.31),
connection 1.51x (1.40-1.58). Unpinned runs on this container spread widely
(route's interval was 1.4 wide), so every number above is pinned.

### What changed

All in `crates/vesper-prism/` except one harness hunk (below). Instructions
are callgrind counts of `abba measure 40 <workload>` (50 operations with the
warm-up, setup included), the port built with the in-tree compiler.

| Step | route | encode |
|---|--:|--:|
| Rust (`ab965f2`) | 8.71 M | 9.68 M |
| port on `main` | 16.76 M (1.92x) | 14.78 M (1.53x) |
| word-at-a-time byte classes | 16.43 M | 13.39 M |
| target: unescaped parts copied whole, two buffers, reserved sizes | 12.88 M | |
| router: inline comparisons, per-thread segment buffer | 12.73 M | |
| response: fields pre-rendered in one buffer, decimals in place | | 10.52 M (1.09x) |
| router: one-way nodes followed in a loop, flat node and edge words | 11.29 M | |
| target: one escape scan per path and query | **10.92 M (1.25x)** | **10.58 M (1.09x)** |

decode went to 36.1 M against Rust's 41.0 M (0.88x) and connection to 46.1 M
against 47.3 M (0.97x).

- **`vesper/bytes` (`@vesper/prism-bytes`), a new package** the decoder,
  response and target packages depend on. `find_not_in` is a scalar loop of
  about 14 instructions per byte on the SSE2-only x86-64 target, and the hot
  paths ran it over every field name, field value and target. The new scans
  read eight bytes at a time (`try_read_u64_le`) and test them with word
  arithmetic on 7-bit lanes, where adding a constant below 128 never carries
  into the next lane, so each lane's flag is exact:
  - `first_control` (field values: controls but tab, DEL) is exact per word;
  - `first_not_token` (field names) and `first_not_path` (targets) test a fast
    class (letters, digits, `-`, and `./_&=` for targets) and hand only a word
    that leaves it to `find_not_in` with the exact class.

  `tests/byte_classes.rs` puts every byte value at every position of names,
  values and targets of 1, 7, 8, 9, 16 and 21 bytes (both sides of each word
  boundary and the tail) and checks the result against the RFC grammars
  written out in the test. It passed against the Rust build before the flip.
- **Target.** A segment or query part without `%` (and without `+` in the
  query) is copied whole, and the checks of decoded bytes (`/`, `\`, NUL,
  UTF-8) are skipped for it: the grammar already kept its raw bytes to ASCII
  without those. One scan of the whole path, and one of the query, decides
  whether any part needs its own. The result is two buffers (`bytes`, `ends`)
  instead of nine sequences, each allocated once at its final size.
- **Response.** A field is kept the way it goes on the wire (lowercase name,
  `: `, value, CRLF) in one buffer, so `finish` copies all fields at once, and
  `push_decimal` writes digits in place instead of through a scratch sequence
  (one allocation per call).
- **Router.** The trie's nodes and edges are word arrays read one field at a
  time (copying a 6-word `Node` out of a `ChunkSeq[Node]` went through the
  stack twice). The match follows a node with one way on in a loop and
  recurses only where a static and a parameter child both exist. Edge-key and
  method comparisons are inline length tests and slice `eq`. The Rust adapter
  reuses a per-thread buffer for the target's segments, and the capture list
  is allocated by the first capture only.
- **Divergence edits** follow the code: the router's now reads
  `if param_child == none()`.

### The flip

- `src/reference/` and the `vesper-port` feature are gone; `src/forward/` is
  the implementation. `vesper-port-harness` is a plain build dependency.
- Bazel: the default crate target runs `build.rs` with the five prebuilt
  objects. This needed one change outside the crate:
  **`crates/vesper-port-harness`** now reads `VESPER_PORT_OBJECT_<LIBRARY>`
  and `VESPER_PORT_GENERATED_BINDING_<LIBRARY>` before the shared names, so a
  crate with several libraries can pass one object per library (its own
  commit; no lane owns the harness). The divergence binaries link a copy of
  the crate without its build script, as before.
- `bench-abba.sh` builds the Rust baseline from `ab965f2` in a temporary
  worktree, as Aura's does, with this branch's `examples/abba.rs` (which
  also gained `abba measure <ops> <workload>` for profiling).
- `test-port.sh` runs the crate's tests, `vesper-prism-h3`'s and the
  divergence checks.

### Compiler gaps

1. **Vector code is slower than the scalar loop it replaces.** A 16-lane
   `first_control` written with `Vec::load_masked[u8, 16]`, `Vec::lt`,
   `Vec::eq`, `Mask::and` and `Mask::bitmask` (§14.3) cost 277 instructions
   per 1-2 iterations, more than the scalar `find_not_in`. In the object:
   every module constant used in the function is a call
   (`vesper_function_N_const_SPACE`), every vector value goes through a stack
   slot as two 64-bit words between operations, the masked load copies byte
   by byte into a stack buffer for short tails, and a callee with vector
   parameters is never inlined (a helper `in_range(v, lo, hi)` stayed a call
   per 16 bytes). Repro:

   ```vesper
   const SPACE: u8 = 32;
   fn controls16(in s: Slice[u8]) -> u64 eff [Isa[Baseline128]] {
       let v = Vec::load_masked[u8, 16](in s, 0);
       Mask::bitmask(Mask::and(Vec::lt(v, Vec::splat[u8, 16](SPACE)), Mask::active[u8, 16](0, s.len())))
   }
   pub fn controls(in s: Slice[u8]) -> u64 {
       if Isa::supported[Baseline128]() {
           controls16(in s)
       } else {
           0
       }
   }
   ```

2. **`find_not_in` is still scalar on x86-64 without SSSE3** (known from
   `port-speed-short-scans.md`), about 14 instructions per byte. The port now
   avoids it on its hot paths; a `pshufb`-free lowering for SSE2 (or an
   x86-64-v2 baseline) would let the port drop `vesper/bytes`.
3. **Records read out of a sequence are copied through the stack twice.**
   `match r.nodes.try_get(i) { Option::Some(n) => n.param_child, ... }` loads
   all six words of the `Node`, stores them to one stack slot, copies them to
   a second and then reads one field. The port now stores the trie as words.
4. **Every `extend` is a `memcpy` call**, also for 2- and 3-byte copies whose
   length the code knows, where Rust inlines the copy. In `route` the port
   makes about 13 `memcpy` calls per operation against Rust's 0.4.

### What is left

- Part 2 (`vesper-prism-h3`) and part 3 (the drivers), below.
- The census row (`scripts/rust-census.py`, STATUS tables) is updated once
  the plan PR (#1415) that adds the script has merged.
