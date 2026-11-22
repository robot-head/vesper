# 13 Strata regions and memory layout

A parser may allocate thousands of nodes that all expire at the end of one request. A region gives them a common lifetime. Their layout determines how much unrelated data the processor has to load when it scans them.

<!-- technical-detail -->

A parser or storage engine may allocate thousands of short-lived nodes that all expire together at the end of one request. Vesper Strata groups related allocations into lexically scoped regions, reuses uniquely owned constructors in place without reference counts, and tiles collection records so processor scans touch hot fields without loading cold metadata.

Built-in sequences (`ChunkSeq[T]`, `Slice[T]`), UTF-8 strings (`String`, `Str`), and the thirteen `@vesper-std/*` library packages in `packages/vesper-std/` (backed at the OS edge by `crates/vesper-std-host`) are implemented and tested across the interpreter, Cranelift JIT, and native executables. Host-side `Region`, `ShardSlab`, `VirtualArena`, `CacheBTree`, and `RegionTiled2` prototypes live in `crates/vesper-alloc` (`REGION_LOWERING_CONTRACT_VERSION = 0`, `REGION_ACCESS_CONTRACT_VERSION = 1`). Per `REVIEW-STATUS.md` (`SYS-02`, `TYPE-03`), `ChunkSeq`, `TiledSeq`, and `Box` currently allocate ambiently rather than charging `eff [Alloc]`, compiler lowering for `eff [r: Region]` escape and watermark liveness remains open, and `@fip` constructor reuse plus `tiled(N) struct` surface lowering are design specifications rather than active compiler passes.

## 13.1 Decide who can allocate

An allocation capability makes allocation visible in a function’s effects. A region groups objects that can be reclaimed together. A general heap allocation has a different lifetime and cost. Consider both the longest-lived reference and the fields the program repeatedly visits when selecting a layout.

<!-- technical-detail -->

Ambient global allocators hide heap traffic inside ordinary functions and suffer up to 2x fragmentation slowdowns under long-running adversarial workloads. Strata (`@vesper-std/alloc`) proposes factoring dynamic memory authority into two explicit lexical capabilities in `Sigma`: `eff [Alloc]` for general-purpose core-local slab allocation, and `eff [r: Region]` for lexically scoped bump arenas tied to a capability label `r`. In the target effect discipline, any function whose `eff [...]` row omits both `Alloc` and `Region` carries a static zero-allocation guarantee, though today built-in `Box`, `ChunkSeq[T]`, and `TiledSeq[T]` still allocate ambiently until `Alloc` charging lands.

~~~vesper
// General-purpose sharded slab authority in Sigma:
linear effect Alloc {
    fn alloc_raw(size: usize, align: usize) -> RawBlock;
    fn dealloc_raw(sink block: RawBlock) -> Unit;
}
// Lexically scoped virtual-memory bump arena branded by capability label r:
linear effect Region {
    fn bump_alloc(size: usize, align: usize) -> RegionPtr;
    fn checkpoint() -> Watermark;
    fn rewind(sink mark: Watermark) -> Unit;
}
~~~

Strata pairs explicit allocation effects with three structural layout mechanisms: `RegionBox[r, T]` and `SlotArena[r, V]` / `RegionRef[r, T]` replace `'arena` lifetime parameters and `Rc`/`RefCell` graph bookkeeping with capability-branded handles mediated by `inout r`; `@fip` reuses uniquely consumed tree nodes at zero header cost; and `tiled(N) struct` (`AoSoA`) segregates `hot` scan columns from `cold` metadata so `[T-CallProj]` field borrows feed SIMD registers directly.

## 13.2 Keep a region reference within its scope

A region introduces a fresh lifetime and an allocation capability. Objects within it can refer to each other, even cyclically, if no reference survives reclamation. To rewind to a checkpoint, first establish that every reference into the reclaimed portion has expired. Returning an owned scalar summary can be simpler than returning the temporary parse tree.

<!-- technical-detail -->

To prevent dangling arena pointers without infecting structs with Rust-style `'arena` lifetime parameters, Strata indexes every region handle (`RegionBox[r, T]`, `RegionSlice[r, T]`, `RegionRef[r, T]`) by the lexical capability name `r` introduced in `Sigma` by `Region::scoped`. The proposed typing rules `[T-RegionAlloc]` and `[T-RegionScope]` check that `r` does not appear in `free_caps(tau_out)` when the scope exits, while preserving `T`'s substructural mode `m` on `RegionBox[r, T]`.

    [T-RegionAlloc]
      (r : Cap_linear[Region]) in Sigma    Delta; Sigma; Gamma |- e : tau @ m | eps
      ---------------------------------------------------------------------------------
      Delta; Sigma; Gamma |- r.box(e) : RegionBox[r, tau] @ m | eps U {r : Region^linear}
    ----
    [T-RegionScope]
      Delta; (Sigma, r : Cap_linear[Region]); Gamma |- e_body : tau_out @ m_out | {r : Region^linear} U eps
      r not_in free_caps(tau_out)
      -----------------------------------------------------------------------------------------------------
      Delta; Sigma; Gamma |- Region::scoped(\r. e_body) : tau_out @ m_out | eps

Modal preservation keeps bulk reclamation sound across ownership classes: when `T : mod copy`, `RegionBox[r, T]` crosses via `[T-Cross]` to `copy`, allowing intra-region DAGs or cyclic graphs and `O(1)` cursor reset without running destructors; when `T : mod linear` (such as `WriteTxn`), `RegionBox[r, T]` stays `linear` so scope exit or `rewind` is rejected until `T` is unpacked and consumed; and when `T : mod affine` wraps external OS handles, it links onto a LIFO finalizer chain (elided when `T` owns only allocations inside the same region `r`). Because `[T-RegionScope]` discharges `r` from the outer effect row `eps`, a pure package declaring `effects = []` in `Loom.toml` can use a bounded `Region::scoped` scratchpad internally and return a compact `copy` summary. Within a scope, `scratch.checkpoint()` mints a linear `Watermark` token and `scratch.rewind(sink mark)` resets per-iteration temporaries in `O(1)`:

    VirtualArena Chunk [virtual_base .. bump_limit] (mmap MAP_NORESERVE, MADV_FREE on reset)
    ┌──────────────────────┬──────────────────────────────┬──────────────────┬─────────────┐
    │ Pre-Mark Survivors   │ Per-Batch Scratch (rewound)  │ Free Bump Space  │ Guard Page  │
    │ latest_by_key, tiles │ temp AST, decode_batch nodes │                  │ PROT_NONE   │
    └──────────────────────┴──────────────────────────────┴──────────────────┴─────────────┘
    ▲                      ▲                              ▲                  ▲
    virtual_base           mark (Watermark checkpoint)    bump_cursor        bump_limit
                           │                              │
                           └──◀── rewind(sink mark) ──────┘ (walks LIFO FinalizerNode chain;
                                                             requires active_linear_count == 0;
                                                             SYS-02: post-mark handles go Stale)

~~~vesper
fn compact_log_segment[P: IsaProfile](
    inout out_frame: ByteSlice,
    in raw_segment: ByteSlice,
) -> Result[u32, CompactError] eff [Isa[P], Fail[CompactError]] {
    Region::scoped(256.kib(), |cap scratch| eff [scratch: Region, Isa[P], Fail[CompactError]] {
        let mut latest_by_key = FlatTable::with_region[u64, LogOffsetEntry](in scratch, 4096);
        let mut entries = TiledSeq::with_region[LogOffsetEntry](in scratch, 4096);
        for batch_hdr in raw_segment.iter_wire_headers[KafkaBatchWireHeader]()? {
            let mark: Watermark = scratch.checkpoint();
            let summary = decode_and_dedup_batch(in scratch, inout latest_by_key, in batch_hdr)?;
            entries.push(summary);
            scratch.rewind(sink mark);
        }
        flush_compacted_tiles[P](inout out_frame, in entries)
    })
}
~~~

Mandatory `SYS-02` review limitation (`content.typ` line 3419 and `REVIEW-STATUS.md`): checking `r` absence in `tau_out` alone is insufficient for soundness because a region brand must also be prevented from escaping through post-call contexts, mutable `inout` or `out` parameters, or lexical effect handler state. Likewise, `scratch.rewind(sink mark)` is unsound if any handle allocated after the watermark remains live or if a pre-watermark collection grows into storage above the watermark—a liveness property the static rules above do not yet establish. In `crates/vesper-alloc`, the host runtime mitigates both hazards dynamically by pairing every `RegionBox`, `RegionBytes`, and `Watermark` with the minting `RegionId` and a monotonically increasing, never-reused serial, returning `HandleError::Stale` on rewound or foreign handles and `RegionError::ProtectedByWatermark` if a pre-watermark `RegionVec` attempts to reallocate across an active checkpoint.

## 13.3 Match the allocator to the request

A sharded slab serves repeated small allocations from size classes, keeping common operations local. A virtual arena reserves address space and advances a cursor as callers request storage. Reclaiming a whole arena is cheap when lifetimes align. Consider stranded capacity and peak resident memory as well as the cost of one allocation.

<!-- technical-detail -->

Strata backs `eff [Alloc]` and `eff [r: Region]` with two hardware-sympathetic allocator tiers designed to eliminate atomic contention on small objects and `memcpy` growth stalls on contiguous buffers.

| Tier / Pool | Backing Capability | Geometry & Fast Path | Reclamation & Cross-Core Policy |
| --- | --- | --- | --- |
| ShardSlab | eff [Alloc] | 64 KiB slabs, 48 size classes (8B..16KiB), | 0-atomic local LIFO pop/push; 64-slot batched |
                                        3-instruction intrusive freelist            remote free queue (REMOTE_FREE_BATCH) across cores
    DmaSlab        eff [Alloc] + DMA    2 MiB hugepages in Hardware Domain 1,       Pre-registered via IORING_REGISTER_BUFFERS for
                                        shared with Pulse io_uring & Bridge MPK     zero-copy kernel rings and isolated FFI
    VirtualArena   eff [r: Region]      mmap(MAP_NORESERVE) contiguous reservation  O(1) cursor reset + MADV_FREE page recycling;
                                        with lazy 4 KiB / 2 MiB page faults         egglog bump coalescing via r15 / x28 vmctx

In the full multi-threaded design, each Pulse worker shard owns private `ShardSlab` bins and returns migrated `portable` fiber deallocations in 64-slot cache-line-aligned message batches (`snmalloc` style). In the current repository (`crates/vesper-alloc`), `ShardSlab` is prototyped as a bounded single-threaded allocator using non-atomic `Rc` bookkeeping (`!Send`, `!Sync`) where `SlabBlock` owners keep backing slab storage alive even if the parent pool handle is dropped, and `VirtualArena` implements bounded virtual reservations with `VirtualMark` checkpoints while `vesper-opt-egglog` bump coalescing via `r15`/`x28` remains a compiler target.

## 13.4 Reuse a consumed constructor

Functional-but-in-place (`@fip`) rewriting consumes an old data constructor and reuses suitable storage for its replacement. For a tree transformation, the visible result can be a new tree even when individual nodes occupy their old addresses. Reuse depends on ownership and layout compatibility.

<!-- technical-detail -->

Functional tree transformations in Koka and Lean 4 store an 8-byte reference-count header ahead of every heap node and emit a runtime `rc == 1` conditional branch before reusing storage in place. In Vesper, a `sink` parameter statically proves unique ownership at compile time, allowing the proposed `@fip` (`vesper-opt-egglog`) pass to pair unpacked `Box` `ReuseSlot` tokens 1-to-1 with same-sized `Box::new` allocations using 0-byte object headers, zero runtime `rc == 1` branches, and an empty `eff []` allocation row.

