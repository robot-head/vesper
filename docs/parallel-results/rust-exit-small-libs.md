# Rust exit SH6 lane 12: small libraries, Chorus and Pact

Claim [#1407](https://github.com/robot-head/vesper/issues/1407). Prompt:
[12-small-libs.md](../agent-prompts/self-host-sh6/12-small-libs.md). The lane
carries #915 (Crucible), #965 (TOML reader) and #1068 (Chorus). Started
2026-11-21 from `main` at `ab965f2a`. This report grows with each part.

## Status by crate

| Crate | State | Speed (port / Rust, ABBA) | What keeps Rust |
|---|---|---|---|
| vesper-pact | Rust gone from `src/` (part 1, #1427) | unchanged (port was already the only build) | the seed, until Stage 2 |
| vesper-loom-calculus | flipped, Rust in the seed (part 2, #1449) | 8 pkgs 1.12x [1.03, 1.38]; 64 pkgs 0.80x [0.74, 0.91]; 256 pkgs about 0.77x | the seed; TOML, JSON and SemVer text readers (#965) |
| vesper-loom-audit | flipped, Rust in the seed (part 3, this PR) | 8 pkgs 1.37x [1.34, 1.41] (n=54); 64 pkgs 1.15x [1.05, 1.27]; 256 pkgs 0.96x [0.91, 1.04] | the seed; input-size checks and BLAKE3 context digests (serde_json + blake3), until `vesper-blake3-sys` lands |
| vesper-semver | above the bar | 2.2-3.0x; empty interfaces 4.2x | encoder design (below) |
| vesper-beacon | above the bar | exp 3.5x, hist 4.5x, spans 2.1x, traceparent 1.0x | per-call FFI cost (calls of a few ns) |
| vesper-crucible | partly above the bar | draws 1.81x, tape 1.80x, shrink 2.62x, fuzz 2.66x, dpor 0.80x, cached 0.92x | not ported: fuzzer driver, schedule explorers, arena |
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
interfaces, so that the Vesper checker can compare nodes by offset. On 64
exports it takes about 56% of the port's time. That is more than the whole
Rust `compare`. A word-hash for the string interner saves about 11% of the
port's instructions. A packed slot table and inline comparison save almost
nothing, because the cost is per word hashed and compared. Reaching 1.5x
needs a design that does not hash-cons every node: for example, hashing
only the classes the checker compares by offset, or memoizing equal
declarations before they are encoded.

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
