# Port P3b lane 5: vesper-prism-h3 (#1067)

Status: all three parts. Part 1 is QPACK, part 2 frames, priorities and capsules, part 3 message validation. `error.rs` and `session.rs` stay Rust, for the reasons below. The Rust build stays the default. The Vesper build is behind the non-default `vesper-port` feature, per the standing parity rule.

| Part | Scope | State |
|---|---|---|
| 1 | QPACK: Huffman code, prefixed integers, static and dynamic tables, encoder, decoder (`src/qpack/**`) | #1075 |
| 2 | `frame.rs`, `priority.rs`, `capsule.rs`, plus a frame-parsing ABBA | part 2 PR |
| 3 | `message.rs`. `session.rs` and `error.rs` are assessed and stay Rust (see part 3). | part 3 PR, closes #1067 |

## Layout

- `crates/vesper-prism-h3/vesper/` is the Loom package `@vesper/prism-h3`, about 3,200 lines in 10 files:
  - `int.ves`: helpers;
  - `huffman.ves`, `huffman_codes.ves`: the Huffman code;
  - `prefix.ves`: prefixed integers and strings;
  - `table.ves`, `static_table.ves`: the static and dynamic tables;
  - `encoder.ves`, `decoder.ves`: the encoder and decoder;
  - `reason.ves`: error mapping;
  - `lib.ves`: the exported root.

  `vesper/gen_tables.py` regenerates `huffman_codes.ves` and `static_table.ves` from the Rust tables.