~~~vesper
enum Color { Red, Black }
affine struct RbNode[K, V] {
    color: Color, key: K, val: V,
    left: Option[Box[RbNode[K, V]]],
    right: Option[Box[RbNode[K, V]]],
}
@fip fn balance_left[K: mod copy, V](
    color: Color, key: K, sink val: V,
    sink left: Option[Box[RbNode[K, V]]],
    sink right: Option[Box[RbNode[K, V]]],
) -> Box[RbNode[K, V]] eff [] {
    match left {
        Some(Box(RbNode {
            color: Color::Red, key: lk, val: lv,
            left: Some(Box(RbNode { color: Color::Red, key: llk, val: llv, left: a, right: b })),
            right: c,
        })) => {
            let new_left = Box::new(RbNode { color: Color::Black, key: llk, val: llv, left: a, right: b });
            let new_right = Box::new(RbNode { color: Color::Black, key, val, left: c, right });
            Box::new_in_caller(RbNode { color: Color::Red, key: lk, val: lv, left: Some(new_left), right: Some(new_right) })
        }
        other_left => Box::new_in_caller(RbNode { color, key, val, left: other_left, right }),
    }
}
~~~

Here `Box::new_in_caller` writes the root constructor directly into caller-provided return storage while the two child nodes reuse the exact addresses unpacked from `left`.

## 13.5 Store frequently scanned fields together

An array of structures keeps each record together. A structure of arrays puts each field in a separate run. An array of structures of arrays (AoSoA) groups records into tiles and stores each field contiguously within a tile. Updating positions for eight objects can then load one tile’s positions without loading all eight objects’ metadata.

A hot/cold split takes infrequently accessed fields out of the scan path. A chunked sequence grows by adding chunks. A transient collection lets a unique owner perform a series of changes before publishing a stable value. Choose based on how often the workload scans, grows, or shares the collection.

<!-- technical-detail -->

Standard Array-of-Structures (`AoS`) records pull cold metadata into L1 cache lines during tight scans, while external Structure-of-Arrays (`SoA`) macros break field borrowing. Strata proposes `tiled(N) struct` (`N` in `8, 16, 32, 64`), an Array-of-Structures-of-Arrays (`AoSoA`) layout that packs `N` `hot` fields contiguously on 64-byte L1 cache-line boundaries while isolating `cold` fields in a page-aligned side slab (`crates/vesper-alloc` prototypes two-column 64-byte-aligned tiles in `RegionTiled2` up to a 64 KiB tile ceiling, and `vesper-verify-lean` models `TiledSeq[T]` as `Array T`):

    Tile k (16 records): Hot Columns Contiguous on 64B L1 Boundaries     Cold Side Array (Page-Aligned)
    ┌──────────────────────────────────────────────────────────────┐     ┌────────────────────────────┐
    │ base_offset[0..8]  (8 x u64 = 64B, L1 line 0)                │     │ producer_id[0..16]  (128B) │
    ├──────────────────────────────────────────────────────────────┤     ├────────────────────────────┤
    │ base_offset[8..16] (8 x u64 = 64B, L1 line 1)                │     │ compression_codec   (16B)  │
    ├──────────────────────────────────────────────────────────────┤     └────────────────────────────┘
    │ batch_crc[0..16]   (16 x u32 = 64B, L1 line 2) ──▶ SIMD load │       (untouched during CRC scan)
    ├──────────────────────────────────────────────────────────────┤
    │ byte_len[0..16]    (16 x u32 = 64B, L1 line 3)               │
    └──────────────────────────────────────────────────────────────┘

~~~vesper
tiled(16) copy struct LogOffsetEntry {
    hot base_offset: u64,
    hot batch_crc: u32,
    hot byte_len: u32,
    cold producer_id: u64,
    cold compression_codec: u8,
}
fn count_matching_crc[P: IsaProfile](
    in index.{batch_crc}: TiledSeq[LogOffsetEntry],
    target_crc: u32,
) -> u64 eff [Isa[P]] {
    let mut hits: u64 = 0;
    for tile_idx in 0..index.tile_count() {
        let crc_vec = index.load_tile_vec[P, batch_crc](tile_idx);
        let mask = crc_vec.simd_eq(VVec::splat[P, u32](target_crc));
        hits = hits.wrapping_add(mask.count_ones() as u64);
    }
    hits
}
~~~

In the shipped compiler (`content.typ` lines 3545–3554), dynamic buffers and views are built around `ChunkSeq[T]`, `Slice[T]`, `String`, and `Str`. `ChunkSeq[T]` is a built-in one-word growable buffer with doubling capacity supporting `new`, `with_capacity`, `push`, `pop`, `len`, `get`, `set`, `try_get`, `try_set`, `clear`, `truncate`, `reserve`, `extend` (`extend_from_slice`), `as_slice`, and `as_slice_mut`. `s.reserve(n)` is a total capacity hint; unrecoverable out-of-memory growth terminates native executables with exit status `71`. Its mode joins `T`'s mode with `affine`: a `ChunkSeq` of `linear` items cannot be implicitly dropped and must be drained before calling `ChunkSeq::finish`, which raises `Fail[RuntimeError]` (`IndexOutOfBounds`) if elements remain. While `get`, `set`, and `s[i]` raise `Fail[RuntimeError]` on out-of-range indices, `try_get` and `try_set` are total (`Option` / `bool`) so methods holding a live `inout` loan—across which abortable effects like `Fail` are forbidden (`E0402`)—can index safely. Native codegen frees owned `ChunkSeq` and `String` buffers on rebinding and on every function, handler `return`, or `Fail` exit (element drop glue inside sequences is not yet automatic).

`Slice[T]` (`s[a..b]` or `s.as_slice()`) is second-class: it is valid only as the whole type of an `in` or `inout` parameter (`E1520`) and may be written only as a call argument, a reading `Slice` method receiver (`b[a..c].find_byte(x)`), or the initializer of an immutable bound view (`E1521`). Two `inout` slices of one sequence in a single call must be disjoint by syntactic boundary (`s[0..k]` and `s[k..n]`) or known ordering facts (`E0301`), and an out-of-bounds slice range aborts with status `71`. Sequences and slices of `copy` elements (`E1522` otherwise) provide block operations `copy_from(at, in src)` / `try_copy_from` (rejecting self-views via `E0301`, while permitting disjoint sibling field paths like `q.a.extend(in q.b[0..k])`), overlap-safe `memmove` `copy_within(lo, hi, at)` / `try_copy_within`, `fill(v)` and `fill(lo, hi, v)` / `try_fill`, plus `ChunkSeq`-only (`E1525` on `Slice`) `resize(n, v)`, `extend_from_within(lo, hi)` / `try_extend_from_within`, and `extend_reversed(in src)`. On `ChunkSeq[u8]` and `Slice[u8]`, `read_u16_le`, `read_u16_be`, `read_u32_le`, `read_u32_be`, `read_u64_le`, and `read_u64_be` alongside `write_u16_le` through `write_u64_be` (and total `try_read_*` / `try_write_*`) validate the full `2`-, `4`-, or `8`-byte interval up front so short buffers are never partially written; byte slices also provide total `sum_be16()` (mod `2^64` for RFC 1071 checksums), `eq`, three-way `cmp` (`-1`, `0`, `1`), `find_byte`, `find_either(a, b)`, `find_not_in(lo, hi, high)` (testing `u64` bitmaps `lo` for `0..64` and `hi` for `64..128` plus `bool` `high` for `128..256`, skipping 16-byte runs of repeated class bytes even on x86-64 without SSSE3), `eq_ignore_ascii_case`, `extend_ascii_lowercase`, and `make_ascii_lowercase(lo, hi)` / `try_make_ascii_lowercase`.

For non-`copy` elements (which cannot be copied out by `s[i]` or `get`, `E1522`), Vesper supports total place primitives `swap(inout a, inout b)` and `replace(inout a, v)` (requiring disjoint `inout` places `E0308`/`E0300`/`E0402`, never second-class views `E1520`, bypassing `E0305` move-out restrictions on `inout` fields), `s.swap(i, j)` (no-op when `i == j < s.len()`), and in-place element views: `f(in s[i])`, `f(inout s[i])`, `match s.try_get(i) { Some(x) => .., None => .. }`, and match-scrutinee-only `match s.try_get_mut(i)` (`E1522`). Distinct integer literals (`swap(inout s[0], inout s[1])`) check statically, while dynamic multi-element borrows `f(inout s[i], in s[j])` evaluate indices first and raise `IndexOutOfBounds` before lending if `i == j`; field paths through elements (`s[i].f`, `s[i].f.g = v`) desugar through `try_get`/`try_get_mut` with `Fail[RuntimeError]` (two element paths on one sequence in one call or `f(in st, inout st.err)` remain `E0402`). `String` (`affine` UTF-8 `{len, cap, data}`) and second-class `Str` (`in` parameter or static-text function return only, `E1530`) uphold RFC 3629 (`String::from_utf8`, `push_str`, `push_char`, `next_char`, `char_at`, `char_count`, `is_char_boundary`, status `71` on off-boundary slices). Immutable bound views (`let b = name.as_bytes();`, `let sub = w.text[a..b];`) freeze immutable owners against writes (`E0313`) and moves (`E1531`) until block exit while rejecting `let mut`/`inout`/`sink` sources; because `Option[Str]` is illegal, `@vesper-std/bytes::views` (`find`, `rfind`, `starts_with`, `ends_with`, `split_once`, `rsplit_once`, `strip_prefix`, `strip_suffix`) and `@vesper-std/intern` (`lookup_view`, `lookup_span`) split and intern via byte offsets. Lean extraction currently refuses bulk sequence ops, `swap`/`replace`, and string operations.

