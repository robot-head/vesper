# Fiber stack images and Chrono byte-image checkpoints (wave seven, lane 3)

Claim: #326. Design sections §12 (native fibers) and §31 (Chrono).
Baseline: `283c811` (origin/main, a descendant of the required `c025b9a`).

## What was built

### `vesper-fiber-abi`: additive stack-image API (every direct-fiber target)

All additions; the ABI v2 `repr(C)` layouts, offsets and switch frames are
unchanged (`abi.rs`, `switch.rs` and `switch_windows.rs` are not modified;
the existing `round_trip`, `objects`, `platform`, `windows_platform`,
`fp_env`, `lifecycle`, `scheduler` and `fiber_lifecycle` tests pass as
before, including the compile-time `size_of::<FiberContext>() == CTX_SIZE`
and offset assertions).

* `Parked::stack_image() -> StackImage { saved_sp, top, bytes }`: the bytes
  in `[saved_sp, top)`, saved switch frame first. `StackImage::word_at`.
* `Parked::register_frame() -> RegisterFrame`: the switch frame decoded per
  target (`FrameAbi::{SysVX86_64, Aarch64, WindowsX64}`): callee-saved
  GPRs, d8–d15 / xmm6–xmm15, `mxcsr`/`x87_cw` or `fpcr`/`fpsr`, the Windows
  TEB stack fields, return address and frame pointer.
  `RegisterFrame::parse_as` decodes any of the three layouts.
* `FiberConfig::fixed_stack_base: Option<usize>` (default `None`, so every
  existing `..Default::default()` literal still compiles) and
  `FiberConfig::at_free_fixed_base()`. `FiberStack::with_fixed_base`
  reserves the stack at exactly that address:
  * Linux: `mmap(MAP_FIXED_NOREPLACE)`; an occupied range is
    `io::ErrorKind::AlreadyExists` and nothing is replaced (pre-4.17
    kernels that treat the flag as a hint are caught by an address check).
  * macOS: the address as a hint without `MAP_FIXED`; another placement is
    unmapped and reported the same way.
  * Windows: `VirtualAlloc` at the address (must be 64 KiB-granular), which
    fails instead of replacing.
  The reservation gains one *context page* below the guard (Windows: below
  the overflow reserve) and the `FiberContext` lives there, so the context
  address is the same in every fiber created at that base. Floating fibers
  keep the heap context and their old layout.
* `Parked::image() -> Result<FiberImage, ImageError>` (fixed-address
  fibers only) and `Fiber::from_image(template: Fiber, &FiberImage) ->
  Result<Parked, Box<ImageRejected>>`. The template is a *created* fiber
  (made with the unsafe `Fiber::new` or a prototype spawn, which vouches for
  its entry). The restore copies the stack bytes, generation, park request,
  park count and data frame in; the restored continuation gets the
  template's fiber id. Rejected with a typed `ImageError`, template handed
  back untouched: `EntryMismatch`, `ArgumentMismatch`, `CodeMismatch`
  (different code `Rc`, or the original code freed), `EvidenceMismatch`,
  `DataSize`, `StackSize`, `AddressMismatch`, `NotFixedAddress`, `NotFresh`,
  `Malformed`.
* Code and evidence identity use per-thread weak anchors: the image names an
  anchor that holds `Weak` references to the code owner and evidence vector,
  so a freed module whose allocation address is reused by different code is
  not mistaken for the original.
