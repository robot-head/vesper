# Wire/Json: two-stage SIMD on-demand JSON parsing (§20.3, §20.5)

Wave-six lane 7, claim issue #249. Branch `claude/project-thread-pvav5h`
(the prompt suggested `wave6/serde-simd-json`; this session may push only the
former). Base `07322e3` (origin/main, the merge of plan PR #268, a descendant of
the required `cda092f`). All source changes are inside `crates/vesper-serde/**`;
the root patch is one `Cargo.lock` hunk in its own commit.

## What was built

**Stage 1, structural index** (`src/simd/`). `StructuralIndex::build(input, isa,
limits)` reads 64-byte blocks (the tail padded with spaces, like
`load_masked(.., 0x20)`) and emits `u32` positions of operators, opening and
closing quotes and scalar starts outside strings, the positions of
escape-starting backslashes inside strings, the first control byte inside a
string, an unclosed-string flag, and a UTF-8 verdict.

- Each ISA path computes five raw masks per block (backslash, quote, operator,
  whitespace, control) and validates UTF-8. Operators and whitespace use
  nibble-indexed shuffle tables (`pshufb` / `vqtbl1q`), with control bytes
  masked out of the operator class (the table alone would classify 0x0c and
  0x1a as `,` and `:`). UTF-8 uses the Keiser–Lemire three-table lookup with
  the continuation-length check and a carried incomplete-sequence vector.
- Shared branchless code then derives escapes (the odd-backslash-run
  carry kernel), string interiors (prefix XOR via `PCLMULQDQ` / `PMULL`
  carry-less multiply by all-ones), and structural positions.
- Paths: `Isa::Avx2` (AVX2 + PCLMULQDQ, 2x32 bytes), `Isa::Sse42` (SSE4.2 +
  PCLMULQDQ, 4x16), `Isa::Neon` (NEON + AES/PMULL, 4x16) and `Isa::Scalar`, an
  independent byte-at-a-time reference (per-bit quote parity, the existing
  `compute_escaped_mask_64`, `core::str::from_utf8`). `Isa::detect()` picks the
  fastest at run time; requesting an unavailable path is
  `ErrorKind::IsaUnavailable`. The `eff [Isa[P]]` source effect remains a
  compiler step for later.
- `unsafe` is confined to `src/simd/kernels.rs` (`#![deny(unsafe_code)]` at the
  crate root, `allow` only there): calls into `#[target_feature]` functions
  after run-time detection of every enabled feature, and unaligned loads from
  `&[u8; 16]`/`&[u8; 32]` references.

**Stage 2, validation and tape** (`src/stage2.rs`). `IndexedJson::parse_simd`,
`parse_with_isa`, `parse_with_index` and the reusable `SimdParser` walk the
structural positions with a bounded explicit stack (no recursion, hard depth
128): strings are delimited by their quote pair, escapes are visited from the
stage 1 list only, scalars span to the next structural. They build the same
subtree-skipping token tape as the existing recursive-descent
`IndexedJson::parse`, so the existing `Node`/`Cursor`/`ObjectFields` API is the
on-demand cursor: numbers are converted and escaped strings unescaped only when
read, unescaped strings borrow the input, unknown subtrees skip in O(1).
Duplicate keys (by decoded scalar equality) are still rejected, with the same
work accounting; the comparison now has a no-decode fast path for unescaped
keys that charges exactly the same units (unit-tested against the decoded loop).
After stage 2, one safe `core::str::from_utf8` pass over the whole input
yields the `&str` the document keeps, so every token view stays an O(1)
slice (a review finding against per-access span validation).

**§20.5 ingestion.**
- `NdjsonBatch::parse` uses `ndjson::Engine::detect()` (SIMD when available,
  one stage 1 buffer shared by all records); `parse_with(.., Engine::Scalar)`
  keeps recursive descent. Batch statistics and `event_columns` projections are
  identical between engines.