| Package | Ceiling / Ffi | Implemented Modules and Deterministic Semantics (packages/vesper-std/) |
| --- | --- | --- |
| @vesper-std/core | Fail, Alloc | min, max, clamp, swap, ordering; option & result combinators; saturating_*_k |
| @vesper-std/collections | Fail, Alloc | 16-key flat-arena CacheBTree (no delete merge yet), dense insertion-order |
                                                     FlatTable (non-copy vals via position/try_get), OrderedSet/FlatSet, 32-way
                                                     RadixSeq with O(1) freeze SeqSnapshot and path-copying thaw (#847 traces)
    @vesper-std/fmt           Fail, Alloc            ChunkSeq[u8] & fmt::text String/Str integer format/parse (ParseIntError), fnv1a64
    @vesper-std/bytes         Fail, Alloc            Endian read_*/push_*/put_* helpers; views offset splitters; ct::all_zero & ct_eq
    @vesper-std/sort          Fail, Alloc            O(n log n) unstable::sort introsort (0 alloc), stable::stable_sort, keyed::
                                                     sort_by_key & sort_pairs, floats::sort_f64 (TotalOrderF64) & sort_f32_bits
    @vesper-std/fs            Ffi[StdFs], Fail       read/write/atomic replace, metadata, deterministic byte-sorted walk::Entries,
                                                     IoError kind codes, private linear extern file/dir handles (vesper-std-host)
    @vesper-std/process       Ffi[StdProcess], Fail  Linear Command/Child handles (drop kills & reaps), Result-returning handle ops
    @vesper-std/env           Ffi[StdEnv], Fail      Byte-slice CLI args, env vars, and cwd via length-checked copy-out protocol
    @vesper-std/cpu           Ffi[StdCpu]            Backend-emitted leaf machine code: x86_cpuid, x86_rdtsc, x86_rdpmc, x86_rdrand64;
                                                     aarch64_cntvct, aarch64_pmccntr, aarch64_pmevcntr, aarch64_rndr (0 external link)
    @vesper-std/arena         Fail, Alloc            Typed u32 Idx[T] (idx_none = u32::MAX), Arena[T], IdxMap[K, V] / idx_map_owning
    @vesper-std/graph         Fail, Alloc            Deterministic GraphBuilder; iterative Tarjan scc, Kahn topo + cycle, CHK dom, reach
    @vesper-std/intern        Fail, Alloc            O(1) first-insertion Symbol table, lookup_view/lookup_span, resolve_into, span
    @vesper-std/hash          Fail, Alloc            Portable blake3 (hash, keyed, KDF, XOF Digest) + fast fnv1a64/32, xxh64, mix64
                                                     (paired with @vesper-std/hash-sys linear extern Hasher over Ffi[Blake3])

## 13.6 Follow a region operation into the runtime

The allocator supplies the storage behind region operations. Check where a cursor advances, which alignment a request needs, and when pages can be reused. A fast cursor update does not establish escape safety. The allocator checks that the request fits. The language check determines whether any surviving value could still refer to the reclaimed storage.

<!-- technical-detail -->

In the target native ABI (`content.typ` §13.6), the active region's 64-byte-aligned `RegionEvidenceState` is addressed via `EvidenceSlot.state_ptr` in Cranelift's pinned `vmctx` register (`r15` on `x86_64`, `x28` on `AArch64`), turning aligned bump allocation into checked pointer arithmetic and watermark rewind into a LIFO walk of `FinalizerNode` entries down to `watermark_cursor` once `active_linear_count == 0`. Alongside it, `SizeClassSlab::alloc_local` pops core-local free slots with zero atomics and steals `remote_free_stack` batches only when the local list empties (`crates/vesper-alloc` also ships a port-backed mode `IMPLEMENTATION = "vesper"` linked against `"rust"` seed backing).

    // crates/vesper-alloc/src/lib.rs — target RegionEvidenceState and SizeClassSlab layouts
    #[repr(C, align(64))]
    pub struct RegionEvidenceState {
        pub bump_cursor: usize, pub bump_limit: usize,
        pub virtual_base: NonNull<u8>, pub reserved_bytes: usize,
        pub affine_finalizer_head: *mut FinalizerNode, pub active_linear_count: u32,
    }
    impl RegionEvidenceState {
        pub fn try_bump_aligned(&mut self, size: usize, align: usize) -> Option<NonNull<u8>> {
            let aligned = (self.bump_cursor.checked_add(align - 1)?) & !(align - 1);
            let next = aligned.checked_add(size)?;
            if next > self.bump_limit { return None; }
            self.bump_cursor = next;
            NonNull::new(aligned as *mut u8)
        }
        pub unsafe fn rewind_to(&mut self, watermark_cursor: usize) {
            debug_assert_eq!(self.active_linear_count, 0);
            while !self.affine_finalizer_head.is_null()
                && (self.affine_finalizer_head as usize) >= watermark_cursor
            {
                let node = &*self.affine_finalizer_head;
                self.affine_finalizer_head = node.prev;
                (node.drop_fn)(node.payload_ptr);
            }
            self.bump_cursor = watermark_cursor;
        }
    }
    #[repr(C, align(64))]
    pub struct SizeClassSlab {
        pub local_free_head: *mut FreeSlot,
        pub remote_free_stack: AtomicPtr<FreeSlot>,
        pub slot_bytes: u16, pub owner_core_id: u16,
    }

# 14 Vector instructions and portable SIMD

Single instruction, multiple data (SIMD) applies one operation to several lanes. A portable vector loop must choose an instruction set available on the machine and handle a final group shorter than a full vector.

<!-- technical-detail -->

Single-instruction, multiple-data (SIMD) execution processes several data lanes in one register operation, provided the host processor supports the required instruction set and tail elements never read or write past buffer bounds. Vesper Vector turns instruction-set availability into a zero-cost lexical capability and pairs fixed-width and scalable vectors with total masked memory operations.

Fixed-width `Vec[T, N]`, `Mask[T, N]`, and `eff [Isa[P]]` capability checking with `Isa::supported[P]()` are implemented in `src/vector.rs` across the reference evaluator, Cranelift JIT, and native object emitter, while `crates/vesper-simd` provides a standalone `Baseline128` byte scanner plus `vvec` (`VVec`, `VMask`) strip-mined prototypes for portable scalar, Arm SVE, and RISC-V RVV 1.0 targets. Per `REVIEW-STATUS.md` (`COMP-01`..`COMP-05`, `SYS-01`..`SYS-06`), source-level scalable `VVec[P, T]`, automatic `dispatch_simd` multiversioning, `egglog` vector equality saturation, and Lean 4 `bv_decide` extraction remain standalone prototypes or design specifications rather than machine-checked compiler passes.

## 14.1 Keep inactive lanes out of the computation

A lane mask identifies the elements that participate in an operation. For a ten-element input and a width of four, only two lanes are active in the third group. Masked memory operations must also honor the mask when accessing memory. Ignoring the unused results would come too late for an out-of-bounds load.

<!-- technical-detail -->

Most systems languages mark vendor SIMD intrinsics `unsafe` for two distinct reasons: executing an instruction absent on the host CPU raises `SIGILL`, and an unpredicated wide load or store at the end of a slice crosses into unmapped pages even if the caller ignores the upper lanes afterward. Vesper separates and eliminates both hazards in safe code. First, CPU feature availability is tracked statically via `eff [Isa[P]]`, making pure lane arithmetic safe and total without `unsafe` blocks. Second, tail loads and stores take explicit lane masks (`Mask::active[T, N]` or `VMask[P, T]`) that suppress inactive memory accesses before touching memory.

At call boundaries, `[T-CallProj]` statically guarantees that `inout dst` and `in src` slice projections never alias. Vector kernels and Cranelift lowering can therefore emit wide register reads and writes directly without synthesizing runtime pointer-overlap checks or scalar alias-bailout loops, while higher-order combinators carrying `eff [Isa[P] | r]` monomorphize inline without indirect function-pointer calls.

## 14.2 Require an instruction-set profile

An `Isa[P]` capability records which instruction-set architecture profile a block may use. Dispatch selects a suitable implementation after establishing hardware support. Keep a fallback for a machine with a narrower profile. For example, select a narrower implementation when the CPU lacks an instruction required by the wider one.

<!-- technical-detail -->

Hardware instruction-set availability is represented in `Sigma` by a stateless, erased `copy` capability `Isa[P]` parameterized over five `IsaProfileKind` targets: `Baseline128` (`x86_64` SSE4.2 or `AArch64` NEON), `Avx2` (256-bit `x86_64` AVX2 + BMI2 + FMA), `Avx512` (512-bit `x86_64` AVX-512F/BW/VL + `k`-masks), `Sve2` (scalable 128..2048-bit `AArch64` SVE2 + `whilelo`), and `Rvv1` (scalable RISC-V Vector v1.0 + `vsetvli`). Profiles form the capability lattice `Baseline128 <= Avx2 <= Avx512`, `Baseline128 <= Sve2`, and `Baseline128 <= Rvv1`, so any helper requiring `Isa[Q]` is callable inside a scope holding `Isa[P]` whenever `Q <= P` (128-bit shapes accept any profile, 256-bit shapes require `Avx2` or `Avx512`, and 512-bit shapes require `Avx512`).

In the shipped compiler (`src/vector.rs`), user handlers cannot fabricate `Isa[P]`; plain code discharges `Isa[P]` exclusively through `if Isa::supported[P]() { ... } else { ... }`, which folds against the active Cranelift ISA flags so the evaluator, JIT, and object backend take identical branches. The manuscript (§14.2) also proposes a higher-order multiversion `dispatch_simd` entry point that probes `CPUID` / `HWCAP` once and dispatches to specialized clones compiled per profile `P`:

~~~vesper
// Implemented checked dispatch in src/vector.rs (no handler can forge Isa[P]):
fn sum_u32_auto(in xs: Slice[u32]) -> u32 {
    if Isa::supported[Avx2]() {
        sum_u32_avx2(in xs)
    } else if Isa::supported[Baseline128]() {
        sum_u32_128(in xs)
    } else {
        0
    }
}
// Proposed multiversion dispatcher (§14.2):
// copy effect Isa[P] {}
// fn dispatch_simd[R](kernel: [P: IsaProfile] fn(in cap: IsaCap[P]) -> R eff [Isa[P]]) -> R;
~~~

## 14.3 Choose fixed or scalable vectors

`Vec[T, N]` has a fixed lane count. A scalable `VVec` loop processes a runtime-sized strip each time, with a predicate for the remaining elements. Structure the computation around active elements so the answer stays the same when a target chooses a different vector length.

<!-- technical-detail -->

Vesper Vector distinguishes implemented fixed-width vectors (`Vec[T, N]` and `Mask[T, N]` in `src/vector.rs`) from proposed length-agnostic strip-mined vectors (`VVec[P, T]` / `ScalableVec` and `VMask[P, T]`). In `src/vector.rs`, `Vec[T, N]` and `Mask[T, N]` are `copy` types over the six scalar lane kinds `u8`, `u32`, `u64`, `i64`, `f32`, and `f64` at total widths of `128`, `256`, or `512` bits (`i8`, `u16`, `i16`, `i32`, and `swizzle` remain manuscript extensions). Integer lane math wraps modulo lane width; floating-point lanes follow IEEE binary32/binary64 (`add`, `sub`, `mul`, `div`, `min`, `max`, `sqrt`, `abs`, `neg`) plus `Vec::mul_add(a, b, c)`, which computes `a * b + c` with a single rounding on every backend (hardware FMA where available, C `fmaf`/`fma` otherwise), alongside lane-count-preserving `to_bits`, `from_bits`, exact `widen_f64`, and round-to-nearest-even `nearest_f32`. Unmasked `Vec::load[T, N](in s, at)` and `Vec::store(inout s, at, v)` check `at + N <= s.len()` once and raise `Fail[RuntimeError]` (`IndexOutOfBounds`) before writing; `Vec::load_masked[T, N](in s, at)` and `Vec::store_masked(inout s, at, v)` are total, using `Mask::active[T, N](at, s.len())` so inactive tail lanes read zero and leave memory untouched:

    Input Slice (10 x f32 elements):  [ e0  e1  e2  e3 | e4  e5  e6  e7 | e8  e9  --  -- ]
    Fixed Vec[f32, 4] (128-bit):
      iter 0 (at=0): Mask [1, 1, 1, 1] ──▶ load_masked [e0, e1, e2, e3]
      iter 1 (at=4): Mask [1, 1, 1, 1] ──▶ load_masked [e4, e5, e6, e7]
      iter 2 (at=8): Mask [1, 1, 0, 0] ──▶ load_masked [e8, e9, 0.0, 0.0] (no fault past e9)
    Scalable VVec[P, f32] / ScalableVec (VL = 4 * vscale lanes, e.g. 8 lanes at 256-bit SVE2/RVV1):
      step 0 (idx=0): P::active_mask(0,10) = [1,1,1,1,1,1,1,1] ──▶ step +8 (whilelo / vsetvli)
      step 1 (idx=8): P::active_mask(8,10) = [1,1,0,0,0,0,0,0] ──▶ step +2 (0 scalar tail loop)

~~~vesper
fn saxpy_masked(a: f32, in x: Slice[f32], inout y: Slice[f32]) eff [Isa[Baseline128]] {
    let av: Vec[f32, 4] = Vec::splat[f32, 4](a);
    let mut at: u64 = 0;
    let n: u64 = y.len();
    while at < n {
        let m: Mask[f32, 4] = Mask::active[f32, 4](at, x.len());
        let xv: Vec[f32, 4] = Vec::load_masked[f32, 4](in x, at);
        let yv: Vec[f32, 4] = Vec::load_masked[f32, 4](in y, at);
        let out: Vec[f32, 4] = Vec::select(m, Vec::mul_add(av, xv, yv), yv);
        Vec::store_masked[f32, 4](inout y, at, out);
        at = wrapping_add(at, 4);
    }
}
~~~

For scalable ISAs (`Sve2`, `Rvv1`, and `Avx512` opmasks), §14.3 proposes `VVec[P, T]` and `VMask[P, T]`, where `P::active_mask[T](idx, len)` computes hardware predicates (`whilelo` on SVE2, `vsetvli` on RVV 1.0, `k1` masks on AVX-512) to eliminate separate scalar tail loops. In `crates/vesper-simd`, Cranelift JIT and ELF object emission currently implement `Baseline128` only (`16`-byte SIMD chunks with a checked scalar remainder), while `crates/vesper-simd::vvec` prototypes `VVec`/`VMask` as standalone Rust modules with scalar, SVE, and RVV 1.0 backends:

~~~vesper
fn scan_record_delimiters[P: IsaProfile](
    in data: ByteSlice,
    delim: u8,
) -> (u64, u8) eff [Isa[P]] {
    let needle = VVec::splat[P, u8](delim);
    let mut matches_count: u64 = 0;
    let mut xor_acc = VVec::splat[P, u8](0);
    let mut idx: usize = 0;
    let len = data.len();
    while idx != len {
        let mask: VMask[P, u8] = P::active_mask[u8](idx, len);
        let chunk = VVec::load_masked(in data, idx, mask, 0);
        let eq_mask = chunk.simd_eq(needle).and(mask);
        matches_count = matches_count.wrapping_add(eq_mask.count_ones() as u64);
        xor_acc = xor_acc.bit_xor(chunk);
        idx = idx.wrapping_add(P::lanes[u8](mask));
    }
    (matches_count, xor_acc.reduce_xor())
}
~~~

## 14.4 Validate a vector rewrite

An optimizer can explore several equivalent-looking implementations and choose a cheaper one. Equality saturation retains candidate expressions together. A bit-vector proof can check a proposed rewrite at a specified width. Overflow, signedness, masks, and floating-point semantics determine which equalities actually hold.

<!-- technical-detail -->

In the proposed Stage 4 optimizer (`vesper-opt-egglog`), SIMD equality saturation explores equivalent vector forms concurrently: fusing `add(mul(a, b), c)` into `fma(a, b, c)` where rounding contracts permit, recognizing widen-multiply-accumulate dot products (`u8 -> u16 -> u32` lowered to `vpdpbusd` on `x86_64` and `udot` on `AArch64`), and collapsing adjacent `swizzle` shuffles into a single permutation. Profile-aware cost extraction scales elementwise costs by `P::register_bits`, penalizes cross-lane shuffles, and hands Cranelift vector IR to instruction-selection rules verified in upstream Cranelift with VeriISLE.

To check branchless SIMD kernels against subtle lane-shift, sign-extension, or mask bugs, §14.4 proposes translating `Vec[T, N]` to `BitVec (width(T) * N)` (and instantiating `VVec` across `vscale` multipliers `1, 2, 4, 8, 16`) so Lean 4's built-in SAT bit-blaster `bv_decide` proves bit-for-bit equivalence against a scalar specification (as shown in the illustrative theorem sketch below; per `REVIEW-STATUS.md`, `illustrative/SimdDelimiterProof.lean` is not yet an automated `vesper verify` pass):

    -- illustrative/SimdDelimiterProof.lean
    -- Proves via Lean 4 SAT bit-blasting (bv_decide) that 16-lane u8 delimiter counting
    -- matches the scalar reference across all 2^136 (chunk, delim) bit patterns:
    theorem simd_chunk_delim_eq_scalar (chunk : BitVec 128) (delim : UInt8) :
        Simd128.countMatchesU8 chunk delim = ScalarSpec.countMatches16 chunk delim := by
      bv_decide

## 14.5 Lower an operation for a target

The lowering engine selects instructions or a fallback according to the active profile. Use the same scalar input as a reference, including an empty input and a partial tail. Agreement on those examples provides a concrete check against accidentally processing an inactive lane.

<!-- technical-detail -->

When emitting native code, the compiler maps active `IsaProfileKind` profiles and explicit CLI feature sets onto Cranelift ISA flags without silently leaking host CPU presets into cross-compiled artifacts (`crates/vesper-simd`'s `CodeMetadata` hashes exact sorted `codegen_settings` and `enabled_isa_flags` into object cache keys). In the CLI, `vesper build` and `vesper emit --emit object|clif` pair `--target <triple>` with `--target-features <list>` (comma-separated Rust feature names such as `ssse3,avx2,fma`), while `vesper simd` runs the `Baseline128` native byte scanner directly and checks parity against scalar inputs including empty slices and sub-vector tails.

    // crates/vesper-simd — IsaProfileKind flag table & CLI target-feature invocation
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum IsaProfileKind { Baseline128, Avx2, Avx512, Sve2, Rvv1 }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum VectorWidth { FixedBits(u16), Scalable { min_bits: u16, max_bits: u16 } }
    impl IsaProfileKind {
        pub fn cranelift_isa_flags(self) -> &'static [(&'static str, &'static str)] {
            match self {
                Self::Baseline128 => &[("enable_simd", "true")],
                Self::Avx2        => &[("enable_simd", "true"), ("has_avx2", "true"), ("has_fma", "true")],
                Self::Avx512      => &[("enable_simd", "true"), ("has_avx512f", "true"),
                                       ("has_avx512bw", "true"), ("has_avx512vl", "true")],
                Self::Sve2        => &[("enable_simd", "true"), ("use_sve2", "true")],
                Self::Rvv1        => &[("enable_simd", "true"), ("has_v", "true")],
            }
        }
    }
    ----
    $ vesper build --target x86_64-unknown-linux-gnu --target-features ssse3,avx2,fma kernel.ves -o kernel
    $ vesper emit --emit clif --target aarch64-unknown-linux-gnu kernel.ves

# 15 Pulse fibers, completion, and cancellation

While a fiber waits for I/O, another fiber can run on the same thread. The runtime must keep the waiting fiber’s stack and any buffer still in use by the device alive until it can safely reclaim them.

<!-- technical-detail -->

While one fiber waits for network or storage completion, a worker thread can run other fibers so long as every suspended stack and kernel-touched DMA buffer stays live through its terminal completion event. Vesper Pulse combines stackful one-shot continuations, structured fiber nurseries, explicit cancellation checkpoints, shared atomics, and deterministic simulation under a single lexical capability model.

Source-level `Atomic[T]`, `MemoryOrder`, and `parallel_for` are implemented in the compiler (`check`, `run`, `jit`, and `build`), `crates/vesper-pulse` (`#![deny(unsafe_code)]`) implements portable owned-frame completion scheduling and work-stealing `RunningPool` executors over Linux `io_uring`/`epoll`, macOS `kqueue`, and Windows IOCP (`vesper_pulse::select::open`), and `crates/vesper-fiber-abi` prototypes native `x86_64`/`AArch64` stack switching, evidence vectors, `FiberImage` snapshots, and DWARF unwinding. Per `REVIEW-STATUS.md` (`SYS-01`, `TYPE-06`, `TYPE-07`), compiler lowering of general `eff [...]` functions onto native fiber stacks, kernel DMA cancellation quarantine, and full `SimPulse` DPOR fault injection remain active integration milestones.

## 15.1 Separate submission from completion

Submitting I/O creates outstanding work. A later completion tells the runtime what happened. Pulse’s design schedules lightweight fibers around those boundaries. A completion token must retain enough identity to wake the right fiber and return the right storage to its owner.

    Submit read       → device may write the buffer
    Request cancel    → original read may still be in flight
    Terminal read CQE → runtime may retire the read and recover its buffer

CQE means completion queue entry. If the event you just received belongs to the cancellation request, look up the original read before returning its storage to the pool.

<!-- technical-detail -->

Completion-based kernel rings (`io_uring`, IOCP) decouple submitting an operation from reaping its terminal completion queue entry (CQE): even after a cancellation request finishes, the device may still write to the submitted buffer until the original request posts its own terminal CQE. Pulse designs fiber scheduling, borrowing, and observability around those completion boundaries rather than poll-coroutine `Future::poll` state machines.

    Dimension                  Poll-Coroutine Baseline                         Vesper Pulse (crates/vesper-pulse & Design)
    -------------------------  ----------------------------------------------  ----------------------------------------------------
    Function Coloring          Viral async fn state machines + Pin<&mut Self>  Colorless functions with eff [...] on 64 KiB stacks
    Cancel Safety              Dropping a select! future aborts at any .await  [T-Let] rejects Cancel across live linear values (E0402)
    Completion DMA (io_uring)  Synchronous Drop forces owned Vec<u8> buffers   Linear continuations keep in/inout slices live to CQE
    Structured Concurrency     Detached 'static + Send tasks + Arc<Mutex<T>>   Pulse::scope borrows disjoint inout parent field trees
    Work-Stealing vs Shards    Split multi-thread vs thread-per-core ecosystems Unified portable spawn vs pinned shard spawn_local
    Observability & Sim        #[instrument] rebuilds + separate mock crates   Lexical handlers drive vesper top & seeded SimPulse

## 15.2 Identify cancellation points

Network, storage, clocks, scopes, and cancellation appear as lexical capabilities. A suspension can promise to resume a continuation. An explicit cancellation point can abandon it. Before adding a cancellation point, identify who will consume each linear resource if the task stops there.

<!-- technical-detail -->

Pulse separates concurrency and I/O into lexical capabilities distinguished by their handler resumption multiplicity `q` in `{affine, linear}`. Cooperative scheduling (`Async`), monotonic timers (`Clock`), network/storage rings (`Net`, `Storage`), and structured nurseries (`Scope`) are declared `linear effect` (`q = linear`, `res(eps) = linear`), obligating the handler to resume the captured fiber continuation exactly once. Cancellation (`Cancel`) is declared as a plain `effect` (`q = affine`, `res(Cancel) = affine`), permitting a handler to abandon the continuation at explicit checkpoints:

~~~vesper
linear effect Async { fn yield_now() -> Unit; }
effect Cancel { fn check() -> Unit; }
linear effect Clock {
    fn now_ns() -> u64;
    fn sleep_ns(duration_ns: u64) -> Unit;
}
linear effect Net {
    fn accept(in listener: TcpListener) -> Result[TcpStream, IoError];
    fn read_some(inout stream: TcpStream, inout buf: ByteSlice) -> Result[u32, IoError];
    fn write_all(inout stream: TcpStream, in buf: ByteSlice) -> Result[Unit, IoError];
}
linear effect Scope {
    fn spawn_scoped[T: mod copy](f: fn@linear() -> T eff [Async, Cancel]) -> JoinHandle[T];
}
~~~

Under `[T-Let]` (`forall z in Gamma_2, m_z <= res(eps_1)`), a fiber holding an unconsumed `linear` protocol value such as `WriteTxn` or `FrameWriter` may suspend freely across `Storage::flush_wal` or `Net::write_all` because `linear <= linear` holds. Invoking `Cancel::check()`, `Pulse::timeout`, or `Pulse::race` while `WriteTxn` is still live fails `linear <= affine` and is rejected at compile time (`E0402` / `E0402`), preventing half-mutated protocol state from being silently abandoned.

## 15.3 Keep device buffers live through the terminal completion

Pipelining lets a fiber submit several requests before awaiting their results. With direct memory access (DMA), the device can still touch a submitted buffer. The cancellation request can finish before the original I/O request. Keep the buffer live until the original request’s terminal completion. A multishot operation can report several completions before its final one.

<!-- technical-detail -->

When a single fiber pipelines multiple concurrent `io_uring` submission queue entries (SQEs) before awaiting results, §15.3 models each in-flight submission as a `linear struct InFlightRead[B]` whose ownership can only be discharged by awaiting normal completion (`await_cqe`) or submitting `IORING_OP_ASYNC_CANCEL` and waiting until the kernel posts the original operation's terminal CQE (`cancel_and_reap`):

    Parent Fiber Stack (64 KiB fixed VA, Zero-Pin)          r15 / x28 vmctx ──▶ EvidenceVector
    ┌────────────────────────────────────────────────────┐  ┌────────────────────────────────┐
    │ broker.{index} │ broker.{repl_stream} │ WriteTxn   │  │ [Scope] [Storage] [Net] [...]  │
    └───────┬────────┴──────────┬───────────┴─────┬──────┘  └────────────────────────────────┘
            │                   │ (inout)         │ (sink tx)
            │      Pulse::scope ├─────────────────┼──────────────────────┐
            │                   ▼                 ▼                      │ (.join() barrier)
            │          Child Fiber 2       Child Fiber 1                 │
            │          Net::write_all      tx.commit()                   │
            ▼                   │                 │                      ▼
    io_uring DMA Timeline:      └──▶ SQE submit ──┴──▶ Cancel requested? ──▶ Reap TERMINAL CQE
      [Buffer/Stack LIVE] ───────────────────────────▶ [Still DMA-LIVE!] ──▶ [Safe to reclaim]
      (SYS-01: cancel CQE != DMA stop; InFlightRead[B] valid only for owned B, not stack loans)

~~~vesper
linear struct InFlightRead[B] {
    ring_id: u32,
    sqe_user_data: u64,
    pinned_buf: B,
}
impl[B] InFlightRead[B] {
    fn await_cqe(sink self) -> (Result[u32, IoError], B) eff [Storage] {
        Storage::reap_cqe(self.ring_id, self.sqe_user_data, sink self.pinned_buf)
    }
    fn cancel_and_reap(sink self) -> B eff [Storage] {
        Storage::cancel_and_reap_terminal(self.ring_id, self.sqe_user_data, sink self.pinned_buf)
    }
}
~~~

Mandatory `SYS-01` review limitation (`content.typ` line 3882 and `REVIEW-STATUS.md`): this interface is a proposed contract rather than a proven cancellation-safety guarantee, and `InFlightRead[B]` is sound only when `B` is an owned DMA buffer—never a stored second-class stack borrow. Because `io_uring` cancellation is asynchronous, the CQE acknowledging `IORING_OP_ASYNC_CANCEL` does not prove that device DMA into the original buffer has ceased; a cancelled fiber and its fixed-address stack must remain quarantined until the original operation's terminal CQE (including the final event of any multishot request) has been reaped, and stack buffers lent by an independently cancelled parent require owned DMA storage or equivalent quarantine protection. IOCP and other host completion drivers require separate platform-specific lifetime proofs.

## 15.4 Join children and share state deliberately

A nursery bounds the lifetime of child fibers. A join handle represents the obligation to collect a child. Siblings can receive disjoint writable projections or communicate through a linear channel. Portable work can move between workers, while a pinned shard stays attached to its thread.

### Source atomics {#ch-14-4-atomics}

`Atomic[T]` provides operations with explicit memory orders. For example, a relaxed load observes the atomic cell without synchronizing access to other storage. The checker rejects invalid order combinations. [Read the source atomic rules](reference.html#ch-14-4-atomics).

### Scoped host workers {#ch-14-4-workers}

A `parallel_for` body gets shared data and its iteration index. Use an atomic cell for a shared reduction. The call joins the workers before returning, so a temporary shared loan stays inside the scope. [Read the worker rules](reference.html#ch-14-4-workers).

An `Atomic[T]` has one owner and can be shared through reads of the cell. Atomic loads, stores, swaps, and compare-exchange specify a memory order. The order determines what other memory accesses become visible between workers. `parallel_for` joins scoped workers before the data loan ends. The worker body and available execution backend constrain which programs can use this operation.

<!-- technical-detail -->

In the proposed structured concurrency model (`Pulse::scope`), `s.spawn` schedules `portable` fibers on work-stealing deques while `s.spawn_local` pins `!portable` fibers to the local shard's `io_uring` ring. Each spawned child returns a `linear` `JoinHandle[T]` that must be consumed by `.join()` before scope exit, allowing `[T-CallProj]` to lend disjoint `inout` parent field projections across sibling fibers without `'static` bounds or `Arc<Mutex<T>>`, while `Pulse::linear_oneshot[T]()` pairs `LinearTx[T]` (`linear`) with `LinearRx[T]` so `rx.recv()` never observes an abandoned sender:

~~~vesper
fn replicate_and_commit(
    inout broker.{index, metrics, repl_stream}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Scope, Storage, Net, Clock, Cancel, Fail[BrokerError]] {
    Cancel::check()?;
    let wire_slice = batch.as_wire_slice();
    let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);
    let (wal_res, repl_res) = Pulse::scope(|inout s| eff [Scope, Storage, Net, Clock] {
        let wal_join: JoinHandle[Result[CommitReceipt, IoError]] = s.spawn(|| eff [Storage] {
            handle Fail[IoError] in { Ok(tx.commit()) } with { return r => r, raise(e) => Err(e) }
        });
        let net_join: JoinHandle[Result[Unit, IoError]] = s.spawn(|| eff [Net, Clock] {
            Net::write_all(inout broker.repl_stream, in wire_slice)
        });
        (wal_join.join(), net_join.join())
    });
    let receipt = wal_res.map_err(BrokerError::Wal)?;
    let _unit = repl_res.map_err(BrokerError::Replication)?;
    broker.index.insert_in_place(sink batch);
    broker.metrics.batches_written += 1;
    receipt
}
~~~

For shared-memory state in the shipped compiler (`content.typ` lines 3956–4007), `Atomic[T]` is the sole source type mutable through a shared `in` borrow. `T` must be `bool` or an integer scalar (`u8`, `u16`, `u32`, `u64`, `usize`, `i8`, `i16`, `i32`, `i64`; any other type is rejected with `E0821`). `Atomic::new(v)` allocates an `affine` single-owner cell, `into_inner(sink self)` consumes it, and its stored value is private to atomic intrinsics (`E0822` rejects field or place access). Every operation takes the cell by `in`, is total with an empty effect row, acts as an opaque barrier against `egglog` load forwarding, and requires a literal `MemoryOrder` (`Relaxed`, `Acquire`, `Release`, `AcqRel`, `SeqCst`): `load` and the failure order of `compare_exchange(current, new, success, failure) -> Result[T, T]` reject `Release` and `AcqRel`, while `store` rejects `Acquire` and `AcqRel` (`E0820`). `swap`, `fetch_and`, `fetch_or`, and `fetch_xor` work on integers and `bool`; wrapping `fetch_add` and `fetch_sub` apply to integers only. Native Cranelift codegen lowers every atomic operation to a width-matched sequentially consistent instruction, while `--codegen verified` rejects atomics as `Unsupported`.

~~~vesper
fn record(in hits: Atomic[u64], in done: Atomic[bool], n: u64) -> u64 {
    for i in 0..n {
        hits.fetch_add(1, MemoryOrder::Relaxed);
    }
    done.store(true, MemoryOrder::Release);
    hits.load(MemoryOrder::Acquire)
}
~~~

Scoped data-parallel fork-join execution is provided in the shipped compiler by `parallel_for[A](in data: A, n: u64, body: fn @copy(in A, u64))`, which runs `body(in data, i)` across `0..n` and joins all workers before the `in data` loan ends. Passing `data` or `body`'s first parameter as `inout` or `sink` is rejected (`E0824`), so workers coordinate mutations solely through `Atomic[T]` fields inside `data`; `body` must be a named, non-generic top-level function with an empty effect row (`E0823`), and nested `parallel_for` calls execute inline on the calling worker so pools never deadlock. Backends choose compatible schedules: `vesper run` and AOT executables (`vesper build`) run `0..n` sequentially, `vesper jit` dispatches up to four chunks per worker across a process-wide Pulse pool (running the final chunk on the calling thread, or staying entirely on the calling thread if the body touches thread-local `TiledSeq`, `Box`, or region state), and `--codegen verified` refuses bodies that touch atomics:

~~~vesper
struct Job { xs: ChunkSeq[u64], chunk: u64, total: Atomic[u64] }
fn sum_chunk(in job: Job, i: u64) {
    let mut s: u64 = 0;
    let lo: u64 = wrapping_mul(i, job.chunk);
    for k in 0..job.chunk {
        match job.xs.try_get(wrapping_add(lo, k)) {
            Option::Some(v) => { s = wrapping_add(s, v); },
            Option::None => {},
        }
    }
    job.total.fetch_add(s, MemoryOrder::Relaxed);
}
fn total(in job: Job) -> u64 {
    parallel_for(in job, 4, sum_chunk);
    job.total.load(MemoryOrder::SeqCst)
}
~~~

## 15.5 Suspend with a stack and handler evidence

A fiber’s saved machine state records where execution can resume. An evidence vector locates the current lexical handlers. A synchronously completed, tail-resumed operation may avoid a stack switch. If the request is still pending, the runtime must keep its continuation and stack live until completion.

<!-- technical-detail -->

In the native continuation design (§15.5, prototyped in `crates/vesper-fiber-abi` v2), every effectful function receives a pinned Cranelift `vmctx` register (`r15` on `x86_64`, `x28` on `AArch64`) pointing to a cache-line-aligned `EvidenceVector` of `(handler_fn, state_ptr, scheduler_sp, scheduler_ip)` slots indexed by static capability depth in `Sigma` (`O(1)` lookup). Synchronous tail-resumptive operations (`Clock::now_ns()`, uncancelled `Cancel::check()`) fuse inline via `egglog` at `0 ns` with zero stack switches.

When an `io_uring` SQE or channel suspends, the fiber runs on a pooled 64 KiB virtual stack slab (lazy 4 KiB physical pages plus hardware guard page) and calls `vesper_fiber_yield`, spilling callee-saved registers (`rbx, rbp, r12..r15, rsp, rip` on `x86_64`; `x19..x29, lr, sp, d8..d15` on `AArch64` plus `fpenv` state—80 to 144 bytes) into an intrusive `FiberHeader`, storing `FiberHeader*` in `sqe.user_data`, and pivoting `rsp`/`sp` back to the shard loop in roughly 4–6 ns. Suspended stacks never move in virtual memory (`Zero-Pin` stack invariant) and cannot return to the slab pool until the fiber exits and all kernel operations referencing its stack reach terminal completion.

## 15.6 Reproduce a schedule in SimPulse

SimPulse substitutes simulated clocks, completions, and scheduling decisions for live ones. A seed can reproduce the same sequence for a failing scenario. Live telemetry instead observes fibers in a running system. Use the event history to distinguish a fiber waiting for a completion from one waiting to run.

<!-- technical-detail -->

Because `Async`, `Scope`, `Net`, `Storage`, `Clock`, and `Cancel` are lexical effects rather than hardcoded OS syscalls, swapping the outer `handle` layer replaces live kernel rings with `SimPulse` (`vesper test --sim --seed <u64>`) or live production telemetry (`vesper top`, `VESPER_PULSE_TRACE=1`). `SimPulse` pairs a virtual monotonic clock (`Clock::sleep_ns` advances simulated hours in milliseconds whenever runnable fibers park) with PRNG-seeded dynamic partial-order reduction (DPOR) across `Scope::spawn_scoped` and `Async::yield_now`, plus simulated `Net` packet reordering/partitions and `Storage` unfsynced WAL sector drops.

    $ vesper test --sim --seed 0x5eedcafe12340001 broker_replication.ves
    $ VESPER_PULSE_TRACE=1 ./broker &
    $ vesper top --pid $!

Without `#[instrument]` attributes or recompilation, `vesper top` interposes a lock-free `TelemetryPulse` wrapper over `EvidenceVector` to stream scheduling histograms and power the Live Linear Obligation Inspector, which lists every `Mode::Linear` token (`WriteTxn`, `InFlightRead`, `JoinHandle`, `LinearTx`) parked on suspended fiber stacks alongside wait-for cycle detection. At package boundaries, Loom `effects = []` blocks unauthorized fiber/socket use, `vesper semver` flags added `Cancel` or `Net` effects via `[SV-EffRow]`, and `vesper-verify-lean` models concurrency protocols in `EffM`.

## 15.7 Trace the runtime bookkeeping

Follow a request through registration, completion delivery, wakeup, and retirement. The runtime must associate each event with the correct generation of the task. Reusing a stack or slot too early would give a delayed completion access to somebody else’s work.

<!-- technical-detail -->

At runtime, each `PulseWorkerShard` pairs a cache-hot `lifo_slot` with a `VecDeque` run queue (`step_deterministic` selects runnable indices from the seeded PRNG under `SimPulse`). In `crates/vesper-pulse`, `#![deny(unsafe_code)]` `CompletionDriver` backends and `ContinuationTable` slots in `crates/vesper-fiber-abi` pair slot indices with monotonic generations so delayed CQEs never wake a recycled stack.

    // crates/vesper-pulse & crates/vesper-fiber-abi — EvidenceSlot, FiberHeader, PulseWorkerShard
    #[repr(C, align(64))]
    pub struct EvidenceSlot {
        pub handler_fn: unsafe extern "C" fn(*mut u8, u32, *mut u8) -> u64,
        pub state_ptr: *mut u8, pub scheduler_sp: usize, pub scheduler_ip: usize,
    }
    #[repr(C, align(64))]
    pub struct FiberHeader {
        pub saved_sp: usize, pub saved_ip: usize, pub fiber_id: u64,
        pub live_linear_obligations: u32, pub is_portable: bool,
        pub stack_base: NonNull<u8>, pub stack_len: usize,
    }
    pub struct PulseWorkerShard {
        pub core_id: u16, pub lifo_slot: Option<NonNull<FiberHeader>>,
        pub run_queue: VecDeque<NonNull<FiberHeader>>, pub simulated_clock_ns: Option<u64>,
    }
    impl PulseWorkerShard {
        pub fn wake_linear_continuation(&mut self, k_fiber: NonNull<FiberHeader>) {
            if let Some(prev) = self.lifo_slot.replace(k_fiber) { self.run_queue.push_back(prev); }
        }
        pub fn step_deterministic(&mut self, prng_choice: usize) -> Option<NonNull<FiberHeader>> {
            if let Some(hot) = self.lifo_slot.take() { self.run_queue.push_front(hot); }
            if self.run_queue.is_empty() { return None; }
            self.run_queue.remove(prng_choice % self.run_queue.len())
        }
    }

# 16 Bridge wire layouts and foreign calls

Before Vesper can read a foreign value, it needs to know how the bytes are laid out, which bit patterns are valid, and how long the backing storage will live. Bridge describes that contract for device data and calls into other languages.

<!-- technical-detail -->

Before reading device memory or calling C and Rust code, Vesper must account for byte offsets, endianness, valid bit patterns, pointer lifetimes, and handle cleanup. Vesper Bridge expresses those boundary contracts declaratively through padding-free wire records, `.vsys` import manifests, untrusted foreign value wrappers, and versioned library export ABIs.

Declarative `.vsys` package imports, `ForeignVal[T]` validation, `CStr`, `errno`, variadic C calls, zero-copy `CSlice[T]` loans, and `vesper build --lib` export ABIs (`export-abi/1`, `/3`, and `/4`) are implemented in the compiler, while `crates/vesper-ffi` validates `wire struct` layouts and prototypes gated Linux `x86_64` PKU and `AArch64` POE domain helpers (`isolation::hw`). Per `REVIEW-STATUS.md` (`SYS-04`, `TYPE-10`, `COMP-04`), packed wire fields must use bytewise or explicitly unaligned accesses rather than aligned references, hardware MPK/POE keys provide accidental fault containment rather than adversarial native-code sandboxing, and end-to-end binary ABI and FFI authority proofs remain open.

## 16.1 Ask what the foreign boundary guarantees

Check size, alignment, byte order, valid bit patterns, and ownership separately. A foreign integer that encodes a Boolean may contain a value outside the accepted representation. An opaque handle may need exactly one close even if its machine representation fits in one word.

<!-- technical-detail -->

Calling across `extern "C"` in conventional systems languages conflates five independent trust assumptions: implicit struct padding causes undefined behavior on wire casts; invalid C `bool` or enum discriminants corrupt exhaustive matches; raw `*const T` and `*mut T` pointers erase aliasing rules; destructor wrappers silently discard close errors; and `build.rs` scripts grant dependencies unrestricted ambient link authority. Bridge addresses each axis explicitly: `wire struct` forbids implicit padding and non-total fields, Omniglot-inspired `ForeignVal[T]` withholds host bit-validity until checked, `.vsys` `nocapture` parameters map C pointers onto `[T-CallProj]` `in`/`inout` borrows, `linear extern type` enforces exact-once C finalization, and `Loom.toml` confines foreign calls behind `eff [Ffi[Lib]]`.

## 16.2 Read a wire record without copying it

A `wire struct` lays out fields at explicit byte offsets and uses endian-aware scalars. Explicit padding makes every byte accountable. A view must cover enough bytes for the entire record. A packed field may be unaligned, so reading it must use an access appropriate to that layout.

<!-- technical-detail -->

A `wire struct` (`mod copy`) defines a nominal, byte-exact record for network frames, IPC headers, and kernel DMA rings. Every field must inhabit 100% of its bit patterns (`FromBytes` and `AsBytes`): fixed integers, explicit-endian byte-aligned wire scalars (`u16be`, `u32be`, `u64be`, `i64be`, `u16le`, `u32le`, `u64le`), `ByteArray[N]`, explicit `ZeroPad[N]` (zeroed on construction and ignored on equality), or nested `wire struct` types; `bool` and sparse `enum` tags are banned in favor of total `WireFlag8` or `WireTag16` wrappers. If natural alignment would insert any internal or tail gap, compilation rejects the declaration and suggests the exact `ZeroPad[N]`. Per the §16.2 review limitation and `crates/vesper-ffi`, explicit-endian wire integers have 1-byte alignment and must always be read and written via bytewise/unaligned endian loads and stores—never by forming an aligned native reference—allowing a 61-byte `KafkaBatchWireHeader` to project in place via `split_wire_mut`:

    Byte Offset   Width  Field                    Wire Type   Alignment & Bit-Validity Contract
    ┌───────────┬──────┬────────────────────────┬───────────┬──────────────────────────────────┐
    │ [0 .. 8)  │  8 B │ base_offset            │ i64be     │ 1B-aligned big-endian, all bits  │
    │ [8 .. 12) │  4 B │ batch_length           │ i32be     │ 1B-aligned big-endian, all bits  │
    │ [12 .. 16)│  4 B │ partition_leader_epoch │ i32be     │ 1B-aligned big-endian, all bits  │
    │ [16 .. 17)│  1 B │ magic                  │ u8        │ 1B-aligned byte, all bits        │
    │ [17 .. 21)│  4 B │ crc                    │ u32be     │ bytewise unaligned (offset 17!)  │
    │ [21 .. 23)│  2 B │ attributes             │ u16be     │ bytewise unaligned (offset 21!)  │
    │ [23 .. 27)│  4 B │ last_offset_delta      │ i32be     │ bytewise unaligned (offset 23!)  │
    │ [27 .. 35)│  8 B │ base_timestamp         │ i64be     │ bytewise unaligned (offset 27!)  │
    │ [35 .. 43)│  8 B │ max_timestamp          │ i64be     │ bytewise unaligned (offset 35!)  │
    │ [43 .. 51)│  8 B │ producer_id            │ i64be     │ bytewise unaligned (offset 43!)  │
    │ [51 .. 53)│  2 B │ producer_epoch         │ i16be     │ bytewise unaligned (offset 51!)  │
    │ [53 .. 57)│  4 B │ base_sequence          │ i32be     │ bytewise unaligned (offset 53!)  │
    │ [57 .. 61)│  4 B │ records_count          │ i32be     │ Total = 61 B, 0 implicit padding │
    └───────────┴──────┴────────────────────────┴───────────┴──────────────────────────────────┘

~~~vesper
wire struct KafkaBatchWireHeader {
    base_offset: i64be, batch_length: i32be, partition_leader_epoch: i32be,
    magic: u8, crc: u32be, attributes: u16be, last_offset_delta: i32be,
    base_timestamp: i64be, max_timestamp: i64be, producer_id: i64be,
    producer_epoch: i16be, base_sequence: i32be, records_count: i32be,
}
fn stamp_batch_header(
    inout raw_frame: ByteSlice,
    computed_crc: u32,
) -> Result[i64, WireLayoutError] {
    let (inout hdr, in _payload) = raw_frame.split_wire_mut[KafkaBatchWireHeader]()?;
    hdr.crc = computed_crc.to_be();
    Ok(hdr.base_offset.to_native())
}
~~~

## 16.3 Describe an import and validate its result

A package can declare `.vsys` imports in its manifest. A signature can describe a noncaptured string or slice argument, an opaque handle, or a foreign result requiring validation through `ForeignVal[T]`. Lending a slice saves a copy only for the agreed duration. A callee that retains the pointer needs a different ownership contract.

<!-- technical-detail -->

Packages declare foreign C or Rust bindings in `.vsys` files listed under `[package] vsys = ["sys/lz4.vsys"]` in `Loom.toml` (slash-separated relative package paths with no `..`, each named once). Loading a package graph brings every dependency's `.vsys` declarations into scope across `vesper check`, `vesper jit`, `vesper build`, and `vesper build --lib` without consumers repeating `.vsys` text: modules call `LibLz4::f(..)` under `eff [Ffi[LibLz4]]` (confined by `capability_ceiling` in `Loom.toml`, and duplicate library names across packages are rejected), while nominal `.vsys` types keep global names and can also be imported from the declaring package's root module (`use @demo/lz4::{Lz4DecompressCtx}`). Executables link referenced libraries by link name; library roots configure linking via `// @export vsys LibLz4 [direct|isolated(mpk)] [rust=<path>]`; and `vesper jit` / `vesper run` bind only compiler-hosted `vesper_std_host` shims, rejecting arbitrary native libraries prior to execution.

    // sys/lz4.vsys — Declarative C ABI contract verified by Loom and Bridge
    sys library LibLz4 (pkg_config = "liblz4 ^1.9.4", header = "lz4frame.h") {
        linear extern type Lz4DecompressCtx;
        fn LZ4F_createDecompressionContext(version: u32)
            -> ForeignVal[Option[Lz4DecompressCtx]];
        fn LZ4F_decompress(
            inout ctx: Lz4DecompressCtx,
            nocapture inout dst: CSlice[u8],
            nocapture in src: CSlice[u8],
        ) -> ForeignVal[usize];
        fn LZ4F_freeDecompressionContext(sink ctx: Lz4DecompressCtx) -> ForeignVal[usize];
    }

In the implemented `.vsys` contract (`content.typ` lines 4177–4182), `linear extern type H;` and `affine extern type H;` represent pointer-sized opaque handles passed by word as `in h: H`, `inout h: H`, or consuming `sink h: H` (every `linear` extern type must declare at least one `sink` finalizer). Imports return handles via `ForeignVal[Option[H]]` (`NULL` maps to `None`), `ForeignVal[H]` (`NULL` maps to `Err(ForeignInvalid { raw: 0 })`), or `ForeignVal[NonNull[H]]` (unfallible in source; `NULL` traps). A `linear` handle must reach one `sink` on every path: dropping it (`E0302`), consuming on only some branches (`E0309`), double-consuming or using after `sink` (`E0303`), or holding it live across an unhandled abortable effect such as `Fail` (`E0402`) is a compile error, whereas foreign calls themselves return once and may be crossed. An `affine extern type H;` without a drop clause drops silently; `affine extern type H drop f;` binds `fn f(sink h: H) -> ForeignVal[Unit];` and runs `f` once whenever `H` or `Option[H]` (or named struct bindings field-by-field; whole temporary aggregates are rejected) goes out of scope, gets overwritten, or exits a branch/loop—charging `Ffi[Lib]` to the dropping function (`E0401`) and forbidding live crossings over unhandled `Fail` (`E0402`).

| .vsys Feature / Diagnostic | Source Type & Lowering Contract |
| --- | --- |
| E0302 / E0309 / E0303 | Linear extern handle dropped, branch-unbalanced, or used/consumed twice |
| E0401 / E0402 | Affine drop handle missing Ffi[Lib] in row (E0401) or live across Fail (E0402) |
| nocapture in s: CStr | Takes Slice[u8]; copies + appends NUL, frees after call; interior NUL skips C |
                                        and returns Err(ForeignInvalid { raw: first_nul_offset }) in Result[R, ForeignInvalid]
    -> ForeignVal[T] errno              Reads thread errno / Windows GetLastError() immediately; returns (R, u32)
    fn f(fd: i32, ..., arg: i64)        C variadics (>= 1 fixed param, word-sized 64-bit variadic args, no floats);
                                        Apple arm64 zeros unused integer arg regs so variadic args start at sp for va_arg
    nocapture in/inout CSlice[T]        Passes caller (data + start * stride, len) zero-copy for u8, u32, u64, i64, usize

## 16.4 Choose an isolation boundary

A direct call shares the process and relies on the foreign library. Hardware protection keys or permission overlays can limit accidental cross-domain access. A threat model involving hostile native code requires stronger confinement. Decide which party is trusted before choosing the boundary.

<!-- technical-detail -->

Bridge defines two handler modes for `Ffi[Lib]`: `unsafe handle Ffi[Lib] direct` emits a raw System V or AAPCS64 `call` on the active 64 KiB fiber stack (`~0 ns` overhead for VDSO/`liburing` or verified shims), while `handle Ffi[Lib] isolated(mpk)` uses Intel Memory Protection Keys (`WRPKRU` on `x86_64`) or Arm Permission Overlay Extension (`MSR POR_EL0` on `AArch64`) to partition one virtual address space into Domain 0 (Vesper fiber stacks, `EvidenceVector`, private structs), Domain 1 (shared zero-copy I/O arena mapped read-only for `in` and read-write for `inout`), and Domain 2 (foreign library stack, heap, and globals), switching permissions and pivoting `rsp`/`sp` in ~1.5 ns without copying Domain 1 payloads.

    Domain 0: Vesper Host               Domain 1: Shared I/O Arena        Domain 2: Foreign Lib
    ┌──────────────────────────┐        ┌──────────────────────────┐      ┌─────────────────────┐
    │ 64 KiB Fiber Stacks      │        │ DmaSlab / CSlice[u8]     │      │ C Heap, Globals,    │
    │ EvidenceVector (r15/x28) │        │ in  slice -> Key 1 (RO)  │      │ Domain-2 Call Stack │
    │ Private Vesper Structs   │        │ inout dst -> Key 1 (RW)  │      │                     │
    └────────────┬─────────────┘        └────────────▲─────────────┘      └──────────┬──────────┘
                 │ 1. WRPKRU / MSR POR_EL0 (~1.5 ns) │ (0 memcpy)                    │
                 │    Revoke Domain 0; pivot rsp ────┼──────────────────────────────▶│ 2. C call
                 │◀──────────────────────────────────┴───────────────────────────────┘
                 │ 3. Restore Domain 0 key + rsp; validate ForeignVal[T]
                 └──  (SYS-04: fault containment only; hostile native code needs CFI + seccomp)

Mandatory `SYS-04` security limitation (`content.typ` line 4187 and `REVIEW-STATUS.md`): hardware MPK/PKRU and Arm POE permission overlays are accidental fault-containment mechanisms against memory-corruption bugs in benign C libraries, NOT security sandboxes against hostile or adversarial native code. Because `WRPKRU` and `POR_EL0` are unprivileged user-space instructions—and signal delivery frames plus OS system calls (`mprotect`, `mmap`) can alter page rights—confinement against untrusted native code requires binary instruction/CFI verification paired with `seccomp` syscall filtering, or out-of-process / Chapter 17 Nexus WebAssembly isolation.

## 16.5 Validate the layout before creating a view

A layout verifier computes each field offset and inspects gaps and valid bit patterns. To interpret arbitrary bytes as a value without checking their contents, every bit pattern must be valid for that type. To expose a value as bytes, check that every byte you expose is initialized. Checking a format definition once still leaves a length check for each input buffer.

<!-- technical-detail -->

Before projecting a byte slice onto a `wire struct`, `crates/vesper-ffi` statically verifies that every field type is bit-total (`all_bit_patterns_valid`) and that neither internal offsets nor tail alignment introduce implicit padding holes, leaving only a single slice-length (and native-alignment, if any native-endian scalar is used) check at view construction time; exposing a constructed struct as bytes additionally checks `ZeroPad[N]` canonicality.

    // crates/vesper-ffi — static wire struct padding and bit-validity verifier
    pub struct WireFieldSpec {
        pub size_bytes: usize, pub align_bytes: usize, pub all_bit_patterns_valid: bool,
    }
    pub enum WireLayoutError {
        ImplicitPaddingHole { field_idx: usize, offset: usize, required_pad: usize },
        TailPaddingHole { struct_end: usize, required_pad: usize },
        NonTotalFieldType { field_idx: usize },
    }
    pub fn verify_wire_struct_layout(
        name: SmolStr, fields: &[WireFieldSpec],
    ) -> Result<WireStructLayout, WireLayoutError> {
        let (mut offset, mut max_align) = (0usize, 1usize);
        for (idx, f) in fields.iter().enumerate() {
            if !f.all_bit_patterns_valid { return Err(WireLayoutError::NonTotalFieldType { field_idx: idx }); }
            max_align = max_align.max(f.align_bytes);
            let rem = offset % f.align_bytes;
            if rem != 0 {
                return Err(WireLayoutError::ImplicitPaddingHole {
                    field_idx: idx, offset, required_pad: f.align_bytes - rem,
                });
            }
            offset += f.size_bytes;
        }
        if offset % max_align != 0 {
            return Err(WireLayoutError::TailPaddingHole {
                struct_end: offset, required_pad: max_align - (offset % max_align),
            });
        }
        Ok(WireStructLayout {
            name, exact_byte_size: offset, alignment: max_align,
            has_zero_implicit_padding: true, all_fields_from_bytes: true,
        })
    }

## 16.6 Export a function to C or Rust

`vesper build --lib` produces an object for supported public functions. The export rules define an application binary interface (ABI). It specifies symbol names, word layouts, and how a caller receives failures. A caller must use the agreed result storage and failure convention. Consult the current export rules for unsupported parameter modes, generic functions, and effects.

<!-- technical-detail -->

In the reverse direction (`content.typ` lines 4270–4282), `vesper build --lib` compiles a module or package root (including `pub use` re-exports) into a C-ABI object exporting `vesper_<library>_<fn>` symbols alongside optional JSON interface metadata and safe generated Rust bindings used to port compiler crates incrementally (`--lib-name`, `--interface`, `--rust-binding`, `--rust-types`, and repeatable `--instance f[T]` or `// @export instance` for monomorphized generic exports). Three export ABI levels govern marshaling and ownership across the boundary:

    $ vesper build --lib --lib-name vesper_fmt --interface fmt.iface.json \
        --rust-binding port/binding.rs --rust-types crate::types --instance clamp[u64] packages/vesper-std/fmt -o libfmt.o
    ----
    Export ABI Level                Directive & Marshaling / Ownership Contract
    ------------------------------  --------------------------------------------------------------------------------
    vesper.native.export-abi/1      Default 8-byte word layout: scalars by value (f64/f32 in FP regs; f32 in low 32b
                                    of aggregate words), structs in field order, enums as tag + active variant words
                                    (inactive payload words unspecified & ignored); aggregates via scratch/out ptrs;
                                    Fail[E1..En] via trailing status+payload record; no non-Fail effects or closures
    vesper.native.export-abi/3      // @export abi 3 [options]: generates compile-time size/align/offset-checked
                                    #[repr(C)] Rust struct mirrors; in/inout mirrored copy structs & Slice[T] pass
                                    zero-copy caller pointers (abi 3 options adds copy Option/Result mirrors); opaque
                                    host handles pass in by ptr and own ChunkSeq of handles/Strings/Boxes & Box[dyn]
    vesper.native.export-abi/4      // @export abi 4 [options]: adds bidirectional compiler trees: owned <Type>Tree
                                    with single-pass encode/decode (vesper_<lib>_box_new + seq_new), zero-copy lent
                                    <Elem>Seq / <Elem>Ref sequence views, and unsafe { H::lend(&val) } passing Rust
                                    references as in .vsys extern handles to // @export vsys direct rust=<path> shims

Under `export-abi/3`, dropping an opaque handle from Rust drops every owned `ChunkSeq` element and invokes the vtable drop word for any `Box[dyn Trait]` slot while keeping non-copy sequences opaque. Under `export-abi/4`, Vesper walks encoded `<Type>Tree` values via `in` borrowed matching without copying, Rust traverses Vesper handle sequences in place via `<Elem>Seq`/`<Elem>Ref`, and `H::lend(&value)` lets Vesper pass borrowed Rust context pointers solely back into the linking crate's own `.vsys` callbacks without reading, storing, or freeing them.

# 17 Nexus WebAssembly plugins

A plugin that filters messages needs access to a message and a budget for the work. Nexus describes how a host can lend a resource to a guest, meter execution, and recover the resource when the invocation ends.

<!-- technical-detail -->

A multi-tenant broker or query engine running user-defined filters must lend message slices to untrusted plugin code, bound its CPU time, and reclaim every host resource whether the guest returns normally or traps. Vesper Nexus targets the WebAssembly Component Model with stack-switching continuations, substructural WIT resource handles, zero-copy shared ring descriptors, and deterministic instruction-fuel accounting.

In the repository, `crates/vesper-nexus` (`#![forbid(unsafe_code)]`) implements the `O(1)` generational `WitResourceTable` (`table`), a Wasmtime component host (`host`), `FuelAccountant` and `QuantumScheduler` preemption (`fuel`, `pulse` on `SimPulse`), `SharedPluginRing` (`ring`), isolated per-tenant `TenantRings` (`tenant_ring`), `.wit` trait/binding generation (`wit_gen`), and a gated WasmFX stack-switching probe (`wasmfx`). Per `REVIEW-STATUS.md` (`SYS-06`, `REPO-02`), linear handle tracking in `vesper-nexus` is currently enforced by Rust host-library discipline (`#[must_use]` non-`Clone` `WitOwn` and `WitBorrow` wrappers audited by `LeakAuditor`) rather than Vesper compiler `Mode::Linear` lowering, `illustrative/NexusWitSafety.lean` is an illustrative formalization rather than a wired `vesper verify` pass, and mutually distrusting tenants require per-tenant rings and stores (`crates/vesper-nexus::tenant_ring`) rather than a single shared linear memory.

## 17.1 Separate suspension, ownership, and scheduling

The chapter proposes mapping one-shot suspensions to WebAssembly stack switching. Component resources represent owned or borrowed handles. A host also needs a scheduling budget so one tenant cannot occupy a worker indefinitely. Suspending a guest preserves its unfinished resource obligations.

<!-- technical-detail -->

Compiling async guest plugins to WebAssembly ordinarily requires poll-state-machine futures or whole-binary `wasm-opt --asyncify` transforms, while Canonical ABI calls invoke guest `cabi_realloc` and `memcpy` on every record or byte list. Nexus proposes four coordinated mechanisms: (1) lowering one-shot lexical `eff [...]` suspensions directly to WasmFX typed continuation instructions (`cont.new`, `suspend`, `resume`) for sub-10 ns coroutine switches with zero state-machine bloat; (2) mapping WIT `own<T>` to `Mode::Linear` (`WitOwn[Res]`) and `borrow<T>` to second-class `[T-CallProj]` `in`/`inout` loans so handles cannot leak or drop while borrowed; (3) passing padding-free Bridge `wire struct` descriptors over `SharedPluginRing` (multi-memory index `1` / Domain-1 slices) for zero-copy in-place payload inspection, using `crates/vesper-nexus::tenant_ring` (`TenantRings`) to isolate each mutually distrusting tenant in its own ring, Wasmtime store, and filter instance; and (4) charging Cranelift/Wasmtime SSA fuel quanta (`50_000` fuel units, ~25 microseconds) via `QuantumScheduler` so CPU-bound guests yield onto `PulseWorkerShard` or abort with `Fail[PluginTrap]` without starving `io_uring` completions.

    PulseWorkerShard                 WasmFX Guest Instance                     WitResourceTable<T>
    ┌──────────────────┐  resume   ┌───────────────────────────┐  (idx, gen) ┌─────────────────────────┐
    │ Run Queue +      │ ────────▶ │ Typed Continuation Stack  │ ──────────▶ │ Slot[idx]:              │
    │ QuantumScheduler │           │ cont.new / suspend        │             │   generation   == gen   │
    │ (50k fuel slice) │ ◀──────── │ FuelAccountant::charge    │ ◀────────── │   owner_tenant == tid   │
    └──────────────────┘  suspend  └─────────────┬─────────────┘  SYS-06 chk │   active_borrows (lends)│
                                                 │ 0 cabi_realloc            │   payload: Option<T>    │
                                                 ▼                           └─────────────────────────┘
                                   ┌───────────────────────────┐
                                   │ TenantRings / Memory 1    │
                                   │ PluginSliceGrantWire(24B) │
                                   └───────────────────────────┘

## 17.2 Interpret a WIT resource handle

WebAssembly Interface Types (WIT) describes a component’s imports and exports. An owned handle carries responsibility for release. A borrowed handle permits limited use while the owner remains live. A shared buffer view also needs a range, a tenant identity, and a bounded lifetime.

<!-- technical-detail -->

Instead of macro-generated integer wrappers that check borrow lifetimes only via runtime traps, Nexus compiles `.wit` world specifications into linear handle types (`WitOwn[Res]`, `PluginInstance[World]`), a 24-byte little-endian `PluginSliceGrantWire` descriptor (`wire struct`), and a `linear effect PluginHost` capability:

~~~vesper
wire struct PluginSliceGrantWire {
    ring_offset: u32le,
    byte_len: u32le,
    schema_id: u32le,
    tenant_flags: u32le,
    record_timestamp_ns: u64le,
}
linear struct WitOwn[Res] { rep_idx: u32, generation: u32 }
linear struct PluginInstance[World] { instance_id: u64, remaining_fuel: u64 }
linear effect PluginHost {
    fn instantiate_with_fuel[W: WitWorld](
        in compiled: CompiledComponent[W], fuel_budget: u64,
    ) -> Result[PluginInstance[W], PluginError];
    fn call_transform_zc[W: WitWorld](
        inout inst: PluginInstance[W], in grant: PluginSliceGrantWire,
        in payload_ro: DmaSlice, inout payload_out: DmaSliceMut,
    ) -> Result[u32, PluginError];
    fn destroy_instance[W: WitWorld](sink inst: PluginInstance[W]) -> Unit;
}
~~~

## 17.3 Walk a message through a tenant filter

The host lends a message view, gives the plugin fuel, and collects its decision. On success, trap, or exhaustion, the host must end the loan and release invocation resources before recycling the buffer. Keeping the bytes in place saves copying only if the host and guest honor that interval.

<!-- technical-detail -->

When a broker shard runs a tenant filter over `DmaSlice` buffers, `[T-Let]` forbids propagating an error with `?` across `call_transform_zc` while the linear `PluginInstance` `inst` remains unconsumed (`E0402`). Matching explicitly on `call_transform_zc` forces both the normal completion arm and the guest trap / out-of-gas arm to consume `inst` via `PluginHost::destroy_instance(sink inst)` before returning or raising `Fail`:

~~~vesper
fn apply_tenant_wasm_udf(
    in compiled_udf: CompiledComponent[StreamFilterWorld],
    in batch_in: DmaSlice,
    inout batch_out: DmaSliceMut,
    inout metrics: TenantUdfMetrics,
) -> Result[u32, FilterError] eff [PluginHost, Clock, Fail[FilterError]] {
    let mut inst: PluginInstance[StreamFilterWorld] =
        PluginHost::instantiate_with_fuel(in compiled_udf, 100_000)
            .map_err(FilterError::InitFailed)?;
    let grant = PluginSliceGrantWire {
        ring_offset: 0u32.to_le(),
        byte_len: (batch_in.len() as u32).to_le(),
        schema_id: 0x0201u32.to_le(),
        tenant_flags: 1u32.to_le(),
        record_timestamp_ns: Clock::now_ns().to_le(),
    };
    match PluginHost::call_transform_zc(inout inst, in grant, in batch_in, inout batch_out) {
        Ok(written_bytes) => {
            metrics.udf_invocations += 1;
            metrics.fuel_consumed += 100_000 - inst.remaining_fuel;
            PluginHost::destroy_instance(sink inst);
            Ok(written_bytes)
        }
        Err(trap_err) => {
            metrics.udf_traps += 1;
            PluginHost::destroy_instance(sink inst);
            Fail::raise(FilterError::GuestTrapped(trap_err))
        }
    }
}
~~~

## 17.4 State the isolation property precisely

A table lookup should resolve a handle only for its tenant, at the right generation, while the entry is live. Dropping an owner requires the outstanding loan count to reach zero. A proof about this table transition supports those checks. Other parts of a WebAssembly runtime need separate isolation arguments.

<!-- technical-detail -->

At the handle-table boundary, guest code holds a plain integer pair `(idx, generation)` and cannot forge access to another tenant's slot or drop an `own<T>` resource while borrows remain active (`activeBorrows > 0`). The illustrative Lean 4 development `illustrative/NexusWitSafety.lean` states both transition invariants directly over `WitSlotEntry`—while scoping out Wasmtime linear-memory sandboxing, per-tenant ring separation (`tenant_ring`), and host-import linking, which require independent runtime arguments:

    -- illustrative/NexusWitSafety.lean
    theorem wit_handle_tenant_isolated
        (slot : WitSlotEntry) (reqIdx : UInt32) (reqGen : UInt32) (tenantA tenantB : UInt64)
        (h_owner : slot.ownerTenant = tenantA)
        (h_diff : tenantA != tenantB) :
        WitTable.lookupForTenant slot reqIdx reqGen tenantB = .error .unauthorizedTenant := by
      simp [WitTable.lookupForTenant, h_owner, h_diff]
    ----
    theorem wit_drop_own_rejects_active_borrows
        (slot : WitSlotEntry) (h_borrowed : slot.activeBorrows > 0) :
        WitTable.dropOwnSlot slot = .error .resourceHasActiveBorrows := by
      simp [WitTable.dropOwnSlot, h_borrowed]
      omega

## 17.5 Reject a stale slot and charge for work

A generational table pairs a slot number with a generation. Recycling slot 4 increments its generation, so an older handle for slot 4 fails lookup. A separate fuel counter limits how much work the guest can do, including while its handles are valid.

<!-- technical-detail -->

In `crates/vesper-nexus`, `WitResourceTable<T>` pairs `idx` with `generation`, `owner_tenant`, and `active_borrows` (`num_lends`), while `FuelAccountant::charge` enforces deterministic instruction budgets. Per `REVIEW-STATUS.md` (`SYS-06`), `end_borrow` MUST verify `slot.owner_tenant == tenant_id` alongside `begin_borrow` and `drop_own` so Tenant B cannot end Tenant A's active borrow and prematurely unblock `drop_own`. The production crate (`crates/vesper-nexus/src/table.rs` and `fuel.rs`) further hardens the manuscript sketch below by pre-allocating fixed table capacity, retiring slots with `GenerationExhausted` on `u32::MAX` instead of wrapping to generation `1` (eliminating ABA wrap reuse), redacting `expected` owner IDs from `UnauthorizedTenant { actual }`, auditing host `#[must_use]` `WitOwn`/`WitBorrow` drops via `LeakAuditor`, and separating `FuelError` from `WitTableError`:

    // crates/vesper-nexus — generational WIT resource table (with SYS-06 tenant check) & FuelAccountant
    pub struct WitResourceTable<T> { slots: Vec<ResourceSlot<T>>, free_indices: Vec<u32> }
    impl<T> WitResourceTable<T> {
        pub fn begin_borrow(&mut self, tenant_id: u64, h: WitHandle) -> Result<&T, WitTableError> {
            let slot = self.slots.get_mut(h.idx as usize)
                .filter(|s| s.generation == h.generation && s.payload.is_some())
                .ok_or(WitTableError::InvalidOrStaleHandle(h))?;
            if slot.owner_tenant != tenant_id {
                return Err(WitTableError::UnauthorizedTenant { expected: slot.owner_tenant, actual: tenant_id });
            }
            slot.active_borrows += 1;
            Ok(slot.payload.as_ref().expect("verified Some"))
        }
        pub fn end_borrow(&mut self, tenant_id: u64, h: WitHandle) -> Result<(), WitTableError> {
            let slot = self.slots.get_mut(h.idx as usize)
                .filter(|s| s.generation == h.generation && s.payload.is_some() && s.active_borrows > 0)
                .ok_or(WitTableError::InvalidOrStaleHandle(h))?;
            if slot.owner_tenant != tenant_id {
                return Err(WitTableError::UnauthorizedTenant { expected: slot.owner_tenant, actual: tenant_id });
            }
            slot.active_borrows -= 1;
            Ok(())
        }
        pub fn drop_own(&mut self, tenant_id: u64, h: WitHandle) -> Result<T, WitTableError> {
            let slot = self.slots.get_mut(h.idx as usize)
                .filter(|s| s.generation == h.generation && s.payload.is_some())
                .ok_or(WitTableError::InvalidOrStaleHandle(h))?;
            if slot.owner_tenant != tenant_id {
                return Err(WitTableError::UnauthorizedTenant { expected: slot.owner_tenant, actual: tenant_id });
            }
            if slot.active_borrows > 0 {
                return Err(WitTableError::ResourceHasActiveBorrows { idx: h.idx, active_borrows: slot.active_borrows });
            }
            let val = slot.payload.take().expect("verified Some");
            slot.generation = slot.generation.wrapping_add(1).max(1);
            self.free_indices.push(h.idx);
            Ok(val)
        }
    }
    pub struct FuelAccountant { pub remaining: u64 }
    impl FuelAccountant {
        pub fn charge(&mut self, cost: u64) -> Result<(), WitTableError> {
            if cost > self.remaining {
                let rem = self.remaining; self.remaining = 0;
                return Err(WitTableError::FuelExhausted { requested: cost, remaining: rem });
            }
            self.remaining -= cost;
            Ok(())
        }
    }