* `FiberImage::encode` / `decode`: versioned, ends in a per-thread seal (a
  keyed hash with the standard library's randomly keyed `RandomState`).
  `decode` rejects modified bytes and images sealed by another thread or
  process (`BadSeal`) before trusting any field, because only such images
  can be restored safely. This is binding and integrity, not a
  cryptographic MAC against code running inside the process.
* `JitPrototype::{code, spawn_pulse_read_with, spawn_nested_with}` for tests
  and Chrono.

Unsafe code added only where the assignment allows: the fixed mappings
(`stack.rs`, `stack_windows.rs`), writing the context into its page, and the
two image copies in `fiber.rs`.

### `vesper-chrono`

1. **Byte-image activation checkpoints** (`checkpoint`). Checkpoint image
   version 2 adds `ActivationCheckpoint::image` (the encoded `FiberImage`);
   version-1 images still decode. `RecordedActivation::checkpoint` includes
   the image for fixed-address fibers. `ActivationCheckpoint::restore_image`
   restores without re-execution: direct (tail-resumptive) handlers do not
   run again (the test's Clock counter stays at 1; re-execution moves it
   to 2 and changes the result). A different entry argument is
   `CheckpointError::Image(ArgumentMismatch)`, different code
   `EntryMismatch`/`CodeMismatch`; an image of another park is
   `ImageMismatch`. `restore` (re-execution) is kept for fibers without a
   fixed address. The native module now also builds on Windows x86_64.
2. **CoreSight formatter removal** (`formatter`): 16-byte TPIU/ETF frames to
   per-source-ID streams, ID-change auxiliary-bit semantics, null, trigger
   and flush IDs, reserved IDs rejected, `Frames` (ETF/ETB/ETR memory) and
   `FrameSync` (TPIU with `FF FF FF 7F` full-frame syncs) modes, output
   budget.
3. **AArch64 flow reconstruction from ETE atoms** (`ete_flow`): an A64
   branch classifier (`B`, `BL`, `B.cond`/`BC.cond`, `CBZ`/`CBNZ`,
   `TBZ`/`TBNZ`, `BR`/`BLR`/`RET`/`ERET` and PAC forms) walks the code image
   to each P0 element; E/N atoms resolve direct and conditional branches,
   address elements resolve indirect ones, exceptions end the walk at the
   preferred return address. Output reuses `ipt_flow::{FlowTrace, Branch,
   BranchKind}` with timestamp intervals, so `BranchTimeline::steps_from_pt`
   places ETE flow on the effect timeline too. An indirect branch or
   exception keeps the timestamp interval of its atom or exception element
   even when its target address arrives after further timestamps, and a
   context element reporting AArch32 execution makes further flow a typed
   `NotA64` error.
4. **VirtualArena copy-on-write shard checkpoints** (`arena`): `ShardArena`
   shares every shard with a checkpoint (no bytes copied), copies a shard
   on its first write after one, restores, and serialises incremental
   `ArenaDelta`s (only shards written in the epoch, BLAKE3 trailer) that
   `capture_arena_epoch` stores as snapshot epochs and `ArenaDelta::apply`
   replays. Every `keyframe_every`-th epoch is stored as a full keyframe,
   so that after the recorder evicts old snapshot slots `rebuild_arena`
   still rebuilds the newest state from the newest retained keyframe.
   `probe_write_tracking` reports the kernel mechanisms.

`Unavailable::NativeActivationCheckpoint` was renamed
`Unavailable::KernelWriteTracking` (no users outside the crate): byte-image
checkpoints now exist; what is unavailable is kernel-trapped write tracking.

## Checks run

Host: cloud x86_64 Linux container (4 vCPU), Rust 1.97 stable and 1.100.0.

* `cargo fmt --all -- --check`: clean.
* `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  clean (x86_64 Linux).
* `cargo +1.100.0 clippy -p vesper-fiber-abi --all-targets --target
  x86_64-pc-windows-msvc` and `--target aarch64-apple-darwin`, `-D
  warnings`: clean. Chrono could not be checked for Darwin here (blake3's C
  build needs the macOS SDK); macOS CI covers it.
* `cargo test -p vesper-fiber-abi -p vesper-chrono` (x86_64 Linux): all
  pass, repeated 8 times to check the fixed-address tests for flakiness.
* AArch64 Linux under `qemu-aarch64` 8.2 (`cargo +1.100.0 test --target
  aarch64-unknown-linux-gnu`): `stack_image` 6/6, `round_trip` 14/14,
  `checkpoint` 5/5, `image_checkpoint` 4/4, `arena` 5/5, `coresight_flow`
  8/8.
* Windows x86_64 under Wine 9.0 (`x86_64-pc-windows-gnu` build):
  `stack_image` 6/6, `image_checkpoint` 4/4, `checkpoint` 5/5, `arena` 5/5,
  `windows_platform` 11/11, `fiber_lifecycle` 4/4, three runs each.
  Real Windows (MSVC) and macOS AArch64 run in CI.
* `VESPER_LEAN=<lean 4.24.0>/bin/lean cargo test -p vesper-chrono --test
  lean_model`: 2/2 (Lean model unchanged; it still compiles and matches the
  Rust lineage).
* `cargo test --workspace --locked`: 694 tests passed before the run
  stopped at `tests/lean4lean42_acceptance.rs`, which hard-fails without
  `VESPER_LEAN4LEAN42_ROOT` (a prepared candidate that only CI provides; the
  test is unrelated to this lane). CI runs the whole workspace.

## Decisions

* **Template-based restore.** `Fiber::from_image` is safe because the
  template was created through the unsafe `Fiber::new` contract, and the
  image is either in memory (captured by this thread) or sealed by it.
* **Context in the reservation.** Stack bytes hold the context pointer
  (`fiber_main`'s and `vesper_fiber_park`'s frames, callee-saved
  registers), so the context must keep its address too; it lives in a page
  of the fixed reservation. The data frame may move (generated code
  reloads its base after every park, as for `relocate_data`).
* **Address reuse.** Restoring needs the captured range to be free again.
  `free_fixed_base` is inherently racy: any mapping made between release and
  restore (a JIT module, a thread stack, a large `malloc`) can take the
  range, and creation then fails with `AlreadyExists` rather than replacing
  it. The tests serialise fixed-address tests and map everything else
  first; Wine reuses freed ranges especially eagerly.
* **Arena write tracking at the API.** The host permits userfaultfd
  (`CAP_SYS_PTRACE` present, `/dev/userfaultfd` openable;
  `unprivileged_userfaultfd=0`), but registering ranges and
  `UFFDIO_WRITEPROTECT` need unsafe ioctls, and `mprotect` tracking needs a
  `SIGSEGV`/vectored handler; both are outside the unsafe code this lane may
  add (fiber stack mapping and image copy only). The Linux soft-dirty bit
  needs no unsafe code and is exercised by the probe: on this host a write
  after `clear_refs` did **not** set it (kernel likely without
  `CONFIG_MEM_SOFT_DIRTY`). `vesper-chrono` cannot depend on
  `vesper-alloc` (existing dependencies only), so `ShardArena` is Chrono's
  own checkpointable region, not `vesper-alloc`'s `VirtualArena`.

## Remaining gaps

* Kernel-trapped arena write tracking (userfaultfd write-protect,
  `mprotect` faults, Mach `vm_copy`, Windows write watches) is not
  implemented; writes through raw pointers into a shard are not tracked.
  It needs an owner allowed to write that unsafe code.
* Byte images restore only in the capturing process and thread and only at
  the captured address; cross-process replay still uses re-execution.
* Restoring an image re-materialises a continuation: the image does not
  discharge or transfer the captured fiber's obligations. This is
  host-library discipline; Rust ownership here is not the Vesper linear
  typing rules.
* The formatter and ETE flow decoders follow this lane's reading of the
  CoreSight and ETE specifications and were tested only against encoders
  written here. No CoreSight hardware trace was captured (no `cs_etm`
  PMU). Half-word TPIU sync, return-stack compression, ISB/WFI as P0
  elements and T32 flow are not modelled.
* `RegisterFrame` is decoded from the saved switch frame only; registers of
  frames deeper in the stack are not unwound.
* `tests/lifecycle.rs` and `tests/fp_env.rs` stay Linux-only: they use the
  Linux-only Pulse scheduler adapter. Widening them follows once the
  portable Pulse drivers (PR #363) land.
* No performance claims: nothing was benchmarked.