- New `tiles` module: `TelemetryIngestor` decodes the §20.5 `TelemetryPoint`
  (`ts_unix_ms: i64`, `latency_us: u32`, `status_code: u16`, `flags: u16`,
  `trace_id_hex: InlineStr[32]`) from NDJSON into `TelemetryTiles`: 16-wide
  AoSoA tiles, `#[repr(C, align(64))]` 256-byte hot tiles and separate cold
  tiles. Only the five fields are decoded; ranges, kinds, missing fields,
  blank lines and capacity are typed errors, and a failing batch rolls the tiles
  back to their length on entry.

**Lean.** `crates/vesper-serde/lean/SimdJsonEscape.lean` models the branchless
escape kernel over `BitVec 64` and proves with `bv_decide`, for every mask and
carry-in 0 or 1, that its output satisfies the scalar reference's defining
recurrence (`e[0] = carry`, `e[i+1] = bs[i] && !e[i]`, carry-out
`bs[63] && !e[63]`), which determines that reference uniquely. This is a theorem
about the Lean model, not about the compiled Rust; the Rust kernel's agreement
with the reference is tested. `tests/lean_escape.rs` checks the file with
`VESPER_LEAN` (as CI sets it) and rejects `sorry`/`admit`/`axiom`/
`native_decide`. **Lean was not run in this session** (running the downloaded
toolchain was not permitted here), so CI is its first check.

## Acceptance evidence

| Test file | What it checks |
| --- | --- |
| `tests/simd_stage1.rs` | Every SIMD path's per-block masks and UTF-8 verdict are bit-identical to the scalar reference: all 256 byte values in 10 lane positions; 4,000 random JSON-flavoured inputs; backslash runs of 0–140 bytes at 13 offsets around block edges with five tails; every prefix and suffix of a quote-heavy document; all 65,536 two-byte sequences at three block offsets; 3- and 4-byte sequences over all lead bytes and boundary continuation bytes at six offsets; every 97th scalar value and its truncation at block edges. Also branchless versus scalar escape mask on every single run and 40,000 random masks, prefix XOR parity, index positions, limits. |
| `tests/simd_stage2.rs` | Scalar parser versus every path: identical tapes, capacity and `work_used` on 3,000 random valid documents; identical accept/reject **and identical error kind and offset** on 12,000 mutated documents and a list of classic malformed inputs; identical acceptance under 3,000 random tight limit sets (94% identical errors, see gaps); depth 1–300; 300-key objects; reuse of `SimdParser`. |
| `tests/json_test_suite.rs` | JSONTestSuite `test_parsing` (all 318 cases; 316 vendored under MIT, the two large generated files regenerated byte for byte): 93 `y_` accepted with values equal to `serde_json`, the 2 duplicate-key `y_` cases rejected by policy with `DuplicateKey`, all 188 `n_` rejected, `i_` 10 accepted (equal to serde_json where it accepts too) and 25 rejected; every engine agrees on every file. 3,000 random documents equal `serde_json`. |
| `tests/ndjson_engines.rs` | 300 random and mutated NDJSON batches: identical stats, record spans, errors and column projections between engines. |
| `tests/tiles.rs` | Tile layout, values, escaped keys, CRLF, final-newline handling, eleven typed failure cases, transactional rollback, capacity. |
| `tests/tiles_alloc.rs` | Counting global allocator (current thread only): after one warm-up batch, three further 256-record batches, a failing batch, and ten reused `SimdParser` parses make **0 allocations** on every path. |

Both SIMD path families are exercised: AVX2 and SSE4.2 on this x86_64 host, and
NEON by running the whole crate test suite for `aarch64-unknown-linux-gnu`
under `qemu-aarch64` (all pass). CI runs the same tests on Linux x86_64, macOS
AArch64 and Windows x86_64; `simd_paths_are_detected_on_simd_architectures`
fails if x86_64 lacks SSE4.2 or AArch64 lacks NEON there, so the SIMD path
cannot silently fall back (set `VESPER_ALLOW_SCALAR_ONLY` to opt out).

## Measured throughput

