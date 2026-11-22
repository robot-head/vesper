# Rust exit SH6 lane 12: small libraries, Chorus and Pact

Claim [#1407](https://github.com/robot-head/vesper/issues/1407). Prompt:
[12-small-libs.md](../agent-prompts/self-host-sh6/12-small-libs.md). The lane
carries #915 (Crucible), #965 (TOML reader) and #1068 (Chorus). Started
2026-11-21 from `main` at `ab965f2a`. This report grows with each part.

## Status by crate

| Crate | State | Speed (port / Rust, ABBA) | What keeps Rust |
|---|---|---|---|
| vesper-pact | Rust gone from `src/` (part 1, #1427) | unchanged (port was already the only build) | the seed, until Stage 2 |
| vesper-loom-calculus | flipped, Rust in the seed (part 2, #1449) | 8 pkgs 1.12x [1.03, 1.38]; 64 pkgs 0.80x [0.74, 0.91]; 256 pkgs about 0.77x | the seed; `parse_manifest`'s TOML decoding is Vesper (parts 4 to 6, 1.29x); JSON text stays Rust |
| vesper-loom-audit | flipped, Rust in the seed (part 3, this PR) | 8 pkgs 1.37x [1.34, 1.41] (n=54); 64 pkgs 1.15x [1.05, 1.27]; 256 pkgs 0.96x [0.91, 1.04] | the seed; input-size checks and BLAKE3 context digests (serde_json + blake3), until `vesper-blake3-sys` lands |
| vesper-semver | above the bar | 64 exports 1.63-1.81x; 8 exports 1.93-2.29x; empty interfaces 3.2x | encoder cost and checker codegen (below) |
| vesper-beacon | above the bar | exp 3.5x, hist 4.5x, spans 2.1x, traceparent 1.0x | per-call FFI cost (calls of a few ns) |
| vesper-crucible | partly above the bar | draws 1.47x, tape 0.85x, shrink 1.57x, fuzz 1.34x, dpor 0.79x, cached 0.89x | per-draw boundary cost (below); not ported: fuzzer driver, schedule explorers, arena |
| vesper-chorus | front end Vesper-only (#1072) | parse+project 0.70x | Raft cluster model, sessions, simnet, durable, warpnet, rdma (part 2 of #1068) |

Ratios are Hodges-Lehmann medians of paired ABBA epochs with their 95%
intervals, or medians of 21 ABBA rounds where no interval is given. All
runs are on a shared 4-vCPU cloud container, so expect noise of about 0.1x.

## Pact (part 1, #1427)

`check_orphan`, `conformance_env` and `validate_query` run in `@vesper/pact`
(`orphan_check`, `conformance_env_of` and `query_check` in
`vesper/src/lib.ves`). The Rust versions moved into the seed engine.
`tests/standalone_differential.rs` compares them with the seed over every
impl and model of the fixture world plus 3,000 generated conformances and
3,000 generated queries. There were 0 differences, panics included.

## Loom calculus (part 2, #1449)

What closed the gap, in instructions on 8 packages (Rust 78.5M), from 126M
down to 87M:

1. The encoder interns with an Fx-style word hash instead of SipHash.
2. The decoder copies its input with `ChunkSeq::extend` instead of a push
   loop.
3. String comparisons read big-endian words built once per string.
4. The merge sort ping-pongs two buffers with `swap` instead of copying back.
5. The wrapper sends the occurrence indices in byte order (flagged by the top
   bit of `nocc`). The port checks the order in one pass and sorts only when
   the order is wrong or missing, so the solver's encoder, which sends none,
   still works.
6. The port writes version text from the version's parts (`put_ver`).

## Loom audit (part 3)

The audit uses the calculus tables, so items 1 to 6 above carry over. The
wrapper also reserves its encoder tables. On 8 packages the port's
instruction count is 114.8M against Rust's 99.8M. The wall-time gap is wider
than that. Cachegrind shows 5x the L1 write misses, from writing the input
words into fresh buffers twice (the encoder's `words()` and the port's
copy into `inw`), and 85k more mispredicted branches per 51 calls.

## Semver

The Rust-side encoder (`src/port.rs`) hash-conses every node of both
interfaces, so that the Vesper checker can compare nodes by offset. Before
this lane's semver PR it took about 56% of the port's time on 64 exports,
more than the whole Rust `compare`. The PR:

- hashes strings with Fx instead of SipHash (about 11% fewer port
  instructions);
- reuses the encoded node of every export whose declaration equals the old
  interface's (compared with `==` when it nests at most 64 levels);
- sizes the encoder's buffers from the interface, so small inputs do not
  zero 56 KB per call;
- appends message text with one `extend` instead of a checked byte loop
  (`util::put` was 25% of the empty-interface case).

Medians of 21 ABBA rounds, before → after: 64 exports minor change
2.39x → 1.80x, identical 2.17x → 1.63x, `check_release` 2.37x → 1.81x;
8 exports 2.47-2.96x → 1.93-2.29x; empty interfaces 4.21x → 3.20x.

What keeps it above 1.5x: on 8 exports, the encoder (`Encoder::end` alone
is 21% of instructions) and the Vesper checker each cost about as much as
the whole Rust `compare`. The checker's hot loops (`validate_function`,
`validate_type`, `function`) read every word through `try_get`, so each
read carries a bounds check and an `Option` match (see the `try_get` gap
below). Version parsing pays a sequence allocation per input and per
message.

## Crucible

Medians of 21 ABBA epochs (`bench-abba.sh 21 20`), before → after this
lane's crucible PR: draws 1.81x → 1.47x, tape text 1.80x → 0.85x, shrink
2.62x → 1.57x, fuzz 2.66x → 1.34x; DPOR (0.79x) and the cached explorer
(0.89x) were already faster than Rust. The changes:

- `source_draw` takes the one replayed word instead of the whole tape, so a
  draw no longer marshals a slice (29 instructions a draw).
- `draw_bytes` runs its whole flag and byte loop in one Vesper call
  (`source_draw_bytes`), with the tape read through `try_get` in the loop so
  `source_draw` inlines.
- `rng_below_inclusive` skips both divisions for power-of-two bounds
  (bytes, flags): Rust folds them because the bound is a constant after
  inlining, and the port's bound arrives as a variable.
- The shrinker copies tapes with `extend` instead of element by element,
  and keeps the best tape's key instead of re-summing it on every attempt.
- Tape text writes digits most-significant first with `leading_zeros_u64`
  and decodes them without a branch per digit: random hex digits
  mispredicted about half the time (115k mispredictions a run → 22k).

What keeps draws and shrink above 1.5x on some runs: every draw from a
Rust property closure is one call into Vesper (9 arguments, 4 on the
stack, results through memory), plus the Rust wrapper's bookkeeping. That
is about 100 instructions a draw against about 40 for Rust's inlined draw.
`source_draw` also zeroes 8 stack slots twice on entry (a codegen gap,
below). Collections drawn by a closure (`draw_vec`) cannot batch the way
`draw_bytes` does.

## TOML reader (part 4, #965)

`@vesper-std/toml` (packages/vesper-std/toml) is a transliteration of the
parser of `toml_edit` 0.22, which `toml::from_str` (0.8) runs first. It keeps
winnow 0.7's error model, so a rejected document gets the same text,
position and highlight. It builds the same tree, in IndexMap entry order,
with the same item and key spans. `@vesper/loom-calculus` exports it as
`read_toml` (a canonical dump) and `toml_accepts`.

`tests/toml_reader.rs` compares the dump with one built from toml_edit's
`ImDocument` and with `toml::from_str`'s error text:

- 74 fixed cases and every `.toml` file under packages/, crates/ and
  examples/;
- 20,000 mutated documents and 20,000 generated table structures per run
  (`VESPER_TOML_FUZZ` and `VESPER_TOML_SEED` scale them). After the fixes
  below, five seeds of 1,000,000 of each kind ran with 0 differences.

The fuzzing found three differences, all fixed:

1. A big negative float is `-inf`, not an error, because toml_edit only
   rejects `+inf` (`verify(|f| *f != f64::INFINITY)`).
2. An inline table's duplicate key inside an array is a cut, because
   `table_from_pairs` runs under `cut_err`.
3. A table header's duplicate key is shown in toml_write's default key form
   (bare, basic, literal, or escaped basic), not as written, because parsed
   keys keep no raw text.

Speed, `examples/toml_abba.rs`, in-process ABBA over the 122 Loom.toml files
of the repository (32 KB, Rust 791 us per pass): Vesper/Rust **0.96x**,
95% interval [0.93, 1.03], 21 epochs.

The reader has no flip of its own: the bar applies to `parse_manifest` end
to end (below).

## Manifest decoding and the `parse_manifest` flip (parts 5 and 6, #965)

`manifest_toml.ves` decodes the reader's tree into the manifest's JSON with
the messages and spans of `toml::from_str::<Manifest>` (serde derive and
toml_edit's deserializer). It covers what serde derive does that a hand
decoder usually misses:

- `deny_unknown_fields` text (`expected `a``, ``a` or `b``, `one of ...`),
  the first missing field in declaration order, reported on the table;
- structs written as arrays (`visit_seq`: positional fields, defaults,
  `invalid length i, expected struct X with N elements`);
- datetimes where a struct, a map or a package name is expected
  (`$__toml_private_datetime`);
- which span an error gets: a value's own errors take the value's span,
  `PackageName` errors take the caller's (the array in a set, the key in a
  map), and table entries prepend the key path;
- float text (serde_json's shortest form, through `@vesper/serde`'s
  Lemire and Dragon code, with an exact fallback for long literals on a
  rounding boundary, which the first build printed as `0.0`) and Rust's `Debug` escapes, with the toolchain's
  Unicode table generated into `@vesper-std/toml` (`escape.ves`, checked
  against the running toolchain by `debug_escape_table_matches_the_rust_toolchain`).

`semver_text.ves` parses versions and requirements with the `semver` crate's
error text, for the fields the manifest types as `Version` and `VersionReq`.

`parse_manifest` now calls the port under `vesper_port`; `toml` is only a
dev-dependency, for the differential tests:

- `tests/manifest_toml.rs`: fixed cases, every repository Loom.toml, and
  mutations; 8,000,000 mutated manifests over 8 seeds with 0 differences.
- `tests/semver_text.rs`: fixed cases and 1,000,000 mutations, 0 differences.

Speed, `examples/toml_abba.rs` (manifest decoding: `toml::from_str` against
the port and `serde_json::from_str` of its JSON, 122 manifests):
Vesper/Rust **1.29x**, 95% interval [1.28, 1.30]. The first build was 1.49x
[1.48, 1.52]; writing JSON runs without per-byte checks, assembling structs
from the output buffer's ranges, and a generated hash dispatch for field
names took it from 59.5M to under 48M instructions per pass. About 0.18 ms
of the 1.16 ms is the Rust-side `serde_json` read that turns the JSON into
the `Manifest` value.

## Compiler gaps

- **Slice parameters block inlining** (also reported by the P1 semver lane).
  This is why every port copies its input into a local `ChunkSeq`
  (`new_l`), which costs an extra pass over the input and the L1 write
  misses above.
- **Seq field access reloads the header.** Every `l.x.len()` or `l.x[i]` on a
  field of an `inout` struct reloads the field pointer and the header, even
  in a loop that does not change them (`decode_tables` disassembly). Small
  loops spend about half their instructions on these reloads and on spills.
- **`a[lo..hi].cmp(in b[..])` on bytes is an inline byte loop**, not
  `memcmp`. It is fine for short strings but about 2.5x the instructions of
  word-wise comparison on the 47-byte average strings of the audit catalog.
- **`try_get` reads are not hoisted or fused.** A checker that walks a word
  stream by computed offsets (semver's `at(in w, node, field)`) pays a bounds
  check and an `Option` match on every word, even when the node's length was
  checked on entry. This is the bounds-hoisting lane's open item
  ("vectorizing try_get loops"); semver's `validate_*` functions are a small
  reproducer.
- **`@vesper/serde` has no slow path for decimal to double.** Its
  `decode_bits` reports the undecided case (more than 19 digits on a
  rounding boundary) and the Rust caller uses `str::parse`. The manifest
  decoder settles it with an exact big-integer bisection of its own
  (`exact_bits`); a shared slow path in `@vesper/serde` would serve both.
- **Small allocations dominate tree walks.** After the fixes above the
  manifest decoder's profile is `malloc`/`free` from `ChunkSeq` temporaries
  in `struct_value`, `basic_string` and `simple_key`. Scratch buffers that
  outlive a call (or stack-allocated small sequences) would remove most of it.
- **Dead stack stores on entry.** `vesper_crucible_source_draw` zeroes each
  of its 8 spill slots twice before any use (16 stores per call). Small
  functions called once per draw pay for it on every call (see the native
  code size lane).

## Checks run (Linux x86_64)

- `cargo test --release` of calculus, audit, solver and both seeds.
- `scripts/test-port-harness.sh` on calculus, audit and solver: every
  divergent module was detected.
- `cargo +1.100.0 clippy --all-targets -D warnings`, also with
  `vesper-port-divergent`. `cargo fmt` and `vesper fmt --check` pass.
- Stage 1 guard: `bootstrap/run.sh stage1 --reproduce --outcome` exits 0
  ("cross-backend gate: ok") after each seed change. The only differences
  are stale artifact digests; programs, results and verified status match.

## Census

`scripts/rust-census.py` STATUS rows are updated for pact, loom-calculus,
loom-audit and semver. `docs/maintenance/rust-census.md` is left for the
scoreboard lane to regenerate, as its line counts move with every PR.
