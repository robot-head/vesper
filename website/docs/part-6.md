# 30 Wire formats and JSON serialization

A decoder can lend a view of its input, allocate a decoded object, or write directly into a column. The choice determines who owns the output, how long it stays valid, and whether the decoder has to copy the bytes.

<!-- technical-detail -->

A decoder can borrow slices from immutable input bytes, bump-allocate escaped payloads in a lexical arena, or stream decoded fields straight into columnar tiles. Choosing that ownership contract upfront fixes how long decoded views stay live, whether global heap allocation is possible, and how partial failures clean up already-constructed protocol handles.

Part VI opens with Vesper Wire and Json across `@vesper-wire/serde`, `@vesper-wire/json`, `@vesper-wire/archive`, and `@vesper-wire/compact`, backed on the host by `crates/vesper-serde`, the `vesper json` CLI validator, and the Lean 4 models `lean/SimdJsonRoundtrip.lean` and `lean/SimdScanner.lean`. Per `REVIEW-STATUS.md` (`REPO-03`, `COMP-01`..`COMP-05`, and `SYS-01`..`SYS-06`), `crates/vesper-serde` validates immutable inputs through `IndexedJson` today while source-level `@derive(JsonDecode, JsonEncode)` lowering, region escape/rewind verification, and multi-GB/s zero-heap throughput targets remain workload-specific obligations rather than universal compiler guarantees.

## 30.1 Choose what the decoder owns

An unescaped JSON string can sometimes refer directly to input bytes. Escaped text needs decoding and suitable storage. A region can keep decoded values alive for one batch. If a composite decode fails after constructing a linear field, the error path still has to discharge that field.

<!-- technical-detail -->

Borrowing unescaped JSON strings in conventional Rust `serde` infects structs and trait bounds with viral `'de` lifetimes, whereas falling back to `Cow<str>` silently allocates on the global heap whenever a backslash appears. Vesper replaces `'de` with `copy struct JsonStr[r]`, a 16-byte handle that borrows unescaped UTF-8 directly from the immutable `ByteSlice` input and bump-allocates unescaped bytes inside the caller's `r: Region` (`VirtualArena`) only when Stage 1 observed an escape character, while `[T-RegionScope]` prevents `JsonStr[r]` from escaping the enclosing `Region::scoped` block.

Compile-time `@derive(JsonDecode, JsonEncode)` expansion runs inside Vesper Forge's fuel-metered `wasm32-wasip2` sandbox (`macro_emits_effects = []`), compiling short ASCII object keys up to 7 bytes plus the closing quote into SWAR 64-bit integer mask-and-compare switches and precomputing `JsonSizeBound` byte ceilings. When decoding a composite record that contains `Mode::Linear` fields, Chapter 4's `[T-Let]` Resumption Frame Condition forces the generated decoder state machine to discharge or roll back any already-constructed linear fields before propagating `Fail[JsonError]`.

## 30.2 Express storage and effects in a codec

An `Encode` operation writes a representation to an output. A `Decode[r]` operation can tie an output view or allocation to region `r`. The caller must keep its backing storage live. Generic effect rows allow a codec to express the operations its input, output, or allocator requires.

<!-- technical-detail -->

A single pair of colorless Vesper Pact traits (`ByteOut`, `Encode`, and `Decode[r: Region]`) spans fixed Domain-1 DMA buffers, growing `VirtualArena` scratchpads, and suspending Vesper Pulse `io_uring` byte streams by parameterizing the byte sink over an arbitrary lexical effect row `r: eff`:

~~~vesper
trait ByteOut[r: eff = []] {
    fn write_window[P: IsaProfile](
        inout self, min_cap: usize, writer: fn(inout ByteSlice) -> usize eff [Isa[P]],
    ) -> Unit eff [Isa[P] | r];
}
trait Encode {
    fn encode_json[W, P: IsaProfile, r: eff](in self, inout out: W) -> Unit eff [Isa[P] | r]
    where W: ByteOut[r];
}
trait Decode[r: Region] {
    fn decode_json[P: IsaProfile](
        in scratch: RegionCap[r], inout cursor: SimdJsonCursor[r],
    ) -> Result[Self, JsonError] eff [r: Region, Isa[P], Fail[JsonError]];
}
~~~

When `cursor.read_str(in scratch)` inspects a string token whose Stage 1 backslash mask is zero, it validates UTF-8 in SIMD lanes and returns `JsonStr[r]` aliasing the input buffer directly. When escapes are present, `scratch` supplies arena storage without adding `Alloc` to `eff [...]`, and `[T-RegionScope]` rejects any attempt to store that `JsonStr[r]` in longer-lived shard state.

## 30.3 Parse JSON in two stages

The first pass locates structural characters while tracking quoted strings and escapes. A second pass interprets those positions as objects, arrays, and values. A comma inside a quoted string is data. The vectorized scan must track whether it is inside a string even across a group of bytes processed together.

<!-- technical-detail -->

Stage 1 scans 64 bytes per iteration with safe predicated `VVec[P, u8]` operations (`eff [Isa[P]]` across `NEON`, `AVX2`, `AVX-512`, `SVE2`, and `RVV 1.0`). It propagates odd-length backslash runs across 64-byte boundaries using the `0xAAAA_AAAA_AAAA_AAAA` even/odd carry recurrence, turns unescaped quote pairs into contiguous string-interior 1-runs via a single carryless multiply prefix-XOR (`VPCLMULQDQ` on `x86_64`, `PMULL` on AArch64, or `clmul` on RISC-V), validates UTF-8 via dual-nibble `swizzle` lookup tables, and compresses structural token offsets (`{`, `}`, `[`, `]`, `:`, `,`) outside strings into a `u32` tape:

~~~vesper
fn index_json_block_64[P: IsaProfile, r: Region](
    in buf: ByteSlice, offset: usize,
    inout prev_in_string: u64, inout prev_is_escaped: u64,
    inout tape_out: RegionSliceMut[r, u32], inout tape_len: usize,
) eff [Isa[P]] {
    let mask = P::active_mask[u8](offset, buf.len());
    let v = VVec::load_masked(in buf, offset, mask, 0x20);
    let bs_bits = v.simd_eq(VVec::splat[P, u8](0x5C)).to_bitmask64();
    let quote_bits = v.simd_eq(VVec::splat[P, u8](0x22)).to_bitmask64();
    let escaped = compute_escaped_mask(bs_bits, inout prev_is_escaped);
    let unescaped_quotes = quote_bits & !escaped;
    let in_string = Isa::clmul_prefix_xor[P](unescaped_quotes) ^ prev_in_string;
    prev_in_string = ((in_string as i64) >> 63) as u64;
    let struct_bits = classify_structural_lut[P](v) & !in_string;
    Isa::compress_append_offsets[P](struct_bits | unescaped_quotes, offset as u32, inout tape_out, inout tape_len);
}
~~~

Stage 2 walks the structural tape on demand rather than materializing a heap DOM. Object field keys up to 7 bytes match in three ALU instructions via an unaligned `u64le` word load masked against precomputed constants, unknown subtrees skip in `O(1)` via pre-linked tape jump indices, and decimal floating-point literals decode in roughly 5 ns through the Eisel-Lemire 128-bit `POW5_128` significand multiplier before falling back on ambiguous rounding midpoints.

## 30.4 Decode compressed integers and relative pointers

StreamVByte separates compact length information from integer payload bytes so a vector decoder can process groups. A `RelPtr32` locates data by an offset within an archive. Before constructing a view, validate the offset, length, and extent against the mapped archive.

<!-- technical-detail -->

Fixed-schema JSON encoders coalesce per-field capacity checks into one `ByteOut::write_window` reservation using `JsonSizeBound::Bounded` (`u64` at 20 bytes, `f64` at 24 bytes, `bool` at 5 bytes, plus escaped key overhead). Although the manuscript prose labels shortest-roundtrip float formatting as Dragonbox, the actual Vesper implementation in `crates/vesper-serde/vesper/src/grisu.ves` and `dragon.ves` runs Grisu3 (`alpha = -60`, `gamma = -32` over 81 cached powers of ten, packing up to 17 digits into one `u64`) and falls back on uncertain boundary intervals to the exact 1,280-bit Dragon algorithm over 40 little-endian 32-bit limbs (`Big32x40`).

    Stage 1 (64B VVec[P, u8])                Stage 2 (SimdJsonCursor[r])
    ┌──────────────────────────────┐         ┌───────────────────────────────────┐
    │ bs_bits -> 0xAAAA odd carry  │  u32    │ SWAR u64le key mask-and-compare   │
    │ clmul_prefix_xor quote mask  │─tape──▶ │ O(1) bracket skip + Eisel-Lemire  │
    │ dual-nibble UTF-8 + compress │ (in r)  │ JsonStr[r]: slice | Arena bump    │
    └──────────────────────────────┘         └───────────────────────────────────┘
    // RelPtr32[T] / RelSlice32[T] zero-parse archive (@vesper-wire/archive):
    ┌───────────────────────┬──────────────────────────┬─────────────────────────┐
    │ Archive Root (wire)   │ RelSlice32 { off: +16,   │ Target [T; len] payload │
    │ base @ offset 0       │   len: N } ─────────────▶│ @ self_addr + off       │
    └───────────────────────┴──────────────────────────┴─────────────────────────┘

For binary RPC and storage metadata, `@vesper-wire/compact` implements `StreamVByte`, separating packed 2-bit byte-length control tags from variable-length integer payload bytes so four `32`-bit integers decode in one SIMD `swizzle` instruction. For random-access lookup tables and ASTs, `@vesper-wire/archive` pairs padding-free `wire struct` layouts with signed 32-bit relative offsets (`RelPtr32[T]` and `RelSlice32[T]`), allowing mapped `io_uring` or disk pages to resolve slices in `O(1)` without `Pin` once a single archive-wide offset, alignment, and extent validation pass succeeds.

## 30.5 Ingest a batch into columns

The Serde float formatter uses Grisu3 with a 1,280-bit Dragon fallback. Read newline-delimited JSON into a bounded region, decode each row, and append selected fields to an AoSoA tile. Decide what a malformed row does to the unfinished batch. A round-trip proof compares a value with its decoded encoding. The ingestion path also needs bounds and cleanup checks.

<!-- technical-detail -->

Streaming newline-delimited JSON (`NDJSON`) directly into 16-wide Vesper Strata `tiled(16) struct` (`AoSoA`) columns avoids intermediate row-struct materialization, while checkpointing and rewinding a `Watermark` resets the Stage 1 structural tape in `O(1)` after every parsed line:

~~~vesper
tiled(16) @derive(JsonDecode, JsonEncode)
copy struct TelemetryPoint {
    hot ts_unix_ms: i64,
    hot latency_us: u32,
    hot status_code: u16,
    hot flags: u16,
    cold trace_id_hex: InlineStr[32],
}
fn ingest_ndjson_to_tiles[P: IsaProfile, r: Region](
    in arena: RegionCap[r], in ndjson_dma_buf: ByteSlice, inout dest_tiles: TiledSeq[TelemetryPoint],
) -> Result[u32, JsonError] eff [r: Region, Isa[P], Fail[JsonError]] {
    let mut count: u32 = 0;
    for line_slice in SimdLines::split[P](in ndjson_dma_buf) {
        let mark: Watermark = arena.checkpoint();
        let pt: TelemetryPoint = Json::decode_slice[P, TelemetryPoint](in arena, in line_slice)?;
        dest_tiles.push(pt);
        arena.rewind(sink mark);
        count += 1;
    }
    Ok(count)
}
~~~

The repository checks the schema-roundtrip and SIMD-scanner models in `lean/SimdJsonRoundtrip.lean` and `lean/SimdScanner.lean`, complementing the manuscript's illustrative `odd_backslash_simd_eq_scalar` goal:

    -- lean/SimdJsonRoundtrip.lean & lean/SimdScanner.lean
    theorem telemetry_point_json_roundtrip (pt : TelemetryPoint) :
        decodeTelemetryPoint (encodeTelemetryPoint pt) = .ok pt
    theorem telemetry_point_decode_ignores_unknown_fields ...
    theorem telemetry_point_multibyte_utf8_over_32_bytes_rejected ...
    theorem scan_correct {mem : Mem} {input len : Word} {data : List Byte} (d : Byte) ...
    theorem scan_reads_in_slice {mem : Mem} {input len : Word} {data : List Byte} (d : Byte) ...

