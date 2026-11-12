# Warp connection tiles, SIMD CID lookup and RSS steering (wave four, lane 6)

Design §35.1 item 4 and §35.3 `QuicConnTile` / `lookup_cid_simd`.
Prompt: [06-connection-shards.md](../agent-prompts/wave-four/06-connection-shards.md).

- Base: `a5fa024b8d6f62eaa336337d825a2be2ec8972b4` (origin/main at start).
- Branch: `claude/project-thread-balc0t`. The PR closes issue #42.
- New crate: `crates/vesper-warp-shard` with no dependencies at all (std only).

## What is implemented

| Item | Where | Notes |
| --- | --- | --- |
| AoSoA tiles of 16 connections | `src/tile.rs` | Hot, key and cold arrays; layout asserted at compile time |
| Fixed-capacity table, generation-checked handles | `src/table.rs` | insert / get / set / remove / compact, no allocation after `new` |
| SIMD CID compare, 16 lanes at once | `src/simd.rs`, `ConnTable::tile_match_mask` | SSE2, AVX2, NEON, scalar; runtime detection |
| Open-addressed CID index | `src/index.rs` | Linear probing, backward-shift delete, load ≤ 1/2, keyed hasher |
| Batch of ≤ 64 (slot, event) descriptors | `src/batch.rs` | Atomic; vector apply plus scalar reference |
| Idle-timeout scan | `ConnTable::scan_idle{,_with}` | Vector `deadline <= now` compare, resumable output buffer |
| Toeplitz RSS, indirection table, 5-tuples | `src/rss.rs` | Table-driven hash plus bit-serial reference |
| Shared-nothing shard set | `src/shard.rs` | One owned `ConnTable` per shard, no locks/atomics/`Arc` |

### Tile layout

All three tile types are `#[repr(C, align(64))]`; sizes and every field offset
are `const` assertions in `src/tile.rs`.

| Tile | Fields (16 lanes each) | Bytes | Lines |
| --- | --- | --- | --- |
| `HotTile` | `largest_rx_pn_p1`, `next_tx_pn`, `bbr_bw_bytes_per_sec`, `bbr_min_rtt_ns`, `next_edt_ns` (u64); `cwnd_bytes`, `bytes_in_flight` (u32) | 768 | 12 |
| `KeyTile` | local-CID words `[[u32; 16]; 5]`, `occupied: u16` | 384 | 6 |
| `ColdTile` | `idle_deadline_ns` (u64), `generation` (u32), `peer_cid` (20 B), `peer_cid_len` | 576 | 9 |

That is 1728 B per tile, 108 B per connection. Each u64 field is two whole
cache lines and each u32 field one. An 8-byte CID compare touches two key
lines plus the occupancy line.

### CID lookup

- `lookup(dcid)`: hash index, O(1) average. It stores 8-byte entries (low 32
  bits of the hash, slot) and verifies against the tile key words, so a hash
  collision costs one more compare, never a wrong hit. The default hasher is
  `RandomState` (SipHash-1-3, random keys), so probe lengths don't depend on
  attacker-chosen CIDs. Callers can supply any `BuildHasher`.
- `lookup_cid_simd(dcid)` / `lookup_cid_scalar(dcid)` / `lookup_scan_with`:
  index-free scans of all tiles. Per tile, the occupancy mask is ANDed with one
  16-lane `u32` equality mask per CID word: 2 words for 8-byte CIDs, up to 5
  for 20-byte CIDs.
- The CID length is fixed per table (1..=20) because short headers don't carry
  it. Lookups or inserts with a different length are typed errors.

### Kernels (`src/simd.rs`, the only module with `unsafe`)

The crate is `#![deny(unsafe_code)]`, and `simd` alone is `#[allow]`ed. The
`unsafe` there is limited to bounds-asserted unaligned loads and stores on
`[T; 16]` arrays, and to calls into `#[target_feature]` functions through
`Kernels`. `Kernels` can only be built after `IsaPath::is_supported()` is true.

