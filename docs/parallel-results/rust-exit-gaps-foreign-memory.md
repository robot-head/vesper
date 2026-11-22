# Rust-exit gap wave 1, lane 8: foreign memory in place

Claim #1505. Prompt: `docs/agent-prompts/rust-exit-gaps-1/08-foreign-memory.md`.
Base: `main` at `66255e17`.

## The gaps

Two lane reports asked for the same thing from two sides.

- **Runtime glue G9** (`rust-exit-runtime-glue.md`): every pointer parameter
  had to be `nocapture`, so C could never keep a buffer after a call
  returned. io_uring submissions, IOCP `OVERLAPPED` and AF_XDP UMEM
  registration all need that.

  ```text
  fn io_uring_submit(in ring: CSlice[u8]) -> ForeignVal[i32];
  error: borrowed `in` pointer parameters must be declared `nocapture`; capturing foreign pointers are unsupported
  ```

- **OS tools G3** (`rust-exit-os-tools.md`): memory that C owns (AF_XDP
  rings, the perf AUX buffer, guest RAM) could be copied into Vesper, but
  never viewed as a `Slice[u8]` where it is. Every frame became a copy in
  and a copy out, and every ring index became an import call.

## The fix (manuscript §16.3, flagged)

The new bullet is "Foreign Memory in Place: Pinned Buffers and Mappings".
It adds two forms that share one representation: a linear struct whose one
field `data` is a `ChunkSeq[T]` over memory that never moves.

- **`Pinned[T]`** is a buffer C may keep. A `.vsys` parameter
  `in b: Pinned[u8]` or `inout b: Pinned[u8]` (no `nocapture`) passes
  `(data, len)`, and C may hold the address after the call returns.
  - In source, `Pinned[T]` is the generated
    `linear struct Pinned[T] { data: ChunkSeq[T] }`.
  - `Pinned { data: s }` pins a sequence, and `let Pinned { data } = p;`
    releases it.
  - Linearity makes the release explicit on every path.
- **`linear extern mapping M[T];`** is memory the library owns.
  - An import returns it as `ForeignVal[Option[M[len]]]`, where `len` names a
    by-value `u32`/`u64`/`usize` parameter. `NULL` and `MAP_FAILED` are both
    `None`.
  - `in`, `inout` and `sink` parameters pass `(data, len)`, so
    `fn munmap(sink m: M) -> ForeignVal[i32];` is exactly munmap's ABI.
  - A `sink` import is required (the finalizer).
  - Source code cannot build or destructure an `M` (handle misuse).
- **Views.** Both are read, written and viewed through `b.data` under the
  ordinary sequence rules:
  - `b.data[i]`, `set`, `read_u32_le` and `try_write_u64_le` work as on any
    sequence.
  - `f(inout b.data[a..c])` lends a view.
  - `let v = b.data.as_slice();` binds a view. While it is in scope, `b`
    cannot be released (`E1531`).
  - A view never escapes (`E1520`).
  - `b` cannot be used after its release (`E0303`), dropped (`E0302`) or
    kept across an abortable effect (`E0402`).
- **A new buffer rule.** C may hold the address, so `b.data` may never grow,
  shrink, move, be replaced, or be lent `inout` whole (`BufferMisuse`, a
  frontend error, `src/ffi_lowering/buffers.rs`). Only element and range
  accesses, explicit `in` arguments and length-preserving methods are
  accepted.
- **Where it runs.** Only native code lends foreign memory: the evaluator
  refuses these imports.

### Implementation

- `crates/vesper-ffi` (parser and descriptor model):
  - `ExternTypeDescriptor::mapping()`, `ParameterType::Pinned`,
    `FunctionDescriptor::mapping_length()` and `uses_foreign_memory()`.
  - The ABI: two words for a mapping or pinned parameter.
  - The boundary assumptions.
  - Canonical bytes: the new forms are appended only when used, so every
    existing library keeps its identity.
- `src/ffi_lowering/types.rs`: the generated `Pinned[T]` (only when an
  import takes one, so no existing program gains a name) and the mapping
  structs.
- `src/ffi_lowering/buffers.rs`: the buffer rule. It runs after the checker.
  The checker does not record the types of place roots, so the pass types
  them itself from parameters, `let`s, match patterns and struct fields.
- `src/ffi_lowering/native.rs`:
  - `(data, len)` comes from the sequence header.
  - A mapping result gets a fresh `{len, len, data}` header (trap 8 when
    `malloc` fails). The `sink` import frees the header after the call.
  - The SH4 `ffi` hook is not offered these calls.
- `src/ffi_lowering.rs`: the evaluator refuses these imports at the boundary.

## The real user: an AF_XDP receive ring

`tests/foreign_memory.rs` uses the fixtures in
`tests/fixtures/foreign_memory/`. The UMEM is a `Pinned[u8]` that the socket
keeps. The socket then writes received frames into it during later
`xsk_kernel_rx` calls. The receive ring is a mapping in libxdp's layout:

- a u32 producer at byte 0 and a u32 consumer at byte 4;
- 16-byte descriptors from byte 64.

Vesper reads the producer index and the descriptors in place, checksums each
frame where the kernel wrote it, and writes the consumer index back.

