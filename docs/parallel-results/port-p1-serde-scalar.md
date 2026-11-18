# Port wave P1 lane 19: vesper-serde scalar codecs (#916)

Baseline: `69c79b9` (origin/main after #948), merged forward before merge.
The crate links into the compiler, so it follows the seed-copy rule of lane 2
(#899) the way Beacon does: `crates/vesper-serde/seed` is the frozen Rust
build (package `vesper-serde-seed`, library `vesper_serde`), and
`crates/vesper-serde` is the port-backed build, whose `build.rs` always links
the Loom package in `crates/vesper-serde/vesper` and sets `cfg(vesper_port)`.
The compiler and **every** dependent (root package, vesper-lsp, the
scalable-vectors workflow) depend on the seed by rename. Rust stays the
default everywhere and no Rust was deleted (the owner's parity rule).

## API inventory (re-measured before porting)

| Module | Public items | Hot path |
| --- | --- | --- |
| `index` (`IndexedJson`, `Node`, `Cursor`, `ObjectFields`, `JsonString`, `JsonStr`, `Limits`, `Kind`) | 30 fns, 8 types | scalar parse, field lookup, string decode/equals, `as_u64`/`as_i64`/`as_f64` |
| `stage2` (`SimdParser`, `parse_with_index`) | 8 fns, 1 type | stage 2 tape builder |
| `lemire`, `pow5`, `float` (`number::decode_f64`, `FloatLimits`) | 2 fns, 2 types | f64 decode |
| `encode` (`Value`, `EncodeLimits`, `encoded_size`, `encode_into`) | 2 fns, 2 types | transactional encode, shortest f64 text |
| `helpers` (`SwarFieldMatcher`, `JsonSizeBound`, `compute_escaped_mask_64`) | 9 fns, 2 types | schema dispatch |
| `ndjson` (`NdjsonBatch`, `BatchLimits`, cursors, errors) | 18 fns, 9 types | batch admission, framing, accounting |
| `event_columns` (`EventEnvelope`, `EventColumns`, `ProjectionLimits`, `project_into`) | 3 fns, 3 types | schema check |
| `tiles` (`TelemetryIngestor`, `TelemetryTiles`, `SwarKey`, ...) | 19 fns, 10 types | telemetry decode |
| `streamvbyte`, `archive`, `binary` | 21 fns, 10 types | binary codecs |
| `simd` (stage 1 kernels) | 18 fns, 5 types | outside the port boundary |

All 145 tests (142 integration tests, 1 unit test, 2 doc tests at baseline)
pass against both packages: `cargo test -p vesper-serde-seed -p vesper-serde`
runs 290 tests, 0 failures.

## What was ported

`crates/vesper-serde/vesper/` (Loom package `@vesper/serde`, export ABI v3,
6,293 lines over 10 files; `vesper fmt --check` clean):

| Vesper file | Rust it replaces |
| --- | --- |
| `lemire.ves`, `pow5.ves` | `lemire::decode_f64` (Clinger fast path, Eisel-Lemire, the 128-bit power-of-five table generated once). The undecided case returns path 3 and the adapter calls `str::parse`, exactly the Rust build's `DecodePath::Fallback`. Bit-identical on the differential corpus. |
| `index.ves` | `IndexedJson::parse` (UTF-8 validation, the recursive-descent parser with all limit, work and stage 1 entry accounting, duplicate-key checks), `Node::field`, `JsonString::equals` and `decode`. |
| `stage2.ves` | `stage2::build` over the Rust stage 1 index (`parse_with_index`, `SimdParser::parse`), with `admit`. |
| `dragon.ves`, `encode.ves` | `encode::encoded_size`/`encode_into`, including Rust's `{}` text for finite f64 (the exact `flt2dec` Dragon `format_shortest` over a 1,280-bit bignum, sized per exponent). |
| `helpers.ves` | `SwarFieldMatcher::compile_key`/`matches_window`/`matches_key`, `compute_escaped_mask_64`, `JsonSizeBound::string`/`combine_field`, `SwarKey::matches_raw`/`matches`. |
| `binary.ves` | `streamvbyte::{max_encoded_size, encoded_size, encode_into, decode_into}`; `archive::{validation_bytes, encoded_size, encode_into}`, `RelPtr32::{between, resolve}`, `RelSlice32::resolve`, `Archive::validate`. |
| `records.ves` | `Node::as_u64`/`as_i64`, the telemetry decode of `tiles`, `checked_event` (both projection passes) of `event_columns`, and `BatchLimits::validate` plus NDJSON batch admission, framing and accounting of `NdjsonBatch::parse_with`. |
| `lib.ves` | the shared boundary types and the 39 exported entry points. |

The token tape is the port's `Doc` handle, read in place through its
`SeqRef` (no copy); `SimdParser` reuses it across records, so
`tiles_alloc` still sees zero Rust allocations in the steady state.

## What stays Rust and why

- `src/simd/**`: the stage 1 kernels are outside the boundary (lane prompt).
  The scalar stage 1 also lives in `simd/mod.rs` and stays Rust with it;
  porting it needs the same file split the kernels would.
- `src/port.rs` (thin adapter, safe code only): converts the crate's borrowed
  types. Encode flattens a `Value` tree into a word stream and one byte pool
  (one allocation per call that the Rust build does not make); archive
  encode flattens `TreeNode`s the same way. These go once borrowed values
  can cross the export ABI (a slice of handles or of `&str`).
- The adapter takes `&str` of validated input with the safe
  `core::str::from_utf8`, so UTF-8 is checked twice in the port build; an
  ABI-level "validated UTF-8" view would remove it.
- Public types, iterators, cursors, `Display`, the `const fn`s (`SwarKey::compile`,
  `matches_u64_window`, `JsonSizeBound` constants) and `TelemetryTiles`
  storage stay Rust: they are views, compile-time constants or storage, not
  codecs.
- Vesper enum variants share one namespace, so `binary::ErrorKind` crosses
  as `BinErrorKind` with a `B` prefix and record-level errors as a `Fault`
  code; the adapter maps both back.
- `unsafe`: none in the port or adapter. The generated binding
  (`port/binding.rs`, 67 `unsafe` lines, each a documented FFI call) is the
  only unsafe code the port adds.

## Unit tests

The crate has one in-crate test, `index::tests::raw_steps_equal_decoded_steps`.
It checks the Rust `raw_compare_steps` and is kept: it runs in both packages,
and the Vesper `raw_compare_steps` it mirrors is covered by the duplicate-key
tests of `json.rs` and `limits_differential.rs` in the port build. The
integration tests are unchanged except that the three that read package
files (`json_test_suite.rs`, `lean_escape.rs`, `lean_models.rs`) find the
crate directory from `seed/` too.

## Divergence checks

`build.rs` has one edit per ported module (`DIVERGENT_EDITS`, feature
`vesper-port-divergent`, module chosen by `VESPER_SERDE_DIVERGE`); Bazel
builds the same seven objects (`serde_vesper_divergent_*`). `port/check.sh`
(one line in `scripts/test-port-harness.sh`) asserts each is caught:

| Module | Edit | Fails | Still passes |
| --- | --- | --- | --- |
| lemire | low mantissa bit flipped | `every_path_is_bit_identical_to_std_on_a_differential_corpus` | `power_of_five_table_matches_an_independent_big_integer` |
| index | `e+` exponents refused | `json_test_suite_corpus` | `random_documents_equal_serde_json` |
| stage2 | fractions refused | `random_valid_documents_build_identical_tapes` | `deep_and_wide_inputs` |
| encode | `+` for the minus sign | `every_finite_exponent_class_and_arbitrary_finite_bits_roundtrip_exactly` | `all_nonfinite_sign_and_payload_classes_refuse_before_any_nested_output_write` |
| helpers | backslash escapes a backslash | `branchless_escape_mask_matches_scalar_reference` | `prefix_xor_is_parity` |
| binary | every StreamVByte value one byte wider | `streamvbyte_fixed_reference_bytes_and_output_suffixes` | `archive_fixed_reference_bytes_signed_pointers_and_borrowed_views` |
| records | tiles store flags as the status | `ingests_into_hot_and_cold_tiles_on_every_path` | `swar_keys_match_exactly_the_equal_byte_strings` |

All seven were run locally and caught.

## Benchmarks

`examples/serde_abba.rs`, built twice: `serde_abba_rust` from the seed
package and `serde_abba` from the port package, both `--release`. The driver
runs 9 epochs of A B B A windows (20 repetitions each after a warm-up) per
workload and gates the paired totals with `vesper_bench::analyze_pairs`
against the 2x stop rule (B at most +10,000 bp over A). Setup: cloud
container, 4 vCPU Intel Xeon @ 2.30 GHz, shared and noisy, Rust 1.100
toolchain of the repository, the port compiled by the in-tree `vesper`
(release, `opt_level=speed`, inliner on).

| Workload (from `simd_throughput.rs`) | Port / Rust, median (range) | 2x gate |
| --- | --- | --- |
| scalar parse, 2,000-item document | 4.83x (3.20-5.29) | Regression |
| two-stage SIMD parse (Rust stage 1, port stage 2) | 5.70x (5.15-6.25) | Regression |
| f64 decode, 20,000 numbers | 5.23x (4.24-7.22) | Regression |
| f64 encode, 4,000 17-digit values | 64.9x (57.3-68.8) | Regression |
| telemetry tiles, 5,000 lines | 4.67x (4.41-5.62) | Regression |

Every workload is over 2x, so the Rust code stays (stop before deletion).
Where the time goes:

- f64 encode: Rust formats with Grisu and falls back to Dragon only when
  Grisu cannot decide; the port always runs Dragon over a bignum held in a
  `ChunkSeq`, with checked `try_get`/`try_set` per limb. Porting Grisu
  (its cached-power table) is the largest single win available.
- parse, stage 2, tiles: byte-at-a-time `try_get` loops and per-token
  `ChunkSeq` pushes. They need the Stage 1 perf work already merged
  (#931-#936: inlining, wide reads, SIMD scans) applied by rewriting the
  hot loops with the block builtins, which the port parity wave (#949-#956)
  covers.
- f64 decode: the 10^k loop for Clinger and per-digit loops; a table and the
  eight-digit SWAR path everywhere would close most of it.

## Platforms run

Linux x86_64 (this container): both packages' tests, the seven divergence
checks, the benchmark, `vesper fmt --check`. macOS and Windows run
`port/check.sh` through `scripts/test-port-harness.sh` in CI; Linux CI runs
the Bazel targets (`//crates/vesper-serde`, `//crates/vesper-serde/seed`,
the divergent objects).

## Rust code deleted

None. The port is 4.7-65x slower than the Rust build, so by the owner's rule
the Rust seed stays the default for every dependent and is not deleted. The
crate README records the freeze (bug fixes only, #916).