| Kernel | SSE2 | AVX2 | NEON |
| --- | --- | --- | --- |
| `eq_mask_u32` | `cmpeq_epi32` + `movemask_ps` | `cmpeq_epi32` + `movemask_ps` | `vceqq_u32`, weighted `vaddvq_u32` |
| `gt_mask_u64` (unsigned) | emulated from 32-bit compares (hi > or hi == and lo >) | sign flip + `cmpgt_epi64` | `vcgtq_u64` |
| `max_u64` | gt + and/andnot/or | gt + `blendv_epi8` | `vbslq_u64` |
| `add_u64`, `sub_u64` | `add/sub_epi64` | `add/sub_epi64` | `vaddq/vsubq_u64` |

### Batch semantics

These are the same for `apply_batch` (vector) and `apply_batch_scalar`
(reference). For each connection in the batch:

- `largest_rx_pn` becomes the max of its old value and every received pn
  (pn < 2^62).
- `bytes_in_flight` becomes old + Σsent − Σacked − Σlost, which must lie in
  `0..=u32::MAX`. Only the net value is checked, so event order inside a batch
  doesn't matter.
- `cwnd` becomes the last `CwndUpdate`, if there is one.

Any invalid descriptor or net value rejects the whole batch and leaves the
table unchanged. Error precedence is defined and identical for both paths:

1. `TooLong`.
2. The first bad descriptor (`StaleHandle`, `PacketNumberRange`).
3. The out-of-range connection whose first descriptor is earliest.

The vector path first scatters descriptors into per-tile 16-lane staging
arrays. That step is scalar, because descriptors address arbitrary slots. It
then widens `bytes_in_flight` to u64, adds the sent bytes, and detects
underflow with the unsigned gt kernel before subtracting. Overflow uses gt
against `u32::MAX`, and packet numbers use the max kernel.

## Acceptance evidence

