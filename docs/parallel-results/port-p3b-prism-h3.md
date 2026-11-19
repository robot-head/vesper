# Port P3b lane 5: vesper-prism-h3 (#1067)

Status: part 1 of 3 (QPACK). The Rust build stays the default. The Vesper build is behind the non-default `vesper-port` feature, per the standing parity rule.

| Part | Scope | State |
|---|---|---|
| 1 | QPACK: Huffman code, prefixed integers, static and dynamic tables, encoder, decoder (`src/qpack/**`) | this PR |
| 2 | `frame.rs`, `priority.rs`, `capsule.rs`, plus a frame-parsing ABBA | next |
| 3 | `message.rs`, `session.rs`, `error.rs` | last, closes #1067 |

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

## Compiler gaps

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
