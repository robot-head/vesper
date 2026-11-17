# Ignis Vulkan depth (wave nine, lane 28, closes #605)

Base: `5a018bd` (main on 2026-11-16, a descendant of the required baseline
`37f7b8b`), later merged with main at `5a1019e`. Branch
`claude/project-thread-nhtd7g`. Design §19 (manuscript chapter for Ignis,
§19.9 for the Vulkan backend).

Everything below ran on **Mesa lavapipe** (llvmpipe, a CPU Vulkan driver)
with `VK_LAYER_KHRONOS_validation` and synchronization validation required.
No hardware GPU was available, and lavapipe exposes a single queue family
(graphics + compute + transfer), so nothing here is a claim about hardware
drivers or multi-family devices.

## What was built

All in `crates/vesper-ignis-vulkan`. `unsafe` stays inside `src/vk/` (the
Vulkan/XCB FFI modules); the new planning modules are `unsafe`-free.

1. **Formats and depth/stencil** (`src/format.rs`, `vk/device.rs`,
   `vk/backend.rs`).
   * `Format`: RGBA8 unorm/sRGB, BGRA8, RGBA16F, RGBA32F, R32F, R32UI, D16,
     D32F, D24S8, D32FS8. Usage bits come from the device's format features;
     a format the device cannot use is `VulkanError::UnsupportedFormat`.
   * Depth/stencil textures follow the D3D12 plane convention (plane 0 =
     depth, plane 1 = stencil). Layouts are tracked per plane/mip/layer, and
     `separateDepthStencilLayouts` is enabled when present. A barrier on one
     plane without it is a typed `Unsupported` error.
   * `GraphicsDesc` adds the color format, depth format, depth test/write and
     a stencil op with a dynamic reference. `PassWork::Render` takes optional
     color and depth targets with clears. `PassWork::CopyPlaneToBuffer` reads
     back one plane.