Note how `telemetry_point_multibyte_utf8_over_32_bytes_rejected` checks `InlineStr[32]` against UTF-8 byte length rather than character count (17 `'é'` code points occupy 34 bytes and are rejected, whereas 16 occupy 32 bytes and succeed), and `scan_reads_in_slice` proves that 16-byte SIMD block loads plus scalar tail cleanup never read outside `[input, input + len)`.

## 30.6 Select a decoder for the host

The host implementation connects parsing and encoding kernels to storage and instruction-set profiles. Check an accelerated decoder against a simple reference on escaped quotes, truncated inputs, and partial final groups. These cases exercise the transition between scanning and interpreting.

<!-- technical-detail -->

On the Rust host, `crates/vesper-serde` exposes `SwarFieldMatcher`, `compute_escaped_mask_64`, `JsonSizeBound`, and the validated `IndexedJson` tape parser (`IndexedJson::parse` and `IndexedJson::parse_simd`), alongside the read-only `vesper json check FILE [--json]` CLI tool (bounded to 1 MiB regular files without final symlinks). `Limits::default()` sets `max_depth: 64` under a hard container ceiling of `HARD_DEPTH = 128` (`HARD_MAX_DEPTH = 128` during encoding), while `max_index_bytes` (4 MiB by default) splits its budget between reserved Stage 2 token-tape capacity and 4-byte Stage 1 structural/escape offset entries so SIMD and scalar parsers enforce identical rejection boundaries:

    // crates/vesper-serde/src/lib.rs & crates/vesper-serde/src/index.rs
    pub struct SwarFieldMatcher {
        pub field_name: SmolStr, pub field_idx: u16,
        pub swar_word_le: u64, pub swar_mask_le: u64, pub exact_byte_len: u32,
    }
    pub fn compute_escaped_mask_64(bs_bits: u64, prev_is_escaped: u64) -> (u64, u64) {
        if bs_bits == 0 { return (prev_is_escaped, 0); }
        let potential_escape = bs_bits & !prev_is_escaped;
        let maybe_escaped_and_odd = (potential_escape << 1) | 0xAAAA_AAAA_AAAA_AAAAu64;
        let escape_and_terminal = maybe_escaped_and_odd.wrapping_sub(potential_escape) ^ 0xAAAA_AAAA_AAAA_AAAAu64;
        (escape_and_terminal ^ (bs_bits | prev_is_escaped), (escape_and_terminal & bs_bits) >> 63)
    }
    // Host validation CLI (bounded 1 MiB file, rejects duplicate decoded keys and trailing bytes):
    $ vesper json check payload.json --json

# 31 Grammar and canonical binary formats

When bytes are hashed or signed, two byte strings that decode to the same value can cause disagreement. A canonical codec gives each accepted value one representation.

<!-- technical-detail -->

When an application hashes or verifies a signature over wire bytes, two distinct byte sequences that decode to the same logical message enable signature-malleability and request-smuggling bugs. Vesper Grammar pairs canonical binary codecs with linear stream-remainder leases and validated zero-copy Apache Arrow IPC views so accepted wire encodings correspond one-to-one with logical values.

Vesper Grammar lives in `@vesper-parse/grammar` and `crates/vesper-grammar`, paired with the `vesper grammar` CLI inspector and `lean/GrammarNonMalleability.lean`, drawing on Vest (USENIX Security 2025) and EverParse / ASN1* (ACM CCS 2025). Per `REVIEW-STATUS.md` (`GRAM-01`..`GRAM-06`, `COMP-01`..`COMP-05`, and `SYS-05`), Vesper's own surface grammar remains an evolving draft, WASI macro fuel does not prove generated parser termination or bounds-check removal, and constant-time guarantees for `@secret_grammar` depend on downstream compiler backend preservation that remains open.

## 31.1 Specify which byte strings belong to a format

A parser may accept several representations that an encoder would never produce. A non-malleable format requires each accepted representation to be canonical. Secret-bearing formats add a separate requirement on what parsing reveals through timing, errors, or memory access.

<!-- technical-detail -->

Permissive binary decoders (`prost`, `serde_cbor`, or non-strict BER/DER readers) routinely accept overlong LEB128 varints (`0x81 0x00` for `1`), non-minimal length prefixes, or unsorted map keys that a canonical encoder would never emit. Annotating a schema with `@wire_grammar(canonical = true)` targets strong non-malleability over `RegionSlice[r, u8]`: if `parse(b1) == Ok(v)` and `parse(b2) == Ok(v)`, then `b1 == b2` byte-for-byte. For decrypted PKCS#8 keys, TLS 1.3 inner plaintexts, or Vesper Aegis enclave payloads, `@secret_grammar` lowers fixed-width and bounded-padding fields onto Vesper Vault `SecU8` and `SecU64` combinators under `eff [ConstTime]` so Stage 5's Cranelift SSA taint pass rejects secret-dependent branches and table lookups.

Although `@wire_grammar` executes inside Vesper Forge's fuel-metered `wasm32-wasip2` sandbox (`macro_emits_effects = []`), macro fuel bounds only compile-time macro expansion work. Proving that the generated state machine terminates on every input and justifying the elision of redundant slice bounds checks are separate verification obligations per format fragment.

## 31.2 Keep the unconsumed tail

Parsing a field returns a value and a remainder. An `UnconsumedTail` lease makes the caller account for the remaining bytes. In an Arrow column, lengths, validity bits, and offsets must describe compatible ranges. A typed view of values depends on those checks succeeding first.

<!-- technical-detail -->

Silently discarding unparsed trailing bytes after a framed message is a classic vector for HTTP/2, gRPC, and Kafka frame smuggling. Grammar's framed parsers return `(Self, UnconsumedTail)`, where `linear struct UnconsumedTail` carries `Mode::Linear` ownership alongside padding-free 16-byte `wire struct` descriptors for Apache Arrow IPC metadata:

    Authenticated Frame / Stream ByteSlice [0 .. total_len)
    ┌───────────────────────────────────────┬────────────────────────────────────┐
    │ Canonical Wire Prefix [0..next_offset)│ Trailing Slice [next_offset..total)│
    └───────────────────┬───────────────────┴──────────────────┬─────────────────┘
                        ▼                                      ▼
           Msg (canonical bijection:              linear struct UnconsumedTail
         parse(b1)=parse(b2)=Ok(v)=>b1=b2)      ┌──────────────┴───────────────┐
                                                ▼                              ▼
                                      assert_exact_eof()            advance_stream_cursor()
                                   (next_offset == total_len)     (next pipelined frame loop)

~~~vesper
wire struct ArrowBufferDescWire { offset_bytes: i64le, byte_length: i64le }
wire struct ArrowFieldNodeWire { row_length: i64le, null_count: i64le }
linear struct UnconsumedTail { next_offset: usize, total_len: usize }
impl UnconsumedTail {
    fn assert_exact_eof(sink self) -> Result[Unit, ParseError] eff [Fail[ParseError]] {
        let UnconsumedTail { next_offset, total_len } = self;
        if next_offset != total_len {
            Fail::raise(ParseError::TrailingBytesForbidden { remaining: (total_len - next_offset) as u32 });
        }
        Ok(())
    }
    fn advance_stream_cursor(sink self) -> (usize, usize) {
        let UnconsumedTail { next_offset, total_len } = self;
        (next_offset, total_len)
    }
}
~~~

Under Theorem 2 and `[T-Let]`, callers cannot drop `UnconsumedTail`: a single-message envelope must prove `tail.assert_exact_eof()?`, while a pipelined stream loop must consume `tail.advance_stream_cursor()` to hand the exact byte offset to the next frame parser.

## 31.3 Validate before exposing an Arrow view

Read the message metadata, check every referenced buffer, and only then project the selected column. A canonical integer decoder or a Distinguished Encoding Rules (DER) decoder can reject redundant encodings as the bytes arrive. A zero-copy view saves materialization but still needs validation over the borrowed bytes.

<!-- technical-detail -->

Exposing zero-copy columnar buffers from Domain-1 `DmaSlice` memory requires checking signed metadata fields, 64-byte SIMD alignment, and overflow-free byte extents before running predicated `VVec` reductions, just as signed envelope ingress enforces canonical parsing and exact EOF before signature verification:

~~~vesper
fn decode_signed_envelope_strict[r: Region, Msg: VerifiedWireGrammar[r]](
    in arena: RegionCap[r], in signed_frame_bytes: ByteSlice,
) -> Result[Msg, ParseError] eff [r: Region, Fail[ParseError]] {
    let (msg, tail) = Msg::parse_canonical(in arena, in signed_frame_bytes)?;
    tail.assert_exact_eof()?;
    Ok(msg)
}
fn validate_and_sum_arrow_ipc_i64[P: IsaProfile](
    in ipc_body_dma: DmaSlice, in field_node: ArrowFieldNodeWire,
    in validity_desc: ArrowBufferDescWire, in values_desc: ArrowBufferDescWire,
) -> Result[i64, ParseError] eff [Isa[P], Fail[ParseError]] {
    let rows = field_node.row_length.to_native();
    let val_off = values_desc.offset_bytes.to_native();
    let val_len = values_desc.byte_length.to_native();
    if rows < 0 || val_off < 0 || (val_off & 63) != 0 || val_len != rows * 8 {
        Fail::raise(ParseError::MalformedArrowBufferBounds);
    }
    let values_slice = ipc_body_dma.subslice_checked(val_off as usize, val_len as usize)
        .ok_or(ParseError::ArrowTruncatedBody)?;
    let _null_bitmap = ipc_body_dma.subslice_checked(
        validity_desc.offset_bytes.to_native() as usize, validity_desc.byte_length.to_native() as usize,
    ).ok_or(ParseError::ArrowTruncatedBitmap)?;
    Ok(Grammar::simd_sum_nullable_i64[P](in values_slice, rows as usize))
}
~~~

Note that Arrow IPC validation ensures aligned, in-bounds zero-copy views rather than canonical byte uniqueness (null-slot payload bytes and buffer padding can still vary), whereas DER and explicit-presence Protobuf (`vesper.protobuf.explicit-presence/1`) reject non-minimal lengths and redundant wire encodings as bytes arrive.

## 31.4 Prove the two round-trip directions