- `build.rs` compiles the package with `vesper_port_harness::Port`. The binding is committed at `port/binding.rs` (`VESPER_PORT_BLESS=1` regenerates it), and `--rust-types crate::qpack::port::abi` maps the copy types onto the crate's own `Reason`, `QpackError`, `HuffmanError`, `PrefixError`, `EncoderConfig`, `DecoderConfig` and `EncoderStats`.
- `src/qpack/port.rs` is the only adapter. Field lists go in as one byte buffer plus three words per field. Decoded sections come back through the decoder's `Sections` output buffers. Each call writes 4-word records (ready, too large, cancel, direct result), and the Rust wrapper applies them to its ready queue.
- The Huffman and static tables are built once per process (a `OnceLock<Huff>`, matching the Rust build's statics) and passed `in` to every codec call.
- `deny(unsafe_code)` covers the crate in the port build; only the generated binding module is allowed unsafe. `forbid(unsafe_code)` stays in the Rust build.

## API inventory (part 1)

The public API is unchanged in both builds.

| Item | Port build |
|---|---|
| `qpack::Encoder` (`new`, `apply_peer_settings`, `encode`, `on_decoder_stream`, `take_encoder_stream`, `stats`, `insert_count`, `known_received_count`, `table_capacity`, `outstanding_sections`, `blocking_streams`) | Vesper `EncoderCore` |
| `qpack::Decoder` (`new`, `config`, `on_encoder_stream`, `decode_section`, `poll_unblocked`, `cancel_stream`, `take_decoder_stream`, `insert_count`, `table_size`, `table_capacity`, `table_len`, `blocked_streams`) | Vesper `DecoderCore`, ready queue in Rust |
| `qpack::huffman::{encoded_len, encode, decode}` | Vesper `Huff` methods |
| `qpack::prefix::{decode_int, encode_int, decode_string, encode_string}` | Vesper `Huff` methods |
| `Field`, `FieldSection`, `Decoded`, `Reason`, `QpackError` (and `code()`), configs, stats | Rust types, shared by both builds |
| `qpack::live_allocations()` | port build only, for leak tests |

What stays Rust: `connection.rs`, `client.rs`, `transport.rs`, `warp.rs` and `prism.rs` (the driver, per the lane prompt), plus the not-yet-ported part 2 and 3 modules.

## Unit tests

- **Run in both builds:** the `prefix` and `huffman` unit tests.
- **Rust only:**
  - the `qpack/mod.rs` tests for Required Insert Count arithmetic and the static-table spot checks;
  - `code_is_canonical_and_complete`, which inspects the Rust table type.

  These test private Rust items that the port replaces. The integration tests cover the same behavior.
- **Integration tests against the port:** `qpack_rfc`, `qpack_qifs` (the committed QIFS subset, plus the full corpus when available), `qpack_encode_golden`, `robustness`, `h3_mem`, `priority_bodies` and `webtransport_mem`. The interop tests with Warp, quinn and wtransport peers also pass against the port in Cargo.
- **New `qpack_encode_golden`:** the encoder's output on the netbsd QIF under six (capacity, blocked streams, ack cadence) configurations. The Rust build wrote `tests/fixtures/qpack-golden/`; both builds must match it byte for byte, and the decoder must read it back.

## Divergence checks

Each ported module has one wrong edit (`DIVERGENT_EDITS` in `build.rs`, the `DIVERGENT` dict in `BUILD.bazel`). The test that guards the module must fail against it, and a control test must still pass. All five are caught. `port/check.sh` runs them in the Linux "Vesper ports" job, and `:port_divergence_detected` runs them in Bazel.

| Module | Edit | Guarding test fails | Control passes |
|---|---|---|---|
| huffman | no EOS padding bits | `appendix_b_encoder_side` | `appendix_b_decoder_side` |
| prefix | `used` off by one | `committed_qifs_subset_decodes` | `full_qifs_corpus_when_available` |
| table | `first` absolute index off by one | `appendix_b_decoder_side` | `appendix_b_encoder_side` |
| encoder | Required Insert Count encoded without +1 | `appendix_b_encoder_side` | `appendix_b_decoder_side` |
| decoder | post-base index off by one | `committed_qifs_subset_decodes` | `full_qifs_corpus_when_available` |

## ABBA

`examples/abba.rs` with `vesper-bench` `run_abba` (12 epochs of A/B/B/A, 16 batches, with a digest check). Instruction counts are from callgrind over 5 batches. Run with `CALLGRIND=1 sh crates/vesper-prism-h3/scripts/bench.sh`, on the cloud container, Linux x86_64, release builds.

The workloads:
- **decode:** 70 QIFS encoded files, 1,260 sections, each through a fresh decoder.
- **encode:** 18 netbsd blocks × 4 configurations × 4 rounds = 288 sections.

| Workload | Rust instructions | Vesper instructions | Instruction ratio | Wall-clock median (95% interval) | 2x gate |
|---|---|---|---|---|---|
| decode | 171.27M | 179.07M | 1.05x | 1.29x (1.27–1.47x) | Pass |
| encode | 62.24M | 82.10M | 1.32x | 1.41x (1.32–1.49x) | Pass |

How it got there:

| Change | Decode instructions | Encode instructions |
|---|---|---|
| First port | 3.7x | 4.0x |
| Huffman decoding as a 4-bit state machine (two table reads per byte instead of a canonical-code walk per bit), and tables built once and shared instead of per codec | 1.04x | 2.8x |
| Static table name index: FNV-1a buckets plus a same-name chain, replacing two linear scans of the 99 entries for every field | 1.05x | 1.32x |

Not at parity. Rust stays the default. The remaining encode gap is mostly the dynamic table's newest-first scans (`dynamic_exact`, `dynamic_name`). The Rust build does the same scans, but in Vesper each step pays a slice read of `lo`/`nlen`/`vlen` plus bounds checks. Decode wall-clock is above its instruction ratio. This is probably allocation: each decoded field is copied into the output buffers, and Rust then copies it into `Field`s.

## Part 2: frames, priorities, capsules

New Vesper files:
- `varint.ves`: QUIC varints.
- `frame.ves`: SETTINGS and the frame reader.
- `priority.ves`: the RFC 8941 dictionary subset, PRIORITY_UPDATE and the §10 scheduler.
- `capsule.ves`: the capsule reader and a strict UTF-8 check for close reasons.

The Rust API is unchanged, except that `PriorityUpdate` now derives `Copy` (all its fields are copy).

| Item | Port build |
|---|---|
| `frame::{put_varint, write_frame, write_data_header, write_varint_frame}` | The header bytes come from Vesper and the payload is appended in Rust. |
| `frame::Settings::{parse, encode}` | `Settings` maps onto the Vesper copy struct, including its `Option` fields. |
| `frame::FrameReader` (`new`, `with_webtransport`, `push`, `next_frame`, `take_rest`, `at_boundary`, `buffered`, `finish`) | Vesper `FrameReaderCore`. The reader keeps unread bytes from `head` on and compacts on push, so a frame is moved at most once. DATA and HEADERS bytes are read from its `out` buffer. |
| `priority::{Priority::parse, Priority::serialize, PriorityUpdate::{parse, encode}, schedule}` | Vesper. `schedule` passes (id, urgency, incremental) triples in and gets back group lengths and ids. |
| `capsule::{CapsuleReader, write_capsule, write_close_session}` | Vesper `CapsuleReaderCore`. |
| Constants, the `const fn` predicates (`is_http2_frame`, `is_http2_setting`, `is_reserved`), `FrameError::code`, `stream_type` | Rust in both builds. Constants and `const fn`s cannot call into the library, and `stream_type` is one `vesper_warp::varint::decode` call. |

The Rust reader's `data_started` field was never read, so the port drops it.

Tests:
- The frame, priority and capsule unit tests run in both builds.
- New `tests/frame_codec.rs` runs in Cargo and Bazel. It covers:
  - frames split at every byte, including every varint width;
  - SETTINGS wire bytes;
  - WebTransport stream signals;
  - the RFC 9218 examples and the scheduler;
  - capsules split at every byte;
  - malformed UTF-8 in close reasons (overlong, surrogate, above U+10FFFF, truncated, stray continuation byte).
- The end-to-end tests (`h3_mem`, `priority_bodies`, `webtransport_mem`, the interop tests) now run the driver over the ported frames, priorities and capsules.

Divergence checks for part 2. Each guarding test fails and each control test passes. Part 1's controls moved off `frame_reader_survives_random_input`, which now runs ported code.

| Module | Edit | Guarding test fails | Control passes |
|---|---|---|---|
| varint | 64 written in one byte | `frames_round_trip_split_anywhere` | `rfc9218_priorities` |
| frame | one-varint payloads accept trailing bytes | `malformed_frames_are_typed_errors` | `capsules_round_trip_split_anywhere` |
| priority | urgency 8 accepted | `rfc9218_priorities` | `malformed_frames_are_typed_errors` |
| capsule | close code's high byte shifted | `capsules_round_trip_split_anywhere` | `appendix_b_decoder_side` |

### ABBA, all workloads

Instruction counts are now the difference between 6 and 1 batches, so setup (loading the corpus, building inputs and tables) is not counted. This moves the part 1 figures slightly. All numbers below are from one run of `CALLGRIND=1 sh crates/vesper-prism-h3/scripts/bench.sh` after part 3.

The workloads:
- **frames**, per batch:
  - 64 request streams, each HEADERS plus four 1–16 KiB DATA frames plus a grease frame;
  - a control stream with SETTINGS, 256 PRIORITY_UPDATEs and GOAWAY. Each update is also serialized and re-parsed.
  - 512 capsules, mostly DATAGRAM, with unknown ones and a close.

  Each stream is pushed in 1,200-byte pieces.
- **messages** (part 3): the netbsd QIF's 18 field lists, validated as requests 64 times per batch. These are real browser requests. Most carry `connection: keep-alive`, so most are rejected as connection-specific partway through their fields, exactly as in the Rust build.

| Workload | Rust instructions | Vesper instructions | Instruction ratio | Wall-clock median (95% interval) | 2x gate |
|---|---|---|---|---|---|
| decode | 175.61M | 175.19M | 1.00x | 1.03x (0.96–1.18x) | Pass |
| encode | 63.67M | 79.13M | 1.24x | 1.48x (1.38–1.80x) | Pass |
| frames | 59.47M | 61.67M | 1.04x | 1.03x (0.99–1.08x) | Pass |
| messages | 57.55M | 93.92M | 1.63x | 1.79x (1.75–1.86x) | Pass |

Decoding and frame parsing are at parity. QPACK encoding and message validation are not, so Rust stays the default build.

### Compiler gaps found in part 2

7. **Enum variant names shadow struct names in struct literals.** A copy enum `FrameOut { Settings(Settings), ... }` makes `Settings { ... }` resolve to the variant:

   ```
   pub copy struct S { a: u64 }
   pub copy enum E { S(S) }
   pub fn mk() -> S { S { a: 1 } }
   ```

   fails with `constructor 'S' requires 1 fields`, `unknown field 'a'`. The port names the variants `SettingsFrame` and `PriorityUpdateFrame`.
8. **`inout ChunkSeq[u8]` from Rust copies the whole buffer in and out on every call.** `vesper_port_abi::with_seq_mut` does `seq_from`, then the call, then `seq_take`. An export that appends a few bytes to a caller's growing output buffer is therefore O(buffer) per call. Building the 64-stream workload through such writers cost 72M instructions, against 8M in Rust. The port's writers return the bytes they write, and Rust appends them. A borrowed-`Vec` `inout` handle (reserve, write in place, set the length) would remove the copies.
9. **Integer literals in a tuple do not take their type from use.** In `let (lo, hi) = if c == 224 { (160, 191) } else { (128, 191) };`, `lo` and `hi` are `i64`, so passing them as `u8` fails (E0200). Annotating each binding works.

## Part 3: message validation; session.rs and error.rs

`message.ves` ports `parse_request`, `parse_response` and `check_trailers` with their rules:
- token and value checks;
- connection-specific fields and `te`;
- pseudo-header order, duplicates, names and values;
- the extended-CONNECT rules;
- `:authority` against `host`;
- the http/https authority requirement;
- `content-length`;
- `:status`.

The adapter passes the field list in the encoder's flattened form, and Vesper returns the validated head as byte ranges (`RequestParts`, `ResponseParts`). Rust then builds the `String`s; the UTF-8 was already checked in Vesper with the capsule reader's strict check. Rust also drains the pseudo-header fields, which always come first, so the regular `Field`s move into the head without a copy. The checks run in the same order as in the Rust build, so the same input gives the same error.

The byte classes use `find_not_in`. Literal names are compared only after a length test, so a literal is never built for a name it cannot match.

Tests:
- The unit tests run in both builds.
- New `tests/message_rules.rs` runs in Cargo and Bazel. It holds the unit cases plus content-length bounds, the Host rules, pseudo-header bytes and UTF-8, statuses and trailers.
- Divergence: the edit `hosts > 1` → `hosts > 2` (two Host fields accepted) fails `content_length_and_host_rules`, and the control `response_statuses` passes.

The remaining gap (1.63x instructions) is the byte scans. `find_not_in` is a scalar loop on the x86-64 baseline target (it needs SSSE3; see the byte-classes lane), about 7 instructions per byte. Rust's `iter().all(token)` and its value scan compile tighter.

**`session.rs` stays with the Rust driver.** Every function in it is a method of `connection::Core` over `impl Transport`, for example:
- `adopt_stream`, `session_data`, `poll_datagrams`, `send_session_datagram`;
- `open_wt_stream`, `wt_read`, `wt_write`, `wt_reset`.

Each one reads, writes or resets transport streams in the middle of its bookkeeping. Its codec work already runs in the port: capsules, the CLOSE capsule, varints and stream headers (`session_header` is two `put_varint` calls). Moving the bookkeeping needs a callable transport across the export boundary: effectful `dyn` parameters, which are P3b compiler lane 1 (#1063). The driver files the prompt keeps in Rust (connection, client, transport, warp, prism) would also have to come along. It is a follow-up once #1063 lands.

**`error.rs` stays Rust.** `Code::webtransport`, `Code::webtransport_code` and `Code::is_reserved` are `pub const fn`. A const fn cannot call into the library, and dropping `const` would break the public API. `Code::name` returns `&'static str`s from a table, and `Display` formats them. Porting the table would only hand indices back to the same Rust strings.

### Compiler gaps found in part 3

10. **Struct literals are not allowed in an `if` condition**, even as call arguments: `if same(in b, Span { lo: a, hi: b }, s) { ... }` fails to parse (E0002). Bind the literal first.
11. **No `continue`.** Invert the condition and nest the loop body.
12. **A slice view cannot be a method receiver or a `let`.** `b[lo..hi].eq(in x)` and `let st = s.as_slice();` are refused (E1521, a slice is a second-class borrow). A wrapper function taking `in Slice[u8]` works.

## Compiler gaps found in part 1

1. **`wrapping_shl_u64`/`wrapping_shr_u64` assigned to a loop variable fails ownership validation after optimization.** Repro (`vesper build --lib`):

   ```
   pub fn pack(n: u64) -> u64 {
       let mut acc: u64 = 0;
       for i in 0..n {
           acc = wrapping_shl_u64(acc, 5);
       }
       acc
   }
   ```

   ```
   optimized IR failed ownership validation: E0410 IR access to '$slot1_acc' overlaps active loan '$slot1_acc'
   ```

   `wrapping_add` in the same position works, and so does `<<` behind a range check. The workaround is the `shl`/`shr` helpers in `int.ves` (`if s < 64 { x << s } else { 0 }`).
2. **No qualified calls into a submodule** (`self::prefix::decode_int(...)`), and no `use ... as`. Every submodule function needs a unique name, so the encoder and decoder entry points are `enc_*` and `dec_*`.
3. **Integer-literal ranges are `i64`.** `for i in 0..99` cannot index with a `u64`, so bounds are wrapped in `u64` functions (`static_count()`, `buckets()`).
4. **`Result::Ok(())` does not parse as a pattern.** The port uses `Result::Ok(_u)`.
5. **No `u8` → `u64` cast syntax.** The port uses `widen_u64`.
6. **No fixed arrays or static data.** The Huffman, trie and static tables are `ChunkSeq`s built at run time, and every read is a checked `try_get`. A `const` array with bounds-check elimination would remove the `word()`/`byte()` fallbacks from the hot loops.

## Std candidates

- `remove_word` (remove at an index in a `ChunkSeq[u64]`) and `drain_front` (drop a prefix of a `ChunkSeq[u8]`), both written in `int.ves` with `try_copy_within` and `truncate`.
- A string hash (FNV-1a over a `Slice[u8]`), written in `table.ves`.