- **JIT, every host target.** A Rust stand-in kernel runs 37 packets in
  batches of 5 through an 8-entry ring, so the kernel stops when Vesper has
  not caught up.
  - Also covered: 3 packets, a refused map (`MAP_FAILED` is `None`) and a
    bound view.
  - Every socket and ring is released (a live count of 0).
- **Objects, Linux x86-64.** The same program is linked with `xsk.c`, whose
  ring is a real anonymous `mmap`. `xsk_ring_unmap(sink r: Ring)` calls
  `munmap(data, len)`.
- **Refusals.**
  - Growth (`push`, `truncate`, `clear`, `reserve`, `resize`), an `inout`
    lend of the whole sequence, replacement and `swap` are refused. So are
    the same uses through a field path, a match binding and a generic.
  - Leaks, use after release, double release and a view kept across a
    release are refused, as is a returned or stored view, a view of a
    `let mut` mapping, and a mapping live across `Fail`.
  - Building or destructuring a mapping is refused.
  - So are malformed `.vsys` declarations.

### Before and after

Before, this program could not be written: no `.vsys` could give the socket
a buffer to keep, so the C side had to own the UMEM. The only option was the
lane reports' workaround. It is kept in the fixture as `rx_copy`: each ring
index and descriptor field is an import call, and each frame is copied out
through a `nocapture` slice.

Measured with `cargo test --release --test foreign_memory measure -- --ignored --nocapture`
(JIT, 200,000 packets, ABBA, median of 6 runs; the times include the stand-in
kernel writing every frame):

| Path | ns/packet |
|---|---|
| In place (`rx`) | 303.8 |
| Copies and import calls (`rx_copy`) | 277.1 |

The copy workaround came out at 0.91x of the in-place time, so in-place
access is not yet faster on this micro-benchmark. Three things make the
comparison weak:

- The stand-in kernel writes 64-664 bytes per packet byte by byte, and
  that write dominates both paths.
- The copy path's `memcpy` is cheap at these sizes.
- The in-place loop reads every ring field through the `inout` mapping's
  sequence header. Those are the header reloads of seq fields that the
  native code size thread (#1426) owns.

The win this lane delivers is a capability: a buffer C keeps, and memory C
owns read without a call per field. A faster in-place loop waits on #1426.

## Coordination and guards

- **SH4 lane 13 (#1287)** owns the FFI lowering port (`packages/vesper-ffi-lowering`).
  The port does not model pinned buffers or mappings yet. The native `ffi`
  hook declines these calls, so the Rust lowering emits them and the hybrid
  path is unchanged. The two port parity tests (`tests/self_host_ffi_lowering.rs`
  and `tests/self_host_ffi_lowering_rewrite.rs`) leave libraries that use
  foreign memory out of their corpora. The port's work: the generated
  `Pinned[T]` (a generic ADT, which the type-mapping dump does not cover)
  and the mapping structs, `ParameterType::Pinned`, and mapping results.
  - With `tests/fixtures/foreign_memory/xsk.ves` in its corpus, the Vesper
    rewrite port ran out of memory (killed at the 15 GB session limit)
    instead of reporting an unknown form. That looks like a port bug worth a
    minimal repro.
- **The FFI callback stack (#1456-#1459).** It edits the same parser function
  (`retain` callback parameters). `Pinned[...]` is parsed before handle
  names in the same branch, so the two should merge with a small textual
  conflict at most.
- **Stage 1 guard:** no guarded path is touched. `bootstrap/run.sh stage1 --outcome` exits 0 (cross-backend gate ok; no corpus program imports a `.vsys` library). It also reports 23 stale `cranelift_artifact`/`verified` fields, which come from codegen drift on `main` since the lock and not from this change.
- **The hybrid differential:** no hybrid or swap code changed, and the swap
  hook declines the new calls, so it should stay at 0. The full differential
  runs on `main` only.

## New gaps found

1. **Atomic views of foreign memory.** An index that another thread or the
   kernel writes concurrently (the io_uring SQ/CQ heads, AF_XDP indices
   written while Vesper runs) needs acquire loads and release stores. The
   fixture is single-threaded, so each import call is a synchronization
   point. Runtime glue G1 (reads through a foreign pointer) and this lane
   meet here. Minimal repro: a spin on
   `ring.data.try_read_u32_le(0)` with no foreign call in the loop. The
   optimizer may keep that read in a register.
2. **A pinned buffer's address as a value.** Raw io_uring SQEs and AF_XDP's
   `xdp_umem_reg` carry the buffer address inside a struct (os-tools G2).
   `Pinned` passes `(data, len)` as arguments only. A `Pinned::address(in p)
   -> u64` (an integer, never dereferenced in source) would close it.
3. **Aliasing.** Two mappings over the same memory, or a mapping over a
   pinned buffer, look like two distinct sequences to the optimizer. Nothing
   in the fixture does this; a library that returns overlapping mappings
   would need a rule.
4. **Vesper port of the FFI rewrite.** It ran out of memory on the new
   fixture (above).

## What is left

- The port mirror (SH4 lane 13, #1287) and the address-as-value form (gap 2,
  with os-tools G2).
- Using these in the SH6 ports:
  - lane 14's AF_XDP backend for `vesper-warp-umem`, whose port already takes
    the UMEM as `inout mem: Slice[u8]`;
  - lane 13's io_uring driver for `vesper-pulse`, once the atomic views
    (gap 1) exist for its SQ and CQ heads.