Decoding an encoded value should recover that value. Encoding an accepted byte sequence’s decoded value should recover the same bytes. The second direction exposes alternate encodings. Together these laws describe a bijection on the supported domain. [GrammarNonMalleability.lean](https://github.com/robot-head/vesper/blob/main/lean/GrammarNonMalleability.lean) mechanizes the result in `canonical_roundtrip_implies_non_malleable` and proves `canonical_leb128_injective` for the modeled LEB128 encoding.

<!-- technical-detail -->

Encoder-after-decoder round-tripping comes in two distinct directions. Proving `parse(serialize(v)) = some v` shows only that valid logical values survive serialization, whereas proving `serialize(v) = b` whenever `parse(b) = some v` rules out alternate wire representations. In `lean/GrammarNonMalleability.lean`, Vesper proves that this second round-trip law implies byte-unique non-malleability and discharges injectivity for the canonical multi-byte LEB128 decoder:

    -- lean/GrammarNonMalleability.lean
    theorem canonical_roundtrip_implies_non_malleable
        {Msg : Type} (parse : ByteArray → Option Msg) (serialize : Msg → ByteArray)
        (h_ser_parse : ∀ b v, parse b = some v → serialize v = b)
        (b1 b2 : ByteArray) (v : Msg) (hp1 : parse b1 = some v) (hp2 : parse b2 = some v) :
        b1 = b2 := by
      rw [← h_ser_parse b1 v hp1, ← h_ser_parse b2 v hp2]
    theorem canonical_leb128_2byte_rejects_overlong (b0 b1 : BitVec 8)
        (h_cont : (b0 &&& 0x80#8) != 0#8) (h_last : (b1 &&& 0x80#8) = 0#8) (h_minimal : b1 != 0#8) :
        ((b0.zeroExtend 16 &&& 0x7F#16) ||| ((b1.zeroExtend 16) <<< 7)) >= 128#16 := by bv_decide
    theorem canonical_leb128_injective :
        ∀ {bs1 bs2 : List Nat} {v : Nat},
          (∀ b ∈ bs1, b < 256) → (∀ b ∈ bs2, b < 256) →
          decodeCanonicalLeb bs1 = some v → decodeCanonicalLeb bs2 = some v → bs1 = bs2

Together `canonical_leb128_2byte_rejects_overlong` and `canonical_leb128_injective` guarantee that values below 128 cannot be smuggled in a two-byte `0x80..0xFF 0x00` sequence and that no two valid byte lists decode to the same natural number.

## 31.5 Check a variable-length integer

LEB128 encodes an integer in groups of seven bits. A continuation bit tells the decoder whether to read another group. It must reject truncation, out-of-range results, and redundant groups when canonical encoding is required. For an Arrow buffer verifier the analogous checks relate recorded offsets to actual buffer lengths.

<!-- technical-detail -->

In `crates/vesper-grammar`, `decode_canonical_u64_leb128` enforces two guards beyond standard continuation-bit scanning: at byte index 9 (`idx == 9 && byte > 0x01`) it rejects 10th-byte bits that would overflow `u64`, and at the terminating byte of any multi-byte sequence (`idx > 0 && byte == 0`) it rejects overlong zero-padded continuations with `OverlongNonCanonicalVarint`. Alongside it, `validate_arrow_ipc_buffer_64` checks `(offset & 63) == 0` and overflow-safe `offset.checked_add(len)` against `total_body_bytes`, `verify_disjoint_tag_branches` checks deterministic LL(1) choice tags via `FxHashSet<u8>`, and `vesper grammar` validates real files up to 8 MiB:

    // crates/vesper-grammar/src/lib.rs
    pub fn decode_canonical_u64_leb128(buf: &[u8]) -> Result[(u64, usize), GrammarDecodeError> {
        let mut result: u64 = 0;
        for (idx, &byte) in buf.iter().take(10).enumerate() {
            if idx == 9 && byte > 0x01 { return Err(GrammarDecodeError::VarintOverflow64); }
            result |= u64::from(byte & 0x7F) << (idx * 7);
            if (byte & 0x80) == 0 {
                if idx > 0 && byte == 0 { return Err(GrammarDecodeError::OverlongNonCanonicalVarint); }
                return Ok((result, idx + 1));
            }
        }
        if buf.len() < 10 { Err(GrammarDecodeError::UnexpectedEof) }
        else { Err(GrammarDecodeError::VarintOverflow64) }
    }
    // Validate protobuf, DER, or 64B-aligned Arrow IPC files from the CLI:
    $ vesper grammar check arrow-ipc batch.arrow --simd-alignment --json

# 32 Beacon distributed tracing

A trace follows a request across services. Beacon treats each running span as a resource with a parent, a set of exportable fields, and an owner who must record how the span ended.

<!-- technical-detail -->

Distributed tracing follows causal work across fibers and network hops, but conventional drop-guard spans blur normal returns with early `?` unwinds and allocate heap attribute maps on hot paths. Vesper Beacon models every open span as a linear resource paired with register-pinned trace context, columnar OTLP-Arrow ring tiles, and compile-time credential redaction.

Beacon spans `@vesper-obs/beacon`, `crates/vesper-beacon`, the `vesper traceparent` and `vesper top` CLI utilities, and the Lean 4 redaction model `lean/BeaconRedaction.lean`. Per `REVIEW-STATUS.md` (`TYPE-06`, `TYPE-07`, and `REPO-02`), `lean/BeaconRedaction.lean` proves non-interference and PII exclusion for its standalone attribute, batch, and dictionary-stream model, while full integration of linear effect handler state into a single language-wide operational soundness theorem remains open.

## 32.1 Decide what crosses the service boundary

A trace identifier groups work across services. A span identifies one unit of work within it. Context propagation connects the next service to the current span. Define which attributes may be exported before adding tracing to a path that handles personal or secret data.

<!-- technical-detail -->

A trace identifier correlates work across services, while a span identifier names one local operation and its parent edge. Rather than storing context in thread-local `RefCell` slots that break across fiber migrations or wrapping futures in `Instrumented<F>` adapters, Beacon packs the 24-byte `SpanContext` (`trace_id_hi: u64`, `trace_id_lo: u64`, `span_id: u64`, and `flags: u8`) directly inside Vesper Pulse's pinned `EvidenceVector` (`r15` on `x86_64`, `x28` on AArch64) so child fibers spawned in `Pulse::scope` inherit parent trace identity in one register copy.

Every exported attribute schema is fixed before instrumentation touches paths carrying credentials or personal data. Types lacking a `Telemetrable` implementation cannot cross into span columns or logs unless wrapped in an explicit redaction or fingerprinting model.

## 32.2 Finish a span on each exit

An `ActiveSpan` represents an open observation. Consume it by recording completion or an error outcome. A lexical `Trace` capability carries context with the computation across fiber switches. A W3C `traceparent` header needs validation before it supplies a remote parent identity.

<!-- technical-detail -->

Beacon declares `Trace` as a `linear effect` (`q = linear`) and `ActiveSpan` as `Mode::Linear`, so live spans survive across `Storage`, `Net`, and `Clock` fiber suspensions while `[T-Let]` rejects any early `?` or scope exit that fails to call `Trace::emit_ok` or `Trace::emit_err`. At service ingress, a padding-free 55-byte `W3cTraceParentWire` view paired with branchless SIMD hex-nibble lookup (`eff [Isa[P]]`) parses W3C `traceparent` v00 headers in roughly 3 ns without `unsafe`:

~~~vesper
wire struct W3cTraceParentWire {
    version_hex: ByteArray[2], sep0: u8, trace_id_hex: ByteArray[32],
    sep1: u8, parent_span_hex: ByteArray[16], sep2: u8, flags_hex: ByteArray[2],
}
copy struct SpanContext { trace_id_hi: u64, trace_id_lo: u64, span_id: u64, flags: u8 }
linear struct ActiveSpan { ctx: SpanContext, parent_span_id: u64, start_mono_ns: u64, schema_id: u32 }
linear effect Trace {
    fn current_ctx() -> SpanContext;
    fn start_span(schema_id: u32, name: StaticStr) -> ActiveSpan;
    fn emit_ok(sink span: ActiveSpan, attr_u64: u64) -> Unit;
    fn emit_err(sink span: ActiveSpan, err_code: u32, msg: StaticStr) -> Unit;
}
fn parse_w3c_traceparent[P: IsaProfile](
    in raw_hdr: ByteSlice,
) -> Result[SpanContext, TraceParseError] eff [Isa[P], Fail[TraceParseError]] {
    let in wire = raw_hdr.view_wire[W3cTraceParentWire]().map_err(|_| TraceParseError::InvalidLength)?;
    if wire.sep0 != 0x2D || wire.sep1 != 0x2D || wire.sep2 != 0x2D { Fail::raise(TraceParseError::MissingHyphen); }
    let (trace_hi, trace_lo, parent_id, flags) = Isa::decode_w3c_hex_52[P](in wire)?;
    if (trace_hi | trace_lo) == 0 || parent_id == 0 { Fail::raise(TraceParseError::AllZeroIdentifier); }
    Ok(SpanContext { trace_id_hi: trace_hi, trace_id_lo: trace_lo, span_id: parent_id, flags })
}
~~~

## 32.3 Batch telemetry by column

Per-shard ring buffers collect attributes in tiles. An exporter can take columns of IDs, timestamps, and values and send them using an Arrow-based telemetry representation. Keep the backing tile live until export completes. Define what happens when a producer fills the ring faster than an exporter drains it.

<!-- technical-detail -->

Instead of heap-allocating `Vec<KeyValue>` per event, Forge's WASI `@span_schema` macro assigns a compile-time `schema_id` and appends fixed-width columns into a shard-local 16-row `AoSoA` ring mapped in Hardware Domain 1 (`DmaSlab`):

    W3C traceparent v00 (55B wire struct: "00-" | 32-hex trace | "-" | 16-hex span | "-" | 2-hex)
      │  ~3 ns SIMD hex nibble decode
      ▼
    EvidenceVector (r15 / x28): 24B SpanContext ──▶ ActiveSpan (Mode::Linear: emit_ok | emit_err)
      │  inout shard.telemetry (0 Alloc, 0 atomics)
      ▼
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ SpanTileRow AoSoA Ring (tiled(16) in Domain-1 DmaSlab)                     │
    │ hot: [trace_hi|trace_lo|span_id|parent_id|start_ns|dur_ns|schema|attr] x16 │
    │ cold: [InlineStr[64] error_summary x 16]                                   │
    └─────────────────────────────────────┬──────────────────────────────────────┘
                                          ▼  io_uring IORING_OP_SEND_ZC
                            Zero-Copy OTLP-Arrow (OTAP) HTTP/2 Batch

~~~vesper
tiled(16) copy struct SpanTileRow {
    hot trace_id_hi: u64, hot trace_id_lo: u64, hot span_id: u64, hot parent_span_id: u64,
    hot start_unix_ns: u64, hot duration_ns: u64, hot schema_and_status: u64, hot primary_attr_u64: u64,
    cold error_summary: InlineStr[64],
}
~~~

Because the eight hot `u64` columns already match Apache Arrow IPC column buffers, a background Pulse fiber exports full 16-row blocks as OpenTelemetry-Arrow (`OTAP`) HTTP/2 frames via `io_uring` `IORING_OP_SEND_ZC` with `Alloc` absent from `eff [...]` while holding the backing Domain-1 tile lease until the zero-copy send completion arrives. When foreground producers outrun the exporter, the shard ring increments an explicit dropped-tile counter and recycles the oldest unexported non-pinned slot rather than blocking request fibers or falling back to global heap allocation.

## 32.4 Prove what a redacted observation can reveal

Define which fields count as public and compare exports from runs that differ only in protected fields. Redaction has to cover derived fields and error paths too. Omitting a secret column is insufficient if a public label encodes the same information.

<!-- technical-detail -->

Beacon separates debug inspection from `Telemetrable`: `affine struct Secret[T]` intentionally omits both `Debug` and `Telemetrable`, so emitting a credential requires opting into a named Vesper Pact model that replaces or fingerprints the secret value:

~~~vesper
affine struct Secret[T] { inner: T }
pub model RedactedTokenTelemetry: Telemetrable for Secret[BearerToken] {
    fn encode_attr(in self, inout out: AttrWriter) eff [] {
        out.write_static_str("[REDACTED:sha256_prefix]");
    }
}
~~~

In `lean/BeaconRedaction.lean`, Vesper models public, fingerprinted, redacted, and PII-labeled attributes alongside stateful OTAP string-dictionary interning, proving attribute, span, batch, and multi-batch stream non-interference as well as total PII string exclusion:

    -- lean/BeaconRedaction.lean (modelId: "vesper.beacon.redaction-model/1")
    theorem export_span_noninterference (fp : String → Nat) (s t : Span)
        (h : spanLow fp s t) : exportSpan fp s = exportSpan fp t
    theorem stream_noninterference (fp : String → Nat) (cap : Nat) (d : List String)
        {xs ys : List (List Span)} (h : Pairwise2 (batchLow fp) xs ys) :
        encodeStream fp cap d xs = encodeStream fp cap d ys
    theorem pii_never_emitted (fp : String → Nat) (cap : Nat) (batches : List (List Span))
        {s : String} (h : ∀ b ∈ batches, ∀ sp ∈ b, ∀ a ∈ sp.attrs, a.label = .pii → a.val ≠ s) ...

Crucially, `stream_noninterference` accounts for stateful string-dictionary compression across batches (including dictionary capacity resets at `cap`): because redacted and PII attributes emit only fixed markers or numeric fingerprints before `intern` runs, secret payloads never perturb dictionary indices observed by later public spans.

## 32.5 Follow one context through the host

The host parses or creates trace IDs, records events into buffers, and hands batches to the exporter. Use a known parent and child to check the encoded relationship. A parse or export failure should leave neither a dangling span nor a borrowed tile reused too early.

<!-- technical-detail -->

`crates/vesper-beacon` implements `parse_traceparent_55` (enforcing exact 55-byte length, `"00-"` prefix, lowercase ASCII hex, and nonzero 128-bit trace and 64-bit span IDs, exposed on the CLI as `vesper traceparent HEADER [--json]`) together with `FastLogHistogram`, a wait-free core-local latency histogram updated via disjoint `inout shard.telemetry` borrows with zero atomics. `FastLogHistogram` packs 256 `u64` count buckets (a 2 KiB L1-resident array) plus 256 `u64` exemplar span IDs, using `leading_zeros` to index power-of-two octaves with sub-buckets (described as 4-bit / 8-way octave sub-bucketing in `crates/vesper-beacon`, keeping relative quantile error below 2% from nanoseconds to hours):

    // crates/vesper-beacon/src/lib.rs
    pub struct FastLogHistogram {
        pub buckets: [u64; 256], pub exemplar_span_id: [u64; 256], pub total_count: u64,
    }
    impl FastLogHistogram {
        pub fn bucket_index(val_ns: u64) -> usize {
            if val_ns < 16 { return val_ns as usize; }
            let octave = 63usize.saturating_sub(val_ns.leading_zeros() as usize);
            let sub = ((val_ns >> octave.saturating_sub(3)) & 0b0111) as usize;
            (((octave - 3) << 3) + sub).min(255)
        }
        pub fn record_with_exemplar(&mut self, duration_ns: u64, span_id: u64) {
            let idx = Self::bucket_index(duration_ns);
            self.buckets[idx] += 1; self.exemplar_span_id[idx] = span_id; self.total_count += 1;
        }
    }
    $ vesper traceparent 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01 --json

# 33 Warp packet ownership and network pacing

A network server can avoid copying a packet by transferring ownership of a frame between the driver and userspace. That saving makes it essential to know precisely when each frame can be reused.

<!-- technical-detail -->

Kernel-bypass networking avoids per-packet copies by passing fixed DMA frames directly between NIC descriptor rings and userspace protocol logic. That zero-copy path is safe only when every receive, turnaround transmit, retransmission hold, and completion transition accounts for the exact owner of each backing frame.

Vesper Warp (`@vesper-net/warp` and `crates/vesper-warp`) provides the user-space QUIC (RFC 9000), TCP (RFC 9293), and BBRv3 transport layer beneath Prism and Chorus, paired with the Lean 4 models `lean/WarpUmem.lean`, `lean/WarpBbr.lean`, and `lean/WarpPacketNumber.lean`. Per `REVIEW-STATUS.md` (`SYS-01`..`SYS-06` and `REPO-02`), Theorem 2 enforces source-level linear handle consumption rather than physical NIC DMA ring ownership or IOMMU cancellation, the Lean models verify abstract count and arithmetic transitions rather than the Rust driver code, and 100/400 GbE line-rate throughput remains a design goal.

## 33.1 Follow a frame between rings

An AF_XDP socket can exchange frames from a userspace memory area (UMEM) through queues. Receive, application processing, transmit, and completion each imply different owners. A retransmission may still need payload bytes after an individual send completes.

<!-- technical-detail -->

An `AF_XDP` socket exchanges 4 KiB Hardware Domain-1 `DmaSlab` chunks across four lock-free rings: the Fill ring hands empty buffers to the NIC, the Rx ring delivers received packets to userspace, the Tx ring submits outgoing packets, and the Completion ring returns transmitted descriptors once DMA finishes. Under `[T-Let]`, every harvested `UmemFrame[RxHarvested]` must reach either `recycle_to_fill_ring` (on malformed headers or connection misses) or `turn_around_tx_in_place` followed by retransmission parking so Fill-ring descriptors never leak without reference counting.

         ┌─────────────────────── recycle_to_fill_ring ──────────────────────┐
         ▼                                                                   │
    ┌───────────┐   NIC DMA    ┌───────────┐  harvest_rx   ┌─────────────────┴────┐
    │ Fill Ring │─────────────▶│  Rx Ring  │──────────────▶│ UmemFrame[RxHarvest] │
    └───────────┘              └───────────┘               └──────────┬───────────┘
         ▲                                                            │ turn_around_tx
         │ ACK / free                                                 ▼
    ┌────┴──────┐   NIC CQE    ┌───────────┐  SO_TXTIME    ┌──────────────────────┐
    │ Comp Ring │◀─────────────│  Tx Ring  │◀────EDT───────│ Timing Wheel + BBRv3 │
    └───────────┘              └───────────┘  launch_ns    │ (InFlightQuicPkt)    │
                                                           └──────────────────────┘

Per `SYS-01` in `REVIEW-STATUS.md`, Theorem 2 checks source-level linear obligations only: it does not by itself prove physical NIC DMA descriptor ownership, prevent hardware writes if a ring index wraps or an asynchronous queue is torn down before completion, or establish kernel/NIC memory-barrier correspondence. Runtime ring auditing (`UmemAuditor`) and explicit teardown quiescence remain mandatory at the host boundary.

## 33.2 Separate the headers from the payload lease

Wire views describe offsets and byte order in packet headers. A linear ring capability represents the allowed next transition for the backing frame. A valid header does not release the underlying memory. Keep a lease on the backing bytes until the transport can retire them after acknowledgement or retransmission.

<!-- technical-detail -->

Warp overlays padding-free `wire struct` views (`XdpDescWire` and 13-byte `QuicShortHdrWire`) onto `UmemFrame` bytes while keeping frame lifecycle transitions in `linear effect XdpNic` and sealing QUIC 1-RTT payloads in place through Vesper Vault `NonceTicket` capabilities (`eff [ConstTime]` or SmartNIC `ktls` crypto offload) so packet-number AEAD nonces cannot repeat:

~~~vesper
wire struct XdpDescWire { umem_addr: u64le, pkt_byte_len: u32le, options: u32le }
wire struct QuicShortHdrWire { flags_spin_keyphase: u8, dest_conn_id: ByteArray[8], pkt_num_trunc: u32be }
copy struct RxHarvested {}
copy struct TxStaged {}
linear struct UmemFrame[State] { chunk_idx: u32, data_offset: u16, pkt_len: u16 }
linear struct InFlightQuicPkt { pkt_num_full: u64, edt_departure_ns: u64, held_frame: UmemFrame[TxStaged] }
linear effect XdpNic {
    fn harvest_rx_single() -> Option[UmemFrame[RxHarvested]];
    fn view_frame_bytes(in frame: UmemFrame[RxHarvested]) -> ByteSlice;
    fn recycle_to_fill_ring(sink frame: UmemFrame[RxHarvested]) -> Unit;
    fn turn_around_tx_in_place(sink rx_frame: UmemFrame[RxHarvested], tx_len: u16) -> UmemFrame[TxStaged];
    fn submit_paced_tx(sink tx_frame: UmemFrame[TxStaged], pkt_num_full: u64, edt_launch_ns: u64) -> InFlightQuicPkt;
    fn park_for_ack_or_pto(sink inflight: InFlightQuicPkt) -> Unit;
}
~~~

## 33.3 Schedule packet transmission

QUIC ingress reconstructs and validates packet information before using the payload. Egress must respect both available congestion window and pacing time. BBRv3 models delivery conditions to guide sending. A timing wheel groups future send times so ready packets can be found without scanning every connection.

The transport owns a different unit in each protocol. TCP exposes an ordered byte stream. QUIC carries separately ordered streams over packets and protects packets with keys and nonces. Splitting a header from a payload can avoid copying bytes, but a frame retained for retransmission still needs a live owner. Sharding connection state keeps a connection’s updates on its assigned worker.

<!-- technical-detail -->

Receive-Side Scaling (RSS Toeplitz steering) routes each flow to one Pulse worker shard storing connection state in 16-wide `tiled(16) copy struct QuicConnTile` (`AoSoA`) columns. While TCP owns a contiguous byte-stream retransmission window, QUIC multiplexes independent streams over encrypted datagrams whose `InFlightQuicPkt` leases sit in a core-local Hierarchical Hashed Timing Wheel (`O(1)` insert and cancel, zero heap allocation) until peer ACK retirement or Probe Timeout (PTO) loss recovery, paced in silicon via `SO_TXTIME` Earliest Departure Time (EDT) launch timestamps:

~~~vesper
fn poll_and_step_quic_rx_one[Alg: AeadCipher, P: IsaProfile](
    inout conns: TiledSeq[QuicConnTile], in rx_keys: SecretSlab, inout tx_sealer: AeadSealer[Alg], now_ns: u64,
) -> Result[bool, QuicError] eff [XdpNic, ConstTime, Isa[P], Fail[QuicError]] {
    let Some(rx_frame) = XdpNic::harvest_rx_single() else { return Ok(false); };
    let raw_pkt = XdpNic::view_frame_bytes(in rx_frame);
    let Ok((in quic_hdr, in _cipher_body)) = raw_pkt.split_wire_prefix[QuicShortHdrWire]() else {
        XdpNic::recycle_to_fill_ring(sink rx_frame);
        return Ok(true);
    };
    let Some(conn_slot) = Warp::lookup_cid_simd[P](in conns, in quic_hdr.dest_conn_id) else {
        XdpNic::recycle_to_fill_ring(sink rx_frame);
        return Ok(true);
    };
    let full_pn = Warp::decode_packet_number_rfc9000(
        conns.get_largest_rx_pn(conn_slot), quic_hdr.pkt_num_trunc.to_native() as u64, 32,
    );
    let edt_ns = Warp::advance_bbrv3_edt(inout conns, conn_slot, 64, now_ns);
    let tx_frame: UmemFrame[TxStaged] = XdpNic::turn_around_tx_in_place(sink rx_frame, 64);
    let inflight: InFlightQuicPkt = XdpNic::submit_paced_tx(sink tx_frame, full_pn + 1, edt_ns);
    XdpNic::park_for_ack_or_pto(sink inflight);
    Ok(true)
}
~~~

## 33.4 Check conservation and pacing as separate properties

Frame conservation asks whether every frame is in exactly one allowed state. A pacing theorem asks whether modeled sends obey the computed bounds. Neither property guarantees a particular throughput on a live network. Delayed completions, loss, and reordering are useful simulation cases.

<!-- technical-detail -->

Frame conservation and congestion pacing establish orthogonal properties: neither guarantees live-network throughput, which `SimPulse` (`vesper test --sim`) exercises separately under Gilbert-Elliott burst loss, reordering, and NAT rebinding. In `lean/WarpUmem.lean`, `Census` tracks all 11 UMEM frame locations (`free`, `fill`, `nicRx`, `rx`, `appRx`, `appTx`, `tx`, `nicTx`, `comp`, `inFlight`, and `leaked`) and proves total-frame conservation across every transition and malformed-packet path; in `lean/WarpBbr.lean`, Vesper proves monotonic EDT launch scheduling and `targetCwnd` clamping within `[4 * mss, max(hi, 4 * mss)]`:

    -- lean/WarpUmem.lean & lean/WarpBbr.lean
    theorem apply_preserves_total {c c' : Census} {t : Trans} (h : apply c t = some c') : c'.total = c.total
    theorem umem_step_conserves_total_frames {c c' : Census} {o : RxOutcome} (h : quicStep c o = some c') :
        c'.fill + c'.rx + c'.tx = c.fill + c.rx + c.tx ∧ c'.total = c.total
    theorem quicStep_failure_replenishes_fill {c c' : Census} {o : RxOutcome} (hfail : o ≠ .reply)
        (h : quicStep c o = some c') : c'.fill = c.fill + 1
    theorem bbrv3_edt_launch_monotonic (prev now delay : Nat) :
        let (launch, next) := edtStep prev now delay; prev ≤ launch ∧ now ≤ launch ∧ launch ≤ next
    theorem targetCwnd_bounds (cwnd mss hi : Nat) :
        4 * mss ≤ targetCwnd cwnd mss hi ∧ targetCwnd cwnd mss hi ≤ max hi (4 * mss)

## 33.5 Reconstruct a packet number near a window edge

QUIC transmits a truncated packet number and the receiver reconstructs the intended full number relative to its expected value. Test around the half-window boundary, where the nearest candidate changes. The pacer and UMEM verifier track distinct state alongside this decoder.

<!-- technical-detail -->

QUIC headers carry an 8-, 16-, 24-, or 32-bit truncated packet number that the receiver reconstructs closest to `largest_pn + 1` within the 62-bit RFC 9000 space (`< 2^62`). `lean/WarpPacketNumber.lean` proves congruence, `< 2^62` range bounds (`decode_bound`), unique half-window recovery (`decode_window`), and the RFC 9000 Appendix A.3 test vector (`0xa82f30ea` + `0x9b32` -> `0xa82f9b32`), matching `crates/vesper-warp` alongside `Bbrv3FlowState` and `XdpUmemAccounting`:

    // crates/vesper-warp/src/lib.rs (differentially checked against lean/WarpPacketNumber.lean)
    pub fn decode_quic_packet_number_rfc9000(largest_pn: u64, truncated_pn: u64, pn_nbits: u32) -> u64 {
        let expected_pn = largest_pn.wrapping_add(1);
        let pn_win = 1u64 << pn_nbits;
        let pn_hwin = pn_win >> 1;
        let candidate_pn = (expected_pn & !(pn_win - 1)) | (truncated_pn & (pn_win - 1));
        if candidate_pn + pn_hwin <= expected_pn && candidate_pn < (1u64 << 62) - pn_win {
            candidate_pn + pn_win
        } else if candidate_pn > expected_pn + pn_hwin && candidate_pn >= pn_win {
            candidate_pn - pn_win
        } else { candidate_pn }
    }

# 34 Prism web service contracts

An HTTP handler can start streaming a response or hand its connection to another protocol. Prism tracks the response state so the caller knows which action is still valid if the client disconnects.

<!-- technical-detail -->

An HTTP/2 or HTTP/3 handler must either send a complete response, emit an error problem payload, or upgrade the stream into a bidirectional session such as WebSockets—even when the client disconnects or a deadline expires midway through execution. Vesper Prism encodes those transitions as linear typestates paired with per-request region arenas and lexical effect middleware.

Prism comprises `@vesper-web/prism`, `crates/vesper-prism`, and the Lean 4 authorization model `lean/PrismAuthSafety.lean`. Per `REVIEW-STATUS.md` (`SYS-03`, `TYPE-06`, `TYPE-07`, and `REPO-02`), Intel MPK and Arm POE native-TLS isolation controls remain proposed runtime mechanisms rather than verified sandbox implementations, and combining lexical handlers, arena lifetimes, and response typestates does not yet establish end-to-end HTTP/2/3 protocol completeness.

## 34.1 Model a request from route to completion

A router chooses a handler. Middleware supplies scoped behavior such as logging. A request arena owns short-lived decoded data. A response obligation remains live until the application finishes, aborts, or transfers it through an allowed upgrade.

<!-- technical-detail -->

Every Prism request runs inside a 64 KiB `Region::scoped(64.kib(), ...)` `VirtualArena` with `Alloc` omitted from `eff [...]`, eliminating global allocator fragmentation while `[T-RegionScope]` blocks request-arena slices (`RegionStr[req]`, `RegionSlice[req, u8]`) from escaping into long-lived shard tables without lifetime annotations. WASI `@route` and `@derive(JsonSliceDecode, OpenApiSchema)` macros expand inside Forge's Wasmtime sandbox (`macro_emits_effects = []`), HTTP/2 and RFC 6455 frame headers cast in `O(1)` via `wire struct` views over Domain-1 `io_uring` buffers, and foreign C TLS libraries are designed to run behind 1.5 ns Intel MPK / Arm POE protection-key gates.

Every matched route receives `sink res: Responder[HeadersPending]` and must return `ResponseSent` either directly (`res.json`, `res.problem`) or after driving an upgraded `linear struct WsSession` through its closing handshake.

## 34.2 Finish the right kind of response

A responder with pending headers can commit headers and enter a body-writing state. After the response finishes, sending more bytes would violate the protocol. Disconnect introduces another exit path. Account for live response and application resources before unwinding that path.

<!-- technical-detail -->

`linear struct Responder[S]` distinguishes `HeadersPending` from `StreamingBody` so neither unanswered error branches nor double-committed status headers type-check under Theorem 2. Disconnect and timeout safety follow directly from `[T-Let]` and Vesper's effect resumption lattice: Pulse's `Cancel` effect is abortable (`q = affine`, `res(Cancel) = affine`), whereas `TenantDb`, `Storage`, and `Net` are declared `linear effect` (`q = linear`). Before a handler opens a transaction, `Cancel::check()?` can unwind cleanly; once `let mut db_tx: DbTxn = TenantDb::begin_write(grant)` is live, `[T-Let]` rejects `Cancel::check()?` and unguarded `?` operators because `linear <= affine` is false, forcing the fiber to complete `db_tx.commit()` or `db_tx.rollback()` before honoring disconnect teardown:

    Incoming Request (64 KiB Region::scoped VirtualArena, Alloc absent)
      │  Cancel::check()? legal here (no linear DbTxn live; affine <= affine)
      ▼
    ┌───────────────────────────┐  commit_headers   ┌───────────────────────────┐
    │ Responder[HeadersPending] │──────────────────▶│ Responder[StreamingBody]  │
    └─────┬───────────────┬─────┘                   └─────────────┬─────────────┘
          │ json/problem  │ upgrade_websocket                     │ finish_body
          ▼               ▼                                       ▼
    ┌──────────────┐  ┌─────────────────────────┐           ┌──────────────┐
    │ ResponseSent │  │ linear struct WsSession │──close───▶│ ResponseSent │
    └──────────────┘  └─────────────────────────┘   (1000)  └──────────────┘
    // Once linear DbTxn is live: Cancel::check()? REJECTED (linear <= affine is false)

## 34.3 Scope middleware around an operation

A lexical handler can provide a request-specific policy to downstream code. Giving the handler a narrow projection of shard state limits the fields it may change. This lets a request update its counters while another operation reads unrelated configuration.

<!-- technical-detail -->

Instead of deeply nested `Tower::Service` wrapper types, heap-boxed futures, and `Arc<RwLock<_>>` state clones, Prism expresses middleware such as `Auth` (`q = affine`) and `TenantDb` (`q = linear`) as lexical `handle` blocks backed by Cranelift's pinned `vmctx` register (`r15` / `x28`):

~~~vesper
effect Auth {
    fn require_org_writer(org_id: u64) -> OrgWriteGrant;
    fn verify_read_scope(org_id: u64, in req: HttpRequestView) -> Result[OrgReadCap, AuthError];
}
linear effect TenantDb {
    fn begin_write(grant: OrgWriteGrant) -> DbTxn;
    fn append_event(inout tx: DbTxn, grant: OrgWriteGrant, in topic: ByteSlice, in payload: ByteSlice) -> Unit;
}
~~~

Stage 4 (`vesper-opt-egglog`) specializes synchronous JWT or session checks inline, while `[T-CallProj]` lets handlers mutate `inout shard.{rate_buckets, metrics}` alongside immutable `in shard.jwt_keys` borrows on the core-local `WorkerShard` without locks.

## 34.4 Transfer ownership on a WebSocket upgrade

A successful upgrade hands the connection from HTTP response handling to a WebSocket session. Treat it as a transition with one owner at the end. The ordinary response path must finish without an upgrade, and the upgraded path must eventually close the session.

<!-- technical-detail -->

In `ingest_event`, `Cancel::check()?` runs before `TenantDb::begin_write`, and both CRC branches consume `db_tx` and `res`. In `live_event_tail`, `res.upgrade_websocket(in req_raw)` consumes `Responder[HeadersPending]` and hands exclusive ownership to `linear struct WsSession`, which only `ws.close_gracefully(1000)` discharges into `ResponseSent`:

~~~vesper
@route("POST /v1/orgs/{org_id:u64}/events")
fn ingest_event[P: IsaProfile](
    org_id: u64, in req_raw: HttpRequestView, inout shard.{rate_buckets, metrics}: WorkerShard,
    in config: PrismConfig, sink res: Responder[HeadersPending],
) -> ResponseSent eff [req: Region, Isa[P], Auth, TenantDb, Net, Cancel, Fail[ApiError]] {
    Cancel::check()?;
    if !shard.rate_buckets.allow(org_id, config.burst_limit) { return res.problem(429, "rate limit exceeded"); }
    let grant: OrgWriteGrant = Auth::require_org_writer(org_id)?;
    let body = Json::parse_simd[P, IngestEventReq[req]](in req, in req_raw.body())?;
    let mut db_tx: DbTxn = TenantDb::begin_write(grant);
    match verify_payload_crc[P](in body.payload, body.crc32) {
        Ok(()) => {
            TenantDb::append_event(inout db_tx, grant, in body.topic.as_bytes(), in body.payload);
            let seq = db_tx.commit();
            shard.metrics.events_ingested += 1;
            res.json(201, in CreatedReceipt { seq })
        }
        Err(_bad_crc) => { db_tx.rollback(); res.problem(422, "payload CRC mismatch") }
    }
}
@route("GET /v1/orgs/{org_id:u64}/tail")
fn live_event_tail(
    org_id: u64, in req_raw: HttpRequestView, sink res: Responder[HeadersPending],
) -> ResponseSent eff [Auth, EventBus, Net, Clock, Cancel] {
    match Auth::verify_read_scope(org_id, in req_raw) {
        Err(_) => res.problem(403, "forbidden"),
        Ok(read_cap) => {
            let mut ws: WsSession = res.upgrade_websocket(in req_raw);
            loop {
                match Pulse::race_next_event_or_ping(read_cap, 15.seconds()) {
                    TailStep::Event(frame) => ws.send_binary(in frame),
                    TailStep::ClientClose => break ws.close_gracefully(1000),
                }
            }
        }
    }
}
~~~

## 34.5 Exercise failure at a protocol boundary

A simulated client can disconnect before headers, during a body, or after an upgrade. Check which obligations remain after each event. Policy proofs reason about modeled routes and states. Running the scenarios also tests whether the implementation emits the transitions the proof assumes.

<!-- technical-detail -->

In `lean/PrismAuthSafety.lean`, Vesper models `ingestEvent` and proves two Paralegal-style policy theorems: no `TenantDbOp.appendEvent` ever fires without an `OrgWriteGrant` matching `org`, and every appended payload satisfies the configured size ceiling:

    -- lean/PrismAuthSafety.lean
    theorem ingest_event_never_writes_unauthorized_org
        (authOracle : UInt64 → Option OrgWriteGrant)
        (h_sound : ∀ org g, authOracle org = some g → g.orgId = org)
        (org : UInt64) (req : HttpRequestView) (shard : WorkerShardProj) (cfg : PrismConfig) :
        ∀ op ∈ TraceOf (runWithAuth authOracle (ingestEvent org req shard cfg)),
          ∀ g topic bytes, op = TenantDbOp.appendEvent g topic bytes → g.orgId = org
    theorem ingest_event_payload_bounded ...

To test the runtime assumptions beneath those policy models, `SimPulse` (`vesper test --sim`) replays deterministic 64-bit PRNG chaos seeds injecting 1-byte slowloris TCP drips, mid-upgrade client RSTs, and storage latency spikes, while `vesper top` inspects suspended fibers holding live `Responder[HeadersPending]`, `WsSession`, or `DbTxn` handles.

## 34.6 Match a route in a radix tree

A radix tree shares common prefixes between route paths. Matching consumes a literal prefix or binds a parameter until it reaches a handler. Define precedence and validation for ambiguous paths, and keep extracted route values within the request lifetime. `CompiledRouteEntry::verify_zero_heap_budget` audits the handler effect row and rejects a global `Alloc` effect when the route promises a zero-heap budget.

<!-- technical-detail -->

In `crates/vesper-prism`, the radix router (`src/router.rs`) shares literal path prefixes, captures `:name` segment slices within the request lifetime (`RouteMatch::param`), implies `HEAD` fallback from `GET` (`Resolution::Matched` vs `Resolution::MethodNotAllowed`), and rejects conflicting parameter names at a shared prefix position (`RouteError::AmbiguousParameter`) or duplicate routes (`RouteError::DuplicateRoute`) under `RouterLimits`. At registration time, `CompiledRouteEntry::verify_zero_heap_budget` audits the handler's inferred `EffectRow` so routes that do not opt into `requires_global_alloc` cannot accidentally pull `Alloc` into the request path:

    // crates/vesper-prism (CompiledRouteEntry zero-heap audit + radix RouteError)
    pub struct CompiledRouteEntry {
        pub method: HttpMethod, pub pattern: SmolStr, pub param_types: Vec<(SmolStr, SmolStr)>,
        pub handler_symbol: SmolStr, pub required_effects: EffectRow, pub requires_global_alloc: bool,
    }
    impl CompiledRouteEntry {
        pub fn verify_zero_heap_budget(&self) -> Result<(), SmolStr> {
            let uses_alloc = self.required_effects.instances.iter()
                .any(|inst| inst.cap_label.as_deref() == Some("Alloc"));
            if uses_alloc && !self.requires_global_alloc {
                return Err(format!("route `{}` unexpectedly requires global Alloc", self.pattern).into());
            }
            Ok(())
        }
    }

# 35 Chorus protocols and distributed ownership

A choreography describes a conversation among services in one place. Chorus derives each participant’s send, receive, and close steps from that description so the endpoints agree on who acts next.

<!-- technical-detail -->

Choreographic consensus writes a multi-node protocol once and projects each participant's send, receive, choice, and one-sided RDMA steps so local endpoints stay in lockstep. Keeping those distributed steps memory-safe requires tracking both session-channel state and hardware RDMA registration lifetimes until remote access genuinely ends.

Vesper Chorus (`@vesper-dist/chorus` and `crates/vesper-chorus`) pairs `@choreography` endpoint projection and RDMA lease types with `lean/ChorusQuorum.lean`, `lean/ChorusQuorum/SessionDuality.lean`, and `lean/ChorusQuorum/RecursiveDuality.lean`. Per `REVIEW-STATUS.md` (`ECO-02` and `REPO-02`), endpoint duality and quorum-intersection lemmas neither revoke RNIC remote keys on timeout nor establish distributed liveness across network partitions, and the Lean files verify standalone protocol and ballot models rather than the native RDMA stack.

## 35.1 Read a protocol from one role’s viewpoint

A session type specifies an ordered conversation. A role can send, receive, select a branch, or close at the appropriate step. Following the protocol constrains local ordering. Delivery, progress during partitions, and agreement among replicas need explicit network assumptions.

<!-- technical-detail -->

Inside Vesper Forge (`macro_emits_effects = []`), `@choreography` compiles a global protocol description through Endpoint Projection (EPP) into role-local `Mode::Linear` `Session[Proto]` channels advancing through `Send[Msg, Next]`, `Recv[Msg, Next]`, `Select[L, R]`, `Offer[L, R]`, and `End`. Under Theorem 2 and `[T-Let]`, an endpoint cannot silently drop an open channel mid-handshake or reply out of order; transport failures (`ChorusError` / `NetFail`) must explicitly transition the session onto a recovery handler. Because `ChorusNet` and `RdmaQp` are lexical effects, `SimPulse` (`vesper test --sim`) executes 5- and 7-node clusters deterministically in one OS thread under asymmetric partitions, packet reordering, and clock skew.

## 35.2 Keep remote memory registered until access ends

Remote direct memory access (RDMA) lets a network interface operate on registered memory. A lease tracks registration and outstanding work. A peer timeout alone leaves open the possibility of device access. Revocation and completion evidence are needed before quarantined storage returns to the allocator.

<!-- technical-detail -->

Per `ECO-02` in `REVIEW-STATUS.md` and the manuscript's §35.2 review limitation, a local peer timeout does NOT revoke an RNIC remote key (`rkey`): a slow or partitioned peer's NIC can still issue one-sided PCIe DMA writes into registered host pages after a software timer fires. A safe RDMA lifecycle must keep Domain-1 `RdmaRegion` (`MrSlice`) memory quarantined until a per-transfer Type-2 memory window invalidation work request (`IBV_WR_LOCAL_INV` or an equivalent provider-supported revocation) completes on the local RNIC Completion Queue (`ibv_wc`), and session typing cannot guarantee peer progress during a network partition because liveness always depends on transport and failure-detector assumptions:

                     @choreography(roles = [Leader, Follower])
                        ┌────────────────┴────────────────┐
                   EPP  ▼                                 ▼  EPP
        Leader: Session[Send/Select]     Follower: Session[Recv/Offer] (Dual)
        ┌──────────────────────────┐     ┌──────────────────────────────────┐
        │ Domain-1 DmaSlice /      │     │ Domain-1 RdmaRegion (MrSlice)    │
        │ post_write_with_imm ─────┼────▶│ one-sided RNIC PCIe placement    │
        │ InFlightRdmaWrite        │     │ persist WAL + comm AppendAck     │
        └────────────┬─────────────┘     └────────────────┬─────────────────┘
                     ▼ await_wc(CQE)                      ▼ on peer timeout (ECO-02):
           Safe host slice reuse             Quarantine -> Type-2 IBV_WR_LOCAL_INV CQE

Chorus pairs 40-byte `RaftAppendWireHeader` views with linear `Session` channels and `linear struct InFlightRdmaWrite` tokens so host buffers remain live and unaliased across `RdmaQp::await_wc`:

~~~vesper
linear struct Session[Proto] { chan_id: u64 }
linear struct InFlightRdmaWrite { qp_num: u32, wr_id: u64, local_mr_slot: u32 }
linear effect RdmaQp {
    fn post_write_with_imm(
        peer_node: u32, remote_addr: u64, rkey: u32, imm_term_lo: u32, in local_dma_slice: DmaSlice,
    ) -> InFlightRdmaWrite;
    fn await_wc(sink op: InFlightRdmaWrite) -> Result[Unit, RdmaError];
}
~~~

## 35.3 Project a quorum write to its participants

A coordinator sends a proposal, replicas acknowledge it, and a quorum determines whether the operation can advance. Project the global description to each endpoint. If an endpoint must choose between two behaviors, it needs enough information locally to distinguish the branches.

<!-- technical-detail -->

A single `@choreography(roles = [Leader, Follower])` specification expresses one-sided RDMA slot placement followed by follower WAL persistence and acknowledgment, while the leader's `commit_quorum_batch` driver fans out local Quarry `WriteTxn` durability and peer RDMA replication concurrently inside `Pulse::scope` with `Alloc` omitted from `eff [...]`:

~~~vesper
@choreography(roles = [Leader, Follower])
fn raft_replicate_slot(
    Leader.hdr: RaftAppendWireHeader, Leader.batch_dma: DmaSlice, Follower.ring_va: RemoteSlotGrant,
) -> Leader.AppendAck {
    rdma_write Leader.(hdr, batch_dma) -> Follower.ring_va;
    let Follower.ack: AppendAck = Follower.persist_and_ack(in ring_va);
    comm Follower.ack -> Leader
}
fn commit_quorum_batch(
    inout node.{term, commit_index, peers}: RaftLeaderState, in batch_dma: DmaSlice, sink local_tx: WriteTxn,
) -> Result[CommitReceipt, ConsensusError] eff [Scope, Storage, RdmaQp, ChorusNet, Fail[ConsensusError]] {
    let hdr = node.build_append_header();
    let (local_res, peer0_res, peer1_res) = Pulse::scope(|inout s| eff [Scope, Storage, RdmaQp, ChorusNet] {
        let f_local = s.spawn(|| eff [Storage] {
            handle Fail[IoError] in { Ok(local_tx.commit()) } with { return r => r, raise(e) => Err(e) }
        });
        let f_p0 = s.spawn(|| eff [RdmaQp, ChorusNet] { replicate_to_peer_rdma(in node.peers[0], in hdr, in batch_dma) });
        let f_p1 = s.spawn(|| eff [RdmaQp, ChorusNet] { replicate_to_peer_rdma(in node.peers[1], in hdr, in batch_dma) });
        (f_local.join(), f_p0.join(), f_p1.join())
    });
    let receipt = local_res.map_err(ConsensusError::LocalWal)?;
    let acks = 1 + u32::from(peer0_res.is_ok()) + u32::from(peer1_res.is_ok());
    if acks < 2 { Fail::raise(ConsensusError::QuorumNotReached { acks }); }
    node.commit_index += 1;
    Ok(receipt)
}
~~~

## 35.4 Separate quorum intersection from election safety

Two majorities of a fixed replica set overlap. In a set of five, any two groups of three share at least one replica. Turning that fact into election or log safety also requires rules for how a replica votes, records its decision, and recovers after a crash.

<!-- technical-detail -->

Quorum intersection is purely combinatorial, whereas election safety and log matching add state-machine rules for single-vote-per-term ballots and prefix consistency checks. `lean/ChorusQuorum.lean` proves `election_safety`, `follower_append_preserves_matching`, and `half_floor_quorums_can_be_disjoint` (an explicit `n = 5` counterexample where two floor-half sets of size `5 / 2 = 2`, `[true, true, false, false, false]` and `[false, false, true, true, false]`, share zero voters, explaining why `majority n = n / 2 + 1 = 3` is required), while `lean/ChorusQuorum/SessionDuality.lean` and `lean/ChorusQuorum/RecursiveDuality.lean` prove binary projection duality across branching and recursive heartbeat loops:

    -- lean/ChorusQuorum.lean, SessionDuality.lean & RecursiveDuality.lean
    theorem half_floor_quorums_can_be_disjoint :
        ∃ xs ys : List Bool, xs.length = 5 ∧ ys.length = 5 ∧
          count xs ≥ 5 / 2 ∧ count ys ≥ 5 / 2 ∧ count (both xs ys) = 0
    theorem election_safety (bs : Ballots) (c₁ c₂ : Nat)
        (h₁ : 2 * count (votesFor c₁ bs) > bs.length) (h₂ : 2 * count (votesFor c₂ bs) > bs.length) : c₁ = c₂
    theorem follower_append_preserves_matching (sys : List Log) (f leader : Log) ...
    theorem sessionDual_of_proj (a b : Nat) (hab : a ≠ b) (g : G) ...
    theorem raft_replicate_slot_session_dual :
        SessionDual (projectRole leader raftReplicateSlotChor) (projectRole follower raftReplicateSlotChor) = true
    theorem raft_heartbeat_loop_session_dual : ...

## 35.5 Check that local branches can merge

A role uninvolved in a global branch may see identical local behavior on each side. The projection can merge those paths. If one path requires receiving a different next message without an observable choice, the protocol needs an explicit signal or a revised ordering.

<!-- technical-detail -->

When a `GlobalChor::Branch` is decided by `decider` and signaled to `notified`, any uninvolved third role must either execute identical local steps on both sides or receive compatible labeled `Offer` branches so it knows how to proceed (`unmergeable_view_is_undefined` in `SessionDuality.lean`). `crates/vesper-chorus` enforces knowledge-of-choice via `project_endpoint`, `merge_local_projections`, and `are_dual_sessions`:

    // crates/vesper-chorus/src/lib.rs
    pub fn merge_local_projections(
        role: RoleId, lhs: LocalSessionTy, rhs: LocalSessionTy,
    ) -> Result<LocalSessionTy, ProjectionError> {
        if lhs == rhs { return Ok(lhs); }
        match (lhs, rhs) {
            (LocalSessionTy::Offer { peer: p1, left_label: ll1, left: l1, right_label: rl1, right: r1 },
             LocalSessionTy::Offer { peer: p2, left_label: ll2, left: l2, right_label: rl2, right: r2 })
                if p1 == p2 && ll1 == ll2 && rl1 == rl2 => Ok(LocalSessionTy::Offer {
                    peer: p1, left_label: ll1, left: Box::new(merge_local_projections(role, *l1, *l2)?),
                    right_label: rl1, right: Box::new(merge_local_projections(role, *r1, *r2)?),
                }),
            _ => Err(ProjectionError::UnmergeableBranchForUninvolvedRole { role }),
        }
    }
    pub fn project_endpoint(chor: &GlobalChor, target: RoleId) -> Result<LocalSessionTy, ProjectionError>

# 36 Quarry SQL and buffer pools

Quarry checks SQL against a known schema before execution. When a query runs, a page pin keeps the database from evicting storage that the query still needs. The query engine can then scan its fields in batches.

<!-- technical-detail -->

A relational engine needs compile-time schema agreement for queries, disciplined buffer-pool pinning so resident pages are never evicted mid-scan, and crash-consistent write-ahead logging on disk. Vesper Quarry combines offline WASI SQL checking, linear `PagePin` buffer handles, `io_uring` `O_DIRECT` PAX pages, and vectorized `AoSoA` execution.

Quarry spans `@vesper-db/quarry`, `crates/vesper-quarry`, and the Lean 4 recovery and pin-conservation development `lean/QuarryWal.lean`, backing Chapter 1's `WriteTxn` and Chapter 34's `TenantDb`. Per `REVIEW-STATUS.md` (`ECO-03` and `REPO-02`), linear page pins do not prevent latch-order deadlocks or concurrent B-tree split races, migration alias edge cases remain open, and `lean/QuarryWal.lean` proves a page-LSN physical-redo no-steal model rather than full physiological ARIES recovery.

## 36.1 Check a query against a known catalog

Replay schema migrations into a local catalog and check names and types against that snapshot. A buffer pool separately tracks which pages are resident and pinned. Columnar execution works on batches of fields. For example, a valid column name passes the schema check even if the page holding that column has since been evicted.

<!-- technical-detail -->

Inside Forge's fuel-metered `wasm32-wasip2` sandbox (`macro_emits_effects = []`), the proposed `@sql` macro replays declared `migrations/*.sql` DDL into an offline `HermeticCatalog` without ambient filesystem or network sockets, checking column names, bind parameters, outer-join nullability, and output `AoSoA` schemas (targeting sub-100-microsecond expansion on bounded catalogs). At runtime, Quarry maps 16 KiB slotted B+Tree and PAX columnar pages over Domain-1 2 MiB hugepages via `io_uring` `O_DIRECT` (`IORING_OP_READ_FIXED` and `IORING_OP_WRITE_FIXED`) with CRC32C verification, pairing optimistic `PageEpoch` internal traversal with zero-atomic `PagePin` leaf pins.

## 36.2 Hold a pin while a page view is live

A slotted page stores a header, slot information, and row data. A pin prevents a buffer frame from being evicted while the caller uses it. Releasing a pin exactly once protects frame ownership. Concurrent page mutation also needs latches and a consistent acquisition order.

<!-- technical-detail -->

Per `ECO-03` in `REVIEW-STATUS.md` and the manuscript's §36.2 review limitation, `Mode::Linear` `PagePin` handles prevent frame-pin leaks (`BufferPool` exhaustion), NOT latch-order deadlocks, concurrent B-tree page-split races, or deadlocks triggered by suspending a Pulse fiber while holding an exclusive or shared page latch. A sound concurrent B-tree still requires a strict global latch acquisition order (such as root-to-leaf latch crabbing with high-key fence checks on splits) and a static or runtime prohibition against fiber suspension while a page latch is live:

    16 KiB Quarry O_DIRECT Page (Domain-1 2 MiB Hugepage Frame)
    ┌──────────────────────────────┬─────────────────────┬──────────────┬───────────────┐
    │ QuarryPageWireHeader (32B)   │ Slot Dir / PAX Offs │ Free Space   │ Tuple/PAX Col │
    │ lsn|id|crc32c|slots|free|tag │ [u16le; slot_count] │ ───────────▶ │ ◀──────────── │
    └──────────────────────────────┴─────────────────────┴──────────────┴───────────────┘
    // Linear BufferPool PagePin[Access] lifecycle (ECO-03: pin safety != latch order):
    fix_shared(id)    ──▶ PagePin[SharedPin]    ──(Ok | Err CRC)──▶ unpin_clean(sink pin)
    fix_exclusive(id) ──▶ PagePin[ExclusivePin] ──▶ unpin_dirty(sink pin, lsn) | unpin_aborted

~~~vesper
wire struct QuarryPageWireHeader {
    page_lsn: u64le, page_id: u64le, checksum_crc32c: u32le,
    slot_count: u16le, free_space_offset: u16le,
    page_kind_tag: u8, level: u8, flags: u16le, right_sibling_page: u32le,
}
copy struct SharedPin {}
copy struct ExclusivePin {}
linear struct PagePin[Access] { frame_slot: u32, page_id: u64 }
linear effect BufferPool {
    fn fix_shared(page_id: u64) -> Result[PagePin[SharedPin], DbError];
    fn fix_exclusive(page_id: u64) -> Result[PagePin[ExclusivePin], DbError];
    fn view_page(in pin: PagePin[SharedPin]) -> ByteSlice;
    fn mutate_page(inout pin: PagePin[ExclusivePin]) -> ByteSliceMut;
    fn unpin_clean(sink pin: PagePin[SharedPin]) -> Unit;
    fn unpin_dirty(sink pin: PagePin[ExclusivePin], rec_lsn: u64) -> Unit;
    fn unpin_aborted(sink pin: PagePin[ExclusivePin]) -> Unit;
}
~~~

## 36.3 Execute a checked query in batches

A proposed `@sql` expansion reads the catalog during the build and produces typed query access. At execution, a filter can examine a tile of values and pass selected rows to the next operator. Push a filter earlier only when doing so preserves the query’s semantics.

<!-- technical-detail -->

Stage 4 (`egglog`) lowers `@sql` queries into vectorized morsels writing directly into 16-wide `tiled(16) copy struct TenantOrderSummary` tiles inside a bounded `Region::scoped` arena (`Alloc` absent from `eff [...]`). Because `[T-Let]` forbids `?` while `pin: PagePin[SharedPin]` is live, `scan_high_value_orders` explicitly unpins the buffer frame on both the `Ok` happy path and the `Err` CRC-corruption path:

~~~vesper
tiled(16) copy struct TenantOrderSummary {
    hot order_id: u64, hot org_id: u64, hot total_cents: i64, hot created_epoch_s: u64,
    cold customer_ref: InlineStr[32],
}
fn scan_high_value_orders[P: IsaProfile, r: Region](
    in arena: RegionCap[r], target_org: u64, min_cents: i64,
    leaf_page_ids: Slice[u64], inout out_tiles: TiledSeq[TenantOrderSummary],
) -> Result[i64, DbError] eff [r: Region, BufferPool, Isa[P], Fail[DbError]] {
    let v_org = VVec::splat[P, u64](target_org);
    let v_min = VVec::splat[P, i64](min_cents);
    let mut grand_total: i64 = 0;
    for i in 0..leaf_page_ids.len() {
        let pin: PagePin[SharedPin] = BufferPool::fix_shared(leaf_page_ids[i])?;
        let page_bytes = BufferPool::view_page(in pin);
        match decode_pax_order_columns[P](in page_bytes, v_org, v_min, inout out_tiles) {
            Ok(page_sum) => {
                grand_total = grand_total.wrapping_add(page_sum);
                BufferPool::unpin_clean(sink pin);
            }
            Err(corrupt_err) => {
                BufferPool::unpin_clean(sink pin);
                Fail::raise(corrupt_err);
            }
        }
    }
    Ok(grand_total)
}
~~~

## 36.4 Account for pages, tenants, and recovery

Pin conservation checks that acquisitions and releases balance. Tenant isolation checks that a query can reach only authorized pages. A write-ahead log and recovery protocol address the order in which changes become durable. A balanced pin count can coexist with a read from the wrong tenant. Check the page’s tenant as well as its pin.

<!-- technical-detail -->

Pin conservation, row-level tenant confinement (`row.orgId = targetOrg`), and crash recovery address distinct failure modes: a balanced pin count never prevents reading another tenant's page if the tenant predicate is omitted. Although the manuscript prose mentions physiological ARIES, the actual repository development in `lean/QuarryWal.lean` (`vesper.quarry.wal-model/1`, matching `crates/vesper-quarry/src/store`) proves eight theorems over a page-LSN physical-redo no-steal deferred-update WAL, torn commit groups, strict-lock loser undo, and CRC-failing scan pin conservation:

    -- lean/QuarryWal.lean (8 audited theorems, modelId: "vesper.quarry.wal-model/1")
    theorem redo_recovers_history (log : List Rec) (ho : Ordered log)
        (hpos : ∀ r ∈ log, 0 < r.lsn) (d : Disk) (hw : WalConsistent log d) : redo d log = history blank log
    theorem redo_idempotent (log : List Rec) (ho : Ordered log)
        (hpos : ∀ r ∈ log, 0 < r.lsn) (d : Disk) (hw : WalConsistent log d) : redo (redo d log) log = redo d log
    theorem complete_groups (tail : List Rec) :
        ∀ gs : List (List Rec), complete (gs.flatMap frame ++ tail.map Entry.upd) [] = gs.flatten
    theorem recovery_ignores_torn_tail (gs : List (List Rec)) (tail : List Rec) ...
    theorem undo_restores_winners (loser : Nat → Bool) (s0 : Table) (log : List Write) ...
    theorem no_steal_needs_no_undo (loser : Nat → Bool) (s : Table) (log : List Write)
        (h : ∀ w ∈ log, loser w.txn = false) : undo loser s log = s
    theorem scan_restores_pool (min : Nat) (pages : List Nat) (P : Pool) (acc : List Nat) : (scan min P pages acc).1 = P
    theorem scan_conserves_page_pins (min : Nat) (P : Pool) (pages frames acc : List Nat) :
        (scan min P pages acc).1.pinnedFrameCount frames = P.pinnedFrameCount frames

Because Quarry's store is no-steal (uncommitted writes stay buffered until commit), `no_steal_needs_no_undo` explains why crash recovery only replays committed groups (`recovery_ignores_torn_tail`) without writing compensation log records.

## 36.5 Trace a plan through the optimizer

The catalog supplies types and schema facts. A relational optimizer compares candidate plans using valid equivalences and estimated costs. A useful test compares the results on empty inputs, duplicates, and joins with multiple matches, where a mistaken rewrite can quietly change multiplicity.

<!-- technical-detail -->

In `crates/vesper-quarry`, `HermeticCatalog::infer_join_projection` forces output columns nullable on the non-preserved side of `LeftOuter`, `RightOuter`, and `FullOuter` joins, while `RelPlan::push_down_predicates` pushes single-table selection filters past `HashJoin` directly into `PaxTableScan::pushed_predicates` only when `referenced_tables` is a subset of one join input, preserving join multiplicity across duplicates and empty inputs:

    // crates/vesper-quarry/src/lib.rs
    impl HermeticCatalog {
        pub fn infer_join_projection(
            &self, lhs_table: &str, rhs_table: &str, join_kind: JoinKind, select_cols: &[(&str, &str)],
        ) -> Result<Vec<ColumnDef>, SqlTypeError>
    }
    impl RelPlan {
        pub fn push_down_predicates(self) -> Self {
            // Pushes single-table Filter nodes into PaxTableScan SIMD column predicates
            // and routes subset filters into the matching lhs or rhs child of HashJoin.
        }
    }

# 37 Flux incremental streams

When one record changes, a materialized view can update its answer without rerunning the entire query. Flux represents both insertion and removal as weighted changes and tracks how far processing has progressed.

<!-- technical-detail -->

Materialized views update continuously in work proportional to the arriving batch delta rather than re-scanning historical tables on every tick. Vesper Flux pairs DBSP signed-multiplicity Z-Sets over `AoSoA` tiles with linear watermark frontiers and Chandy-Lamport checkpoint barriers.

Flux spans `@vesper-stream/flux`, `crates/vesper-flux`, `lean/FluxDbsp.lean`, and the Z-Set compaction proof `uncompacted_zset_eq_compacted` in `lean/VesperUnifiedTypeTheory.lean` (Appendix A), building on DBSP (Budiu, McSherry, Ryzhyk, and Tannen, VLDB 2023). Per `REVIEW-STATUS.md` (`ECO-04` and `REPO-02`), unbounded incremental joins cannot run in fixed zero-allocation arenas without a proven window, TTL, or cardinality bound, and the Lean models establish algebraic Z-Set, frontier, and shard-migration laws rather than native distributed runtime proofs.

## 37.1 Interpret a signed multiplicity

A Z-set assigns an integer weight to a record. Adding a row contributes +1 and retracting it contributes −1. Consolidation sums equal records and removes zero weights. If the same key arrives twice and is retracted once, one occurrence remains.

<!-- technical-detail -->

A Z-Set maps each tuple to a signed 64-bit integer multiplicity (`z_weight: i64`): `+1` inserts a record, `-1` retracts or deletes a prior record, and consolidation sums duplicate keys and removes net-zero entries (`+1 + -1 == 0`) inside per-epoch `VirtualArena` `ZSetTile[K, V]` (`tiled(16) struct`) columns using `eff [Isa[P]]` SIMD lanes before touching persistent LSM spines.

Instead of reference-counted `Capability<Timestamp>` handles whose accidental retention stalls window emission indefinitely, Flux models event-time progress as `Mode::Linear` `FrontierTicket` obligations and Chandy-Lamport snapshot markers as `Mode::Linear` `BarrierToken` handles committed atomically with upstream Krabka offsets inside a Quarry `WriteTxn`.

## 37.2 Track changes and frontiers

A change-data-capture (CDC) record describes an update. AoSoA tiles batch the changes, and a frontier ticket represents progress in logical time. A watermark states how far a source has advanced under its lateness contract. A checkpoint barrier coordinates a consistent snapshot across operators.

<!-- technical-detail -->

Zero-copy Kafka/Krabka Change-Data-Capture (CDC) log segments enter Flux through a padding-free 32-byte `CdcDeltaWireHeader` and materialize into 16-wide `OrderRiskDeltaTile` columns while `linear effect FluxRuntime` governs monotonic watermark advancement and checkpoint commits:

~~~vesper
wire struct CdcDeltaWireHeader {
    source_lsn: u64le, event_ts_ms: i64le, z_weight: i64le,
    partition_id: u32le, schema_id: u16le, flags: u16le,
}
tiled(16) copy struct OrderRiskDeltaTile {
    hot account_id: u64, hot amount_cents: i64, hot z_weight: i64, hot event_ts_ms: i64,
    cold merchant_code: u32,
}
linear struct FrontierTicket { operator_id: u32, watermark_ts_ms: i64 }
linear struct BarrierToken { epoch_id: u64, shard_id: u16 }
linear effect FluxRuntime {
    fn advance_watermark(sink ticket: FrontierTicket, next_watermark_ts_ms: i64) -> Result[FrontierTicket, FluxError];
    fn retire_frontier(sink ticket: FrontierTicket) -> Unit;
    fn commit_checkpoint_barrier(sink barrier: BarrierToken, inout spine: ZSetLsmSpine, source_lsn: u64) -> Result[CommitReceipt, FluxError];
    fn abort_checkpoint_barrier(sink barrier: BarrierToken) -> Unit;
}
~~~

## 37.3 Update a join and close a window

To update a join, pair new rows from each input with matching old rows on the other. If both inputs change, also count matches between the two changes. A tumbling window needs a frontier and a late-data policy before it can discard old state.

For a single matching key, multiply the multiplicities to count joined pairs. Suppose the left count grows from two to three and the right count grows from three to five.

    Old joined pairs             2 × 3 = 6
    Added left × old right       1 × 3 = 3
    Old left × added right       2 × 2 = 4
    Added left × added right     1 × 2 = 2
    New joined pairs             6 + 3 + 4 + 2 = 15

A fresh batch join gives the same 15 pairs. Omitting the interaction between the two changes would lose two pairs.

<!-- technical-detail -->

To maintain a bilinear equi-join incrementally (`Delta(A Join B) = (DeltaA Join z^-1(B)) + (z^-1(A) Join DeltaB) + (DeltaA Join DeltaB)`), Flux multiplies matching key multiplicities across new and delayed inputs. Suppose a key's left count grows from 2 to 3 (`da = +1`) and its right count grows from 3 to 5 (`db = +2`):

    Old joined pairs             2 × 3 = 6
    Added left × old right       1 × 3 = 3
    Old left × added right       2 × 2 = 4
    Added left × added right     1 × 2 = 2
    New joined pairs             6 + 3 + 4 + 2 = 15

A fresh batch join produces the same `3 × 5 = 15` pairs, whereas omitting the `da × db` interaction term would drop 2 pairs. Per `ECO-04` in `REVIEW-STATUS.md` and the §37.3 review limitation, general incremental joins retain unbounded historical state (`z^-1(I(A))` and `z^-1(I(B))`): zero-allocation fixed-arena execution holds only when a tumbling/sliding window watermark, TTL eviction rule, or input key-cardinality bound proves a finite state ceiling; otherwise operator state must grow or spill to Quarry storage with explicit `Alloc`, `Storage`, and `Fail` effects.

In `step_incremental_exposure_view`, `sink maybe_barrier: Option[BarrierToken]` crosses `Option` to `Mode::Linear` via `[K-Adt]` (joining effective field floors per `TYPE-01`), forcing `[T-Let]` to commit or abort the checkpoint barrier and retire `frontier` on checkpoint failure:

~~~vesper
fn step_incremental_exposure_view[P: IsaProfile](
    in raw_cdc_batch: DmaSlice, in risk_spine: ZSetLsmSpine, inout exposure_spine: ZSetLsmSpine,
    sink frontier: FrontierTicket, sink maybe_barrier: Option[BarrierToken],
) -> Result[FrontierTicket, FluxError] eff [FluxRuntime, Storage, Isa[P], Fail[FluxError]] {
    let (batch_max_ts, max_lsn) = Region::scoped(256.kib(), |cap tick_arena| {
        let mut delta_in = TiledSeq::with_region[OrderRiskDeltaTile](in tick_arena, 2048);
        let mut delta_out = TiledSeq::with_region[OrderRiskDeltaTile](in tick_arena, 2048);
        let (observed_ts, last_lsn) = Flux::decode_and_consolidate_cdc[P](in raw_cdc_batch, inout delta_in);
        Flux::probe_zset_spine_simd[P](in delta_in, in risk_spine, inout delta_out);
        exposure_spine.merge_consolidated_tiles[P](in delta_out);
        (observed_ts, last_lsn)
    });
    match maybe_barrier {
        Some(barrier) => {
            match FluxRuntime::commit_checkpoint_barrier(sink barrier, inout exposure_spine, max_lsn) {
                Ok(_receipt) => {}
                Err(ckpt_err) => { FluxRuntime::retire_frontier(sink frontier); Fail::raise(ckpt_err); }
            }
        }
        None => {}
    }
    FluxRuntime::advance_watermark(sink frontier, batch_max_ts)
}
~~~

## 37.4 Compare incremental and batch answers

Apply all input changes up to a logical time, then compare the maintained view with a fresh batch computation. A second property requires the watermark to move monotonically. General joins may retain unbounded history. A memory bound requires a window, eviction rule, or input bound.

<!-- technical-detail -->

In `lean/FluxDbsp.lean`, Vesper proves exact bilinear join differentiation (`bilinear_join_exact`), additive retraction cancellation (`retraction_cancels`), frontier lower-bound preservation across live holds (`advance_preserves_live_bound`), checkpoint replay equivalence (`restore_replay_eq_uninterrupted`), and partitioned shard rescaling (`migration_preserves_global`), complementing Appendix A's `uncompacted_zset_eq_compacted` in `lean/VesperUnifiedTypeTheory.lean`:

    -- lean/FluxDbsp.lean & lean/VesperUnifiedTypeTheory.lean (Appendix A)
    theorem bilinear_join_exact (a b da db : Int) :
        a * b + (da * b + a * db + da * db) = (a + da) * (b + db)
    theorem retraction_cancels (w : Int) : w + (-w) = 0
    theorem advance_preserves_live_bound (watermark old next : Int) (rest : List Int)
        (h_resp : respects watermark (old :: rest)) (h_mono : old ≤ next) :
        respects watermark (next :: rest)
    theorem restore_replay_eq_uninterrupted {S I O C : Type} ...
    theorem uncompacted_zset_eq_compacted ...

## 37.5 Differentiate an operator circuit

A circuit describes how operators such as joins, sums, delays, and feedback connect. Differentiation constructs operators that propagate changes. Consolidation combines accumulated weights for equal records. Feedback needs a clear time boundary so a change returning around the circuit belongs to the correct step.

<!-- technical-detail -->

In `crates/vesper-flux`, `ZSetBatch::consolidate_in_place` sorts batch keys, sums adjacent `i64` multiplicities with `saturating_add`, and compacts away zero-weight (`w == 0`) tuples in place; `FrontierTracker` rejects out-of-order watermark steps with `FluxCheckError::WatermarkRegression`; and `ZSetCircuitExpr::differentiate_dbsp` implements Stage 4's DBSP differentiation operator `D(Q)` using `z^-1` delayed integrators (`DelayPrevIntegrate`) so feedback and join circuits step cleanly across logical ticks:

    ΔA (+w/-w) ──┬────────────────────────────▶( ⋈ )──┐
                 ├──▶[ I: + ]──▶[ z^-1 ]────┐    ▲    │
                 │                          │    │    │
                 ▼                          │    │    ▼
               ( ⋈ )────────────────────────┼────┼▶( + )──▶ consolidate ──▶ Δ(A ⋈ B)
                 ▲                          ▼    │    ▲     (drop w == 0)
                 │              ┌─────────▶( ⋈ )─┼────┘
    ΔB (+w/-w) ──┴──┬───────────┘                │
                    └──▶[ I: + ]──▶[ z^-1 ]──────┘
    // Δ(A ⋈ B) = (ΔA ⋈ z^-1(I(B))) + (z^-1(I(A)) ⋈ ΔB) + (ΔA ⋈ ΔB); gated by FrontierTicket

    // crates/vesper-flux/src/lib.rs
    impl ZSetCircuitExpr {
        pub fn differentiate_dbsp(&self) -> Self {
            match self {
                Self::StreamInput(name) | Self::Delta(name) => Self::Delta(name.clone()),
                Self::DelayPrevIntegrate(inner) => Self::DelayPrevIntegrate(Box::new(inner.differentiate_dbsp())),
                Self::LinearFilterMap { pred_name, input } => Self::LinearFilterMap {
                    pred_name: pred_name.clone(), input: Box::new(input.differentiate_dbsp()),
                },
                Self::UnionPlus(a, b) => Self::UnionPlus(
                    Box::new(a.differentiate_dbsp()), Box::new(b.differentiate_dbsp()),
                ),
                Self::EquiJoin(a, b) => {
                    let (da, db) = (a.differentiate_dbsp(), b.differentiate_dbsp());
                    let term1 = Self::EquiJoin(Box::new(da.clone()), Box::new(Self::DelayPrevIntegrate(b.clone())));
                    let term2 = Self::EquiJoin(Box::new(Self::DelayPrevIntegrate(a.clone())), Box::new(db.clone()));
                    let term3 = Self::EquiJoin(Box::new(da), Box::new(db));
                    Self::UnionPlus(Box::new(term1), Box::new(Self::UnionPlus(Box::new(term2), Box::new(term3))))
                }
            }
        }
    }
    // ZSetBatch::consolidate_in_place sorts keys, sums i64 weights, and truncates w == 0 entries.