2. **Split barriers** (`src/split.rs`). Each stream's begin (`SyncAfter =
   SPLIT`) and end (`SyncBefore = SPLIT`) halves are paired before recording.
   * The whole barrier becomes `vkCmdSetEvent2` at the begin half and
     `vkCmdWaitEvents2` at the end half, with identical `VkDependencyInfo` as
     the spec requires. Events are per allocator and host-reset before reuse.
   * On a queue without graphics or compute (a transfer-only family), events
     cannot carry those stages, so the pair collapses to one barrier at the
     end half.
   * Unpaired, reopened, mismatched or malformed halves are typed
     `SplitError`s, raised before anything is recorded.
3. **Buffer sub-range barriers** (`src/ranges.rs`). The planner emits
   whole-buffer barriers (D3D12 enhanced barriers cannot narrow), but Vulkan
   buffer barriers take an offset and size.
   * Each barrier is narrowed to the hull of the byte ranges that later
     passes on the same stream touch, up to the next barrier naming the same
     buffer.
   * A later whole-buffer access, or no later access (a handoff before a
     signal), keeps the barrier whole.
4. **Stage/access filtering by queue capability** (`translate::QueueCaps`).
   Masks are intersected with what both the family and the Ignis queue kind
   can do: Direct = graphics + compute, Compute = compute, Copy = transfer.
   A mask that filtering empties becomes `ALL_COMMANDS`, as Vulkan's
   semantics for the first or second scope require.
5. **Queue-family ownership transfers** (`src/ownership.rs`, opt-in with
   `Sharing::Exclusive` or `VESPER_IGNIS_VULKAN_SHARING=exclusive`).
   * A pure planner finds each change of owning family across a submission's
     segments. The backend records a release barrier at the end of the last
     segment on the old family and an acquire barrier at the start of the
     first segment on the new one.
   * Contents being discarded (`UNDEFINED`) skips the transfer.
   * Owners persist on each `GpuBuffer`/`GpuTexture` across submissions.
6. **Presentation** (`vk/present.rs`).
   * `XcbWindow` loads `libxcb.so.1` at run time (no build-time X
     dependency), maps a window, and reads it back with `xcb_get_image`.
   * `Swapchain` (`VK_KHR_xcb_surface` + `VK_KHR_swapchain`) presents a
     texture by blitting it into an acquired image.
   * Both are linear owners.
7. **Sync fds polled from Pulse** (`vk/syncfd.rs`, `src/pulse.rs`).
   * `VulkanBackend::sync_fd_exporter()` is taken before the backend moves
     into the `Runtime`. `export(timeline, value, mode)` returns a `SyncFd`,
     a linear owner that becomes readable once the timeline reaches `value`.
   * **Native:** a submission waits on the timeline value and signals a
     binary semaphore, which is exported as a `sync_file` with
     `vkGetSemaphoreFdKHR(SYNC_FD)`. A fence tells `close` when the
     semaphore can be destroyed. An fd of `-1` (payload already signalled)
     is represented by a readable pipe.
   * **Bridged:** a thread blocks in `vkWaitSemaphores` and writes a byte to
     a pipe.
   * `pulse::SyncFdWaiter` submits one-byte reads to Pulse's `EpollDriver`,
     which reads only after readiness. A sync_file polls `EPOLLIN` when its
     fence signals, and its read then fails with `EINVAL`; the waiter treats
     that as "signalled".
   * The backend refuses `destroy` (`VulkanError::Pending`) while any export
     is open, because exports use its timeline semaphores.
8. **Goldens** (`src/golden.rs`). PAM (`P7 RGB_ALPHA`) encode/decode with a
   bounded, strict parser, plus a tolerance diff. `VESPER_IGNIS_VULKAN_BLESS=1`
   rewrites the goldens in `tests/goldens/`.

## Acceptance tests

Run in CI by `.github/workflows/ignis-vulkan.yml` with
`VESPER_IGNIS_VULKAN_REQUIRE=1`, `VALIDATION=required`, `REQUIRE_PRESENT=1`
and `REQUIRE_SYNC_FD=1`, so a missing device, X server or sync-fd extension
fails instead of skipping. Every device test ends with `assert_clean`: no
validation message of error severity.

* `tests/depth.rs`
  * Two instanced triangles render into RGBA8 with D32S8 depth/stencil: the
    near one wins the depth test, and the stencil is written where either
    covers. The color result matches `goldens/depth_stencil.pam`. Depth
    (0.25 / 0.75 / 1.0) and stencil (7 / 0) planes are read back separately
    and checked per pixel.
  * The triangle renders into RGBA16F, BGRA8 and RGBA32F targets.
  * Every `Format` is either usable or a typed error (lavapipe supports all
    11).
* `tests/barriers.rs`
  * Split barriers produce events across 2 frames and validate. Stats count
    the events.
  * Buffer barriers narrow to sub-ranges: a fill of bytes 64..128 and a
    later copy read only that range.
  * Split barriers on a copy queue are collapsed.
  * Compute-queue barriers drop graphics stages: a dispatch of
    `shaders/fill.wgsl` on a Compute queue, where the planner's
    `ALL_SHADING` masks are filtered.
  * `Exclusive` sharing runs three queue kinds.
* `tests/present.rs`
  * The test starts its own `Xvfb -displayfd`, or uses `$DISPLAY`. It
    renders the triangle and presents it `image_count + 1` times, checking
    that the swapchain used at least 2 images.
  * The pixels read back from the X server equal the rendered texture
    exactly (tolerance 0) and match `goldens/present_triangle.pam`.
  * A missing display is a typed `Unavailable` error.
* `tests/syncfd.rs`, for both native and bridged modes:
  * Frame 1 is submitted. Then fds are exported for its terminal point and
    for the next point, which nobody has submitted yet (wait-before-signal).
    Both are watched on one Pulse epoll driver.
  * Only fd 1 fires. Fd 2 stays quiet for 200 ms, and after frame 2 is
    submitted a blocking `poll(true)` returns it.
  * After the backend is destroyed, export is refused. An open fd makes
    `destroy` fail with `Pending`.
* Unit tests cover the pure modules: split pairing and its errors, ownership
  planning, range narrowing, capability filtering, PAM round-trips and
  rejection of malformed or oversized input, and format planes.

## Decisions

* **Split barriers stay events.** Validation layer 1.3.275 (Ubuntu 24.04,
  the local host) reports `SYNC-HAZARD-WRITE-AFTER-WRITE` at
  `vkCmdWaitEvents2` for a layout transition carried by an event pair. That
  is a layer bug: the spec requires the transition in both dependency infos.
  CI's 1.4.341 accepts the same commands (run 43968514216). So
  `split_barriers_become_events_and_validate` fails only against old local
  layers.
* **Collapse rather than reject splits on transfer queues.** Events on a
  transfer-only family cannot name the graphics/compute stages a D3D12 split
  may carry. The end half already orders correctly, so collapsing costs only
  the overlap.
* **Narrow to the hull, not a list of ranges.** One `VkBufferMemoryBarrier2`
  per planner barrier keeps the 1:1 mapping with the planner. The hull is
  never smaller than any access that needs the dependency.
* **Sync fds use epoll, not io_uring.** io_uring issues the read at once,
  and a sync_file read is `EINVAL` before and after the signal alike.
  `io_uring_setup` is also denied in the cloud sandbox.
* **Native sync fds need `/dev/udmabuf` on lavapipe.** Mesa lavapipe
  advertises `VK_KHR_external_semaphore_fd`/`SYNC_FD` only when it can open
  `/dev/udmabuf`. CI loads the `udmabuf` module (from `linux-modules-extra`
  when needed) and makes it world-accessible. The local cloud host has no
  udmabuf, so the native test is skipped there, and the bridged path covers
  that case.
* **Presentation copies, not renders, into swapchain images.** Swapchain
  images are not graph resources: their lifetime and acquire order belong to
  the WSI. `present_texture` blits from an idle texture and waits on its own
  fence, so the frame graph stays independent of the swapchain.

* **Native sync fds only for submitted points.** A native export submits
  a wait on the timeline's own queue. For a point whose signal is not
  submitted yet, that wait would sit ahead of its own signal on the same
  queue, with no forward progress. `Auto` bridges such points, and `Native`
  rejects them (`Unsupported`).
* **Ownership transfers need a semaphore path.** The planner orders hazards
  only, so two reads on different families may have no dependency at all.
  Vulkan requires the release to execute before the acquire, so a transfer
  whose acquiring segment has no transitive semaphore wait on the releasing
  segment's signal is `VulkanError::OwnershipTransfer`. Submission order is
  not treated as a dependency.
* **Review fixes (Codex review of the PR).**
  * Depth/stencil plane copies on a non-graphics family are `WrongQueue`
    (VUID-10216; `maintenance10` is not enabled).
  * Plane copies constrain only the buffer offset, not the byte count.
  * Presenting an integer format, or a format without `BLIT_SRC`, is
    `Unsupported`.
  * A presentation fence that times out is no longer reset: the copy stays
    in flight, and the next call or `destroy` waits for it.

## Checks run

Local cloud host: Ubuntu 24.04, Mesa 25.2.8 lavapipe, VVL 1.3.275, Xvfb,
Rust 1.97 and 1.100.

* `cargo fmt --all -- --check`: clean.
* `cargo +1.100.0 clippy --locked -p vesper-ignis-vulkan --all-targets -- -D warnings`:
  clean.
* `VESPER_IGNIS_VULKAN_REQUIRE=1 VESPER_IGNIS_VULKAN_VALIDATION=required VESPER_IGNIS_VULKAN_REQUIRE_PRESENT=1 cargo test --locked --no-fail-fast -p vesper-ignis-vulkan`:
  * unit tests: 17 pass;
  * depth: 3 pass;
  * lavapipe: 8 pass;
  * present: 2 pass;
  * schedule: 3 pass;
  * syncfd: 3 pass (native skipped for lack of udmabuf);
  * barriers: 4 of 5 pass. The split-barrier test fails only from the
    VVL 1.3.275 bug described under Decisions.
* Workspace `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`
  and `cargo test --workspace --locked`: see the PR description for results
  on the merged head.
* CI (`Ignis Vulkan (lavapipe)`, ubuntu-latest: Mesa 26.0.8, VVL 1.4.341,
  Rust 1.100): the PR's run is the acceptance run for the native sync-fd
  path and for clean validation of every test.

## Root patch (own commit)

`Cargo.lock` only: `vesper-ignis-vulkan` gains Linux dependencies on
`libloading =0.8.9` (already locked) and the workspace crate `vesper-pulse`.
No new crates, workspace members or Bazel targets. Bazel derives
dependencies from the lockfile, and `tools/bazel/sync_build_files.py` made
no changes.

## Remaining gaps

* **Ownership transfers did not run across real families.** lavapipe has
  one queue family, so with `Exclusive` sharing every transfer is a no-op on
  the device. The planner is unit-tested, and the device test only proves
  that the exclusive mode validates. A multi-family GPU is needed for more.
* No multisampling, mipmapped render targets, or 3D/array depth targets.
* Presentation covers X11/XCB only (no Wayland), FIFO by default, and one
  direct queue. An out-of-date or suboptimal swapchain is reported
  (`OutOfDate`) but not recreated automatically.
* Sync fds are exported for timeline points only, not for fences. The
  native path was exercised only on lavapipe 26.0.8 in CI.
* Rust ownership of `SyncFd`, `Swapchain` and `XcbWindow` is host-library
  discipline. It is not evidence that the Vesper source typing rules for
  §19.9 linear extern types are implemented.
* DirectX 12 and Metal remain deferred.