`cargo run --release -p vesper-serde --example simd_throughput`, best of 7
timed repetitions of ≥150 ms each, on a 4-vCPU cloud VM reporting "Intel(R)
Xeon(R) Processor @ 2.30GHz" (AVX-512 present but unused), rustc 1.97.0, no
other load. A 750 KB array of objects (escaped quotes, non-ASCII text, nested
objects) and a 3.4 MB, 20,000-line telemetry NDJSON file:

| Path | Stage 1 MB/s | Full parse (stage 1+2) MB/s | NDJSON → tiles MB/s |
| --- | ---: | ---: | ---: |
| Scalar reference | 319 | 210 | 162 |
| SSE4.2 | 2,361 | 575 | 302 |
| AVX2 | 3,003 | 583 | 304 |

For scale on the same data: recursive-descent `IndexedJson::parse` 272 MB/s,
`serde_json::from_slice::<Value>` 138 MB/s (it builds a DOM, so it is not a like
for like comparison). Stage 2 (token tape and duplicate-key checks) now
dominates. No AArch64 throughput was measured (QEMU timings are meaningless).

## Checks run

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: pass; also
  `cargo clippy -p vesper-serde --all-targets --target aarch64-unknown-linux-gnu -- -D warnings`.
- `cargo test -p vesper-serde` (x86_64) and the same for
  `aarch64-unknown-linux-gnu` under QEMU: all pass, including doctests.
- `cargo test --workspace --locked --no-fail-fast`: everything passes except
  16 tests in `vesper-loom-build` (`tests/build.rs`, `tests/cli.rs`) failing
  with `UnknownFingerprint { missing: ["engine_dependencies"] }`. That crate
  does not depend on `vesper-serde` (`cargo tree -i` finds no path), so this is
  environmental or pre-existing and unrelated to this lane.
- Mutation checks: weakening one UTF-8 table entry or one whitespace table
  entry makes the stage 1 tests fail.

## Root patch

`Cargo.lock`: `vesper-serde` gains its `serde_json` dev-dependency (a workspace
dependency already in the lock). No manifest, member, BUILD or CI change;
test data lives inside the crate, which `vesper_crate()` already includes.

## Decisions

- Validate the whole document before exposing any view (as the crate already
  required), rather than simdjson-style lazy validation: an O(1) skip over an
  unvalidated subtree could accept malformed input. "On demand" here means
  decoding on demand.
- Make stage 2 produce the existing tape byte for byte, so `Node`, `Cursor`,
  NDJSON and `event_columns` gain the SIMD front end with no API change and the
  scalar parser is a full differential oracle.
- Keep duplicate-key rejection (ambiguous input is an error), at the cost of the
  two `y_object_duplicated_key*` JSONTestSuite cases.
- Validate UTF-8 in the kernel (which rejects first and is differentially
  tested) but obtain the document's `&str` with one safe `core::str::from_utf8`
  pass instead of `from_utf8_unchecked`, so a kernel bug can at worst produce a
  typed `Utf8` error, never undefined behaviour. The cost is a second, fast
  validation pass per document.
- Compare floats with serde_json to 4 ulps: serde_json without
  `float_roundtrip` is not correctly rounded; ours is `str::parse`.

## Remaining gaps

- No `eff [Isa[P]]` typing, `@derive(JsonDecode)`, `tiled(16)` lowering or
  Region/`Watermark` arenas; `SimdParser` buffer reuse is the host analogue.
  Rust ownership here is not evidence for Vesper source typing rules.
- No AVX-512/VPCLMULQDQ, SVE or RISC-V paths; no Eisel–Lemire float decoding
  (std's correctly rounded parse is used); `tiles` matches keys by bytes rather
  than SWAR words.
- Under several simultaneous limit violations, the engines can report different
  limit kinds or offsets (they check in a different order); acceptance is always
  identical. Stage 1 overflow reports `TokenLimit` at offset 0.
- Stage 1 adds up to 4 bytes per input byte of index memory (bounded by
  `4 * max_tokens + 1` entries), not counted in `max_index_bytes`.
- The zero-allocation result holds only after warm-up and for documents no
  larger than earlier ones.
- The Lean proof covers the escape kernel only (not quote parity, the
  classification tables or UTF-8), and was not run locally.
