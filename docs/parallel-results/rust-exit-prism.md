# Rust exit SH6 lane 5: Prism and Prism-H3

Claim: [#1400](https://github.com/robot-head/vesper/issues/1400). Base:
`main` at `ab965f2` (2026-11-21). The earlier claims this lane continues
(#911, #951, #1067) were already closed.

| Part | Scope | State |
|---|---|---|
| 1 | `vesper-prism`: close the speed gap, then flip (the Vesper port is the only build, the Rust implementation deleted) | #1434 |
| 2 | `vesper-prism-h3`: close the encode and message gaps, flip the ported modules | #1472 |
| 3 | `vesper-prism-h3` drivers (`connection.rs`, `client.rs`, `transport.rs`, `warp.rs`, `prism.rs`, `session.rs`) on the ported Warp | 3a: #1491; 3b: ready |

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
2. **Speed.** `taskset -c 3 aspect crate prism bench-abba 21 400`:
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

## Part 2: vesper-prism-h3

### Flip-gate verdict

1. **Full function.** Every test of the crate passes against the Vesper
   build, which is now the only build: the QPACK corpora and golden files,
   frames, priorities, capsules, message rules, HTTP/3 and WebTransport end
   to end, and the quinn and wtransport interop tests. The three unit tests
   of deleted Rust items are now integration tests of the public API in
   `tests/qpack_tables.rs`:
   - the Huffman code is complete (Kraft equality over the code lengths) and
     every symbol round-trips;
   - static-table spot checks (indices 0, 17, 25, 31);
   - the Required Insert Count wraps modulo `2 * MaxEntries` (40 sections
     through a 100-byte table).

   Each of the ten divergent modules still fails its guarding test
   (`port/check.sh`).
2. **Speed.** `taskset -c 3 aspect crate prism-h3 bench`
   against the Rust build (12 ABBA epochs of 16 batches, pinned to one CPU):

   | Workload | Rust | Vesper | Ratio | 95% interval | Before this lane | Bar (upper end <= 1.5x) |
   |---|--:|--:|--:|---|--:|---|
   | decode | 2,374 ns/section | 2,366 ns | 0.99x | 0.97x..1.03x | 0.98x | pass |
   | encode | 3,261 ns/section | 4,257 ns | 1.34x | 1.26x..1.40x | 1.45x (upper 1.47x) | pass |
   | frames | 2,980 ns/frame | 2,904 ns | 0.99x | 0.96x..1.01x | 1.00x | pass |
   | messages | 631 ns/message | 861 ns | 1.36x | 1.33x..1.40x | 1.64x (upper 1.68x) | pass |

   Instructions, five batches (callgrind): encode 71.8 M against Rust's
   63.7 M (1.13x), messages 72.2 M against 57.5 M (1.25x; 1.63x before).
3. **Tests.** No test was deleted. The three unit tests above were
   converted.

Verdict: **flip.** Rust deleted (2,679 lines): the Rust build of QPACK
(decoder 420 lines, encoder 393, the table code in `qpack/mod.rs` 148,
Huffman 151 and its 265-line table, prefix 120, the static table 115 and
the wire constants 65), frames 321, priorities 329, capsules 151 and the
message rules 201. `aspect crate prism-h3 gen-tables` now
writes the Vesper tables (`vesper/src/static_table.ves`,
`huffman_codes.ves`) straight from the RFC 9204 text and the h2 crate's
Huffman table, so the Rust tables and `vesper/gen_tables.py` are gone (its
output for the committed tables is unchanged).

What stays Rust: the adapters that turn the public types into the port's
flat buffers and back (`src/qpack/port.rs`, the forwarding halves of
`frame.rs`, `priority.rs`, `capsule.rs`, `message.rs`), `error.rs`, and the
drivers (`connection.rs`, `client.rs`, `session.rs`, `transport.rs`,
`warp.rs`, `prism.rs`), which are part 3.

### What changed

- **Message rules.** The field-name, field-value and pseudo-header scans
  use `@vesper/prism-bytes`, which gained `first_nul_or_line_break`,
  `first_not_visible` and `first_not_lower_token`. The adapter puts each
  field's start offset in `meta` (the third word was the unused `sensitive`
  flag), so the port no longer builds a starts sequence per message, and it
  reuses per-thread buffers for the flattened fields.
- **`@vesper/prism-bytes`** reads a slice's last bytes as one more word: the
  final eight bytes (overlapping bytes already found clean), or for a slice
  under eight bytes its bytes padded with `a` (two overlapping 4-, 2- or
  1-byte reads). Before, the tail went to the scalar `find_not_in`. The
  exact scans (controls, line breaks, invisible bytes) use the classic
  first-match test `(x - 0x01..) & ~x & 0x80..`, whose lowest flag is exact,
  instead of the 7-bit-lane tests. Prism's decoder and response scans use
  the same code.
- **Huffman encoding** grows the output once to the encoded length (already
  computed to choose between Huffman and raw) and writes the code bits 32
  at a time with `try_write_u32_be`, instead of one `push` per byte.
- **The static table's name hash** mixes the name's first and last words
  and its length instead of an FNV-1a step per byte.

### The flip

- The `vesper-port` feature is gone; `vesper-port-abi` and
  `vesper-port-harness` are plain dependencies, and `build.rs` always builds
  the library.
- Bazel: the default crate target runs `build.rs` with the prebuilt object
  (`VESPER_PORT_OBJECT_H3`) and checks the generated binding, so every test
  file runs against the port as in Cargo. The per-file `port_*_test`
  targets are gone; the divergent binaries remain. The package depends on
  `//crates/vesper-prism:bytes_package`.
- `scripts/bench.sh` builds the Rust baseline from `7dd51a8` in a temporary
  worktree, with this branch's `examples/abba.rs`.

### Compiler gaps (part 2)

5. **`Option` results of `try_read_*` and `try_get` go through the stack.**
   Each read of a word in a scan loop stores the tag and value to a stack
   slot and reloads them (about 15 instructions where Rust has one load).
   The scans here pay it once per word.
6. **Loop-invariant setup is hoisted into every call.** A function whose
   short-slice branch needs a few shifted constants computes them in the
   prologue even when the branch is not taken (about 50 instructions per
   call of `first_not_lower_token` before its short path moved out of the
   loop).
7. **Register pressure spills scalar loops.** The FNV loop inlined into the
   encoder's `field_line` reloaded and stored seven values per byte
   (27 instructions per byte).

## Part 3: the drivers

The drivers are a sans-I/O state machine over the `Transport` trait, which
Warp, quinn and the test transports implement in Rust. Warp's connection
is not ported (lane 4), and the compiler cannot yet call Rust through a
callback (#1296), so the Vesper code cannot call `Transport` itself.

The plan, one PR per piece:

- The Vesper code owns the protocol state and decides.
- It returns typed commands, such as STOP_SENDING, bytes for a QPACK
  codec, or a stream handed to the session layer.
- The Rust driver carries the commands out against the transport.

The Rust that stays is the transport glue and the public API types, such as
the linear `Responder`. It shrinks as each piece moves.

### 3a: the peer's unidirectional streams and control stream

- **What moved.** `vesper/src/control.ves` (`UniCore`) now holds the
  peer's unidirectional streams, in stream ID order. It does what
  connection.rs's `Uni` map, `accept_uni`, `uni_bytes`, `uni_closed` and
  `control_frame` did:
  - It routes each stream by its type varint. The control stream, the two
    QPACK streams and WebTransport streams are recognised; push streams
    are an error; unknown types get STOP_SENDING.
  - It holds at most 16 bytes of a stream whose type has not arrived.
  - It owns the control stream's frame reader and its rules (§6.2.1, §7):
    - SETTINGS must come first.
    - A server's GOAWAY must be a client bidi ID, and GOAWAY never
      increases.
    - MAX_PUSH_ID never decreases.
    - CANCEL_PUSH beyond MAX_PUSH_ID is an error.
    - PRIORITY_UPDATE must name a client bidi stream and never a push.
- **What the Rust driver still does.** It carries out the commands:
  - feeds the QPACK codecs;
  - records the peer's SETTINGS and GOAWAY;
  - bounds and stores priority updates;
  - hands WebTransport streams to the session layer;
  - sends STOP_SENDING.
- **The shared error type.** `ConnectionError` and `TransportError` are
  now Vesper types too, variant for variant, so the binding returns the
  Rust enums directly.
- **Tests.**
  - Every test passes: h3_mem's control-stream and GOAWAY tests,
    priority_bodies, and the WebTransport stream tests through the router.
  - `bad_priority_updates_are_connection_errors` now also covers a
    server-initiated bidirectional element ID (1). Before, it had only
    unidirectional IDs, which the stream-direction check alone rejects.
  - The divergence check gains a `control` module. Dropping the
    server-initiated check from PRIORITY_UPDATE fails that test, and
    `priority_update_overrides_the_header` still passes.
- **Rust lines.** 182 deleted and 124 added, so 58 fewer, not counting the
  generated binding. The added lines are the command executor and the
  boundary types.
- **Speed.** No ABBA workload goes through the router. Its hot path is
  QPACK encoder-stream instructions, which are copied once into `out` and
  read back. A connection-level workload in `examples/abba.rs` comes with
  part 3b, the request streams, where the per-request cost is.

## Part 3b: HTTP/3 connection, client, and WebTransport session state machines

### Flip-gate verdict

- **Full function.** Every crate test binary (`lib` unit tests, `frame_codec`,
  `message_rules`, `priority_bodies`, `h3_mem`, `webtransport_mem`, `robustness`,
  `qpack_rfc`, `qpack_qifs`, `qpack_encode_golden`, `qpack_tables`, `quinn_datagrams`,
  `warp_prism`, `warp_webtransport`, `wtransport_interop`, `port_differential`,
  and `divergence`) passes, along with all 22 Vesper Loom unit tests.
  `webtransport_mem` adds `stream_headers_wire_encoding`, moving the deleted
  `session::tests::stream_headers` check onto the public API.
- **Differential.** `tests/port_differential.rs` compares 7 scenario suites
  (`req_resp_matrix` 1,944, `streaming_and_backpressure` 358,
  `priority_scheduler` 324, `goaway_and_lifecycle` 87, `raw_wire_errors` 55,
  `wt_sessions_and_streams` 142, `capsule_and_datagram_fuzz` 93; **3,003** cases
  total) against frozen golden digests recorded at `0f5a9eba`: **0 differences**.
- **Divergence.** `build.rs`, `BUILD.bazel`, `port/check.sh`, and
  `tests/divergence.rs` gain divergent edits for `session`, `core`, `server`,
  and `client` alongside the 11 existing modules (`huffman`, `prefix`, `table`,
  `encoder`, `decoder`, `varint`, `frame`, `priority`, `capsule`, `message`,
  `control`); each fails its guarding test while an unrelated test stays green.
- **Speed (`RUSTFLAGS="-C target-cpu=native"`, `n = 30` ABBA epochs vs `0f5a9eba`):**

  | Workload | Rust | Vesper | Ratio | 95% interval | Bar (upper <= 1.50x) |
  |---|--:|--:|--:|---|---|
  | `connection` | 26,416 ns/exchange | 25,758 ns/exchange | 0.97x | 0.966x..0.983x | pass |
  | `session` | 62,074 ns/session | 59,182 ns/session | 0.94x | 0.927x..0.965x | pass |

- **Rust deleted.** `git diff --shortstat 0f5a9eba` across
  `src/{session,connection,client}.rs`: **2,798 deletions, 1,172 insertions**
  (1,626 net lines removed; `src/session.rs` shrank from ~700 lines to 87 lines
  of public types). Including `src/qpack/port.rs` ABI/table glue (`+65/-2`):
  2,800 deletions, 1,237 insertions.

### What moved to Vesper (`@vesper/prism-h3`)

- **`vesper/src/pool.ves` (`BytePool`, `OutMap`, `CmdBuf`, `SevBuf`, `HevBuf`).**
  Size-classed block arena (`256`, `1024`, `4096`, `16384`, `65536` byte blocks)
  using `try_copy_from` / `try_copy_within` for zero-heap-churn stream send queues,
  pooled `FrameReader`/`CapsuleReader` boundaries, sorted `u64` lookup helpers,
  and flat FFI command/event staging buffers.
- **`vesper/src/session.ves` (`SessionTable`).** Extended CONNECT sessions
  (`WebTransport` / `Tunnel`), held streams for racing `accept_session` (16-stream
  bound, `WT_BUFFERED_STREAM_REJECTED`), held datagrams, QUIC vs capsule HTTP
  datagram dispatch, `DRAIN_WEBTRANSPORT_SESSION` / `CLOSE_WEBTRANSPORT_SESSION`
  (UTF-8 and 1024-byte reason checks), and `WT_SESSION_GONE` stream resets.
- **`vesper/src/core.ves` (`ConnCore`).** Shared connection state, control/QPACK
  uni-stream handling (directly invoking `dec_on_encoder_stream` and
  `enc_on_decoder_stream`), `flush_begin`/`flush_step` state machine driving
  `priority::schedule_into` + RFC 9218 non-work-conserving turns, header bounds,
  and field/trailer serialization into `BytePool`.
- **`vesper/src/server.ves` (`ServerCore`) & `vesper/src/client.ves` (`ClientCore`).**
  Server request lifecycle, streaming upload window + `max_connection_buffer`
  eviction, interim/chunked/whole response framing, leaked-responder resets,
  client request/response parsing, bidirectional `WEBTRANSPORT_STREAM` sniffing,
  strictly decreasing `GOAWAY`, and `ClientError` checks.
- **Rust boundary.** `RoleCore` in `src/connection.rs` shares `flush`,
  `open_local`, `read_uni`, and WebTransport stream/datagram `Transport` loops
  between `ServerConnection` and `ClientConnection`. All Rust remaining in
  `src/{connection,client,session}.rs` is purely public API types, linear
  `Responder`/`Session` drop obligations, and `Transport` trait I/O execution.

## What is left

- `src/warp.rs` (`Warp` transport adapter awaiting lane 4 `vesper-warp`),
  `src/prism.rs` (`vesper_prism::service::Service` bridge), `src/transport.rs`,
  `src/error.rs`, `src/qpack/port.rs`, and the thin `Transport` glue / linear API
  wrappers in `src/{connection,client,session}.rs`.
- The census row (`scripts/rust-census.py`, STATUS tables) is updated after
  landing on `main`.