All commands ran on an x86_64 Linux host with
`CARGO_PROFILE_{DEV,TEST}_DEBUG=0`, `CARGO_INCREMENTAL=0` and a private target
directory. `vesper-loom-build` derives its engine fingerprint from the
workspace lockfile via `OUT_DIR`, so with a target directory outside the
workspace its 14 cache tests fail with `UnknownFingerprint`; they pass when
rerun with the default `target/` (a property of this environment, not of this
change).

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy -p vesper-warp-shard --locked --all-targets -- -D warnings` | pass |
| `cargo clippy -p vesper-warp-shard --all-targets --target aarch64-unknown-linux-gnu -- -D warnings` | pass (NEON path type-checked) |
| `cargo test -p vesper-warp-shard --locked` | 18 tests pass |
| `cargo clippy --workspace --locked --all-targets -- -D warnings` | pass |
| `cargo test --workspace --locked` | 807 tests pass (Lean not installed here, so `VESPER_LEAN` unset; this crate has no Lean checks) |

Tests (`crates/vesper-warp-shard/tests/`):

- `rss.rs`
  - All eight Microsoft RSS verification vectors (five IPv4 and three IPv6),
    with and without ports, pass through both the table-driven hash and the
    bit-serial reference.
  - The table hash equals the reference for 50 random keys and every input
    length from 0 to 36.
  - Steering is deterministic. 200,000 random v4/v6 UDP tuples over 8 shards
    with a 128-entry round-robin table put every shard within **5 %** of the
    mean (the documented tolerance).
  - Rewriting the indirection table moves flows.
  - Typed errors cover input length, mixed families, table length, shard count
    and out-of-range entries.
  - Four shards are moved to four threads with `std::thread::scope`, and each
    one's CIDs are absent from the others.
- `equivalence.rs`: every supported ISA path is checked against scalar.
  - Kernels on 20,000 random lane sets per path, with edge values (0,
    `u32::MAX`, 2^32, `i64::MAX`, 2^63, `u64::MAX`) and forced equal high
    dwords.
  - Lookup through the index, the vector scan and the scalar scan agree, and
    agree with a model, on churned tables.
    - Five configs with CID lengths 1, 2, 5, 8 and 20.
    - Tables have empty slots and reused slots with bumped generations.
    - Duplicate inserts are rejected; the 1- and 2-byte alphabets force real
      duplicates.
  - Batch: 4,000 random batches per path, including stale handles, pn ≥ 2^62,
    near-`u32::MAX` sizes, and underflow and overflow.
    - Vector and scalar results, errors and resulting table state are
      identical.
    - Failed batches leave the table unchanged.
    - The test asserts that more than 500 batches succeeded and more than 200
      failed.
  - Idle scan: 200 random `now` values, output buffers of 1, 7, 16, 64 and
    1000, resumed to completion; vector and scalar results match the model.
  - Compaction preserves every connection's state, makes every old handle
    stale, reports every relocation, and keeps the index and the vector scan
    consistent.
  - Config and input rejection.
- `zero_alloc.rs`: a counting global allocator (per-thread counter).
  - After one warm-up round, four more rounds allocate **0** times.
  - Each round inserts 4,096 connections, runs index lookups and vector scans,
    applies 200 × 64-event batches through both paths, runs a resumable idle
    scan, removes, compacts and drains.
  - 1,000 RSS steerings also allocate nothing.
- `million.rs`
  - Builds a 2^20-connection table, does 200,000 hit and 200,000 miss lookups,
    and spot-checks the index against the full vector scan.
  - Measured memory is 134,321,664 B (128.1 B per connection) against a
    documented bound of 132 MiB: tiles 108 B, free stack 4 B and index 16 B
    per connection, plus fixed staging. The max probe length was 41 with
    random SipHash keys.

### ISA paths actually executed

- **On this host (x86_64):** detected `avx2`. Scalar, `sse2` and `avx2` all ran
  in the equivalence, alloc and 1M tests.
- **NEON:** compiled and Clippy-checked for `aarch64-unknown-linux-gnu`, but
  not executed here. The existing macOS (AArch64) CI job runs
  `cargo test --workspace`, which executes the NEON path; `reports_isa_paths`
  asserts that NEON is detected there.

## Decisions

- **No dependency on `vesper-simd` or `vesper-alloc`.**
  - `vesper-simd` is a Cranelift JIT byte scanner. Its kernels and ISA
    profiles don't cover lane-wise u32/u64 compares, and depending on it would
    pull Cranelift into this crate.
  - The fixed tile arrays need no allocator beyond the one-time boxed slices.
- **Local vs peer CID.** §35.3 lists `peer_cid` as the cold field and matches
  incoming DCIDs against it. In RFC 9000, an incoming packet's DCID is a CID
  that *we* issued, so lookup keys are stored as a separate local-CID key tile.
  `peer_cid` stays a cold field, used as the DCID for packets we send.
- `largest_rx_pn` is stored as `pn + 1`, so 0 means "nothing received" and the
  unsigned max kernel works without a sentinel.
- Slot generations use `wrapping_add`, on purpose: a stale handle can alias
  only after 2^32 reuses of the same slot while the old handle is still held.
- UDP ports are hashed under `HashFields::AddressesAndPorts`, matching Linux
  `ethtool rx-flow-hash udp4 sdfn`. Windows RSS hashes UDP by addresses only;
  `HashFields::Addresses` gives that behaviour.

## Limits and remaining gaps

- **No hardware RSS.** No NIC RSS key or indirection table is programmed.
  Steering is a software model validated against published vectors.
- **No performance claims.** There are no line-rate, throughput or latency
  claims. All tests ran in the unoptimised test profile.
- **Some batch steps are scalar.** In the vector batch path, the scatter, the
  u32 ↔ u64 widening and narrowing of `bytes_in_flight`, and the `cwnd` blend
  are scalar loops over the 16 lanes. Only the add/sub/compare/max steps use
  intrinsics.
- **Steering is by 5-tuple only.** CID-based shard routing (for connection
  migration, where the 5-tuple changes) is left to integration.
- **No linear obligations here.** `SlotHandle` is `Copy`; misuse is caught by
  generation checks, not by linear ownership. Rust ownership here is not
  evidence that Vesper's source typing rules for `tiled(16)` / `TiledSeq` are
  implemented. Strata's generic `TiledSeq` still doesn't exist.
- **No Lean model.** None was assigned to this lane.
- **Root patch:** `vesper-warp-shard` is added to root `members` and
  `default-members`, and `Cargo.lock` is regenerated, in a separate commit. No
  new workspace dependencies.
