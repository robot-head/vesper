# Ignis: a Vulkan 1.3 backend run on Mesa lavapipe (wave seven, lane 32)

Claim: #356. Base: `283c811` (descendant of the required baseline `c025b9a`).
Branch: `claude/project-thread-genkht`. Design section: §19 (§19.9 Vulkan
lowering). Software stand-in chosen by the owner on 2026-11-14: Mesa lavapipe
(llvmpipe) instead of a hardware GPU.

## What was built

New crate `crates/vesper-ignis-vulkan` (new workspace member) implementing
`vesper_ignis::lifecycle::Backend` with `ash =0.38.0` (Vulkan 1.3.281 bindings,
`loaded` feature: libvulkan is opened at run time) and `naga =30.0.2`
(`wgsl-in`, `spv-out`). `vesper-ignis` is a read-only dependency; nothing in it
changed.

- **Device and queues** (`Device`): Vulkan 1.3 instance and device with
  synchronization2, timeline semaphores, dynamic rendering and
  `VK_KHR_push_descriptor`. Direct/compute/copy queue kinds map to the best
  matching queue families; resources use `CONCURRENT` sharing when more than one
  family is in use. Lavapipe exposes one family with one queue, so all Ignis
  queues share it there. `VK_LAYER_KHRONOS_validation` is loaded on request
  (`off`, if available, or required) with `VK_EXT_validation_features`
  synchronization validation enabled; a debug-utils messenger records warnings
  and errors per device.
- **Barrier translation** (`translate`, no `unsafe`): the planner's D3D12
  enhanced-barrier sync/access/layout vocabulary becomes
  `VkPipelineStageFlags2`/`VkAccessFlags2`/`VkImageLayout`, and each planner
  barrier batch becomes one `vkCmdPipelineBarrier2`. Aggregate scopes are
  expanded conservatively; UAV accesses are read+write; queue-specific D3D12
  layouts map to their generic Vulkan layout. Ray-tracing scopes/accesses,
  stream output, VRS layouts/accesses and split barriers are typed
  `TranslateError`s.
- **Fences** (`schedule`, no `unsafe`): every runtime timeline is a timeline
  `VkSemaphore`. Each queue stream is cut into *waits → body → signals*
  segments; each segment is one `vkQueueSubmit2` with timeline wait/signal infos.
  Because several Ignis queues may share one in-order `VkQueue`, segments are
  submitted in an order where every wait's signal was submitted first; a plan
  with no such order is `ScheduleError::Stalled` before anything is submitted.
  `poll` reads semaphore counters and reports each original signal (planner and
  terminal) exactly once, strictly in submission order, as a lifecycle
  `Completion` (queue fence observation).
- **Lifetimes**: `CommandAllocator` (`VkCommandPool`), `GpuBuffer`,
  `GpuTexture`, `Pipeline` and `VulkanBackend` are non-`Clone`, `#[must_use]`
  owners released only by a consuming `destroy`; dropping one leaks the Vulkan
  object, keeps the device alive, and increments `leaked_owners()`. A frame's
  allocators and `FrameResources` (bound native objects, pipelines, per-pass
  work) move into the lifecycle `Runtime` and come back only on retirement,
  after every original terminal signal was observed. Each stream ends with a
  device-to-host memory barrier, so retired buffers can be read on the host.
  `VulkanBackend::destroy` refuses while any submitted signal is incomplete.
  Rust ownership here is host-library discipline; it is **not** evidence that
  the §19.9 Vesper source typing rules (`linear extern type`s) are implemented.
- **Layout tracking**: each texture's Vulkan layout is tracked per
  (mip, layer) across submissions. A barrier whose `layout_before` disagrees
  with the tracked layout, or a pass whose texture is not in the layout its
  work needs, is `VulkanError::LayoutMismatch` and the submission is rejected
  with every owner returned. The tracked state is committed only after the
  whole submission is on the device.
- **Pass work** (`PassWork`): empty, a dynamic-rendering draw into an RGBA8
  target (optional clear), a compute dispatch with push-descriptor bindings
  (sampled texture, storage texture, storage buffer), texture-to-buffer copy,
  and buffer fill. Work on the wrong queue kind, unbound or wrong-kind
  resources, missing pass work, bad pipelines, over-limit dispatches and
  undersized readback buffers are typed errors before submission.
- **Shaders** (`shader`): WGSL is parsed, validated and compiled to SPIR-V 1.3
  in-process with naga; `@group(0)` bindings are reflected and checked against
  the declared pipeline layout. No external shader toolchain is used.
- **Device loss**: `VK_ERROR_DEVICE_LOST` from any call becomes
  `VulkanError::DeviceLost` and poisons the device: later submissions are
  rejected (owners returned), polls report the error, pending frames never
  retire, and shutdown is `Incomplete` with the owners still held.

## Two Vulkan-vs-D3D12 decisions (found by synchronization validation)

1. **Batch boundary.** D3D12 completes and flushes queue work at every
   `ExecuteCommandLists` boundary and the planner relies on it (a resource's
   first barrier in a graph has `SyncBefore = NONE`). Vulkan has no such
   guarantee between submissions; without one, the second frame's layout
   transition raced the first frame's copy (`SYNC-HAZARD-WRITE-AFTER-READ` and
   `-WRITE-AFTER-WRITE`). Each Vulkan batch therefore begins with a full
   `ALL_COMMANDS` memory barrier. This serializes frames at their start and costs
   a pipeline drain per batch on real GPUs; removing it needs cross-frame
   planning.
2. **Handoff transitions.** D3D12 queue handoffs use `SyncAfter = NONE` before a
   fence signal (and `SyncBefore = NONE` after a wait). In Vulkan an empty stage
   mask on either side of a layout transition does not chain with the semaphore
   operation; validation reported `SYNC-HAZARD-READ-AFTER-WRITE` between the
   direct queue's transition and the compute queue's sampled read. For image
   barriers that change layout, an empty side is widened to `ALL_COMMANDS`
   (execution only, no access added).

## Acceptance tests (all run on lavapipe with validation required)

`tests/lavapipe.rs` (8 tests). Every test except the deliberate hazard asserts
that the validation layer reported no error.

- `triangle_and_compute_read_back_on_one_queue`: draw → compute (samples the
  color target, writes the RGBA8 complement to a storage buffer) → copy to a
  host buffer. Read-back pixels: triangle center `(255, 64, 0, 255)`, corners
  the clear color, covered area within 5% of the analytic 28.1%, and every
  compute output equal to the complement of the read-back texel.
- `three_queues_synchronize_with_timeline_semaphores`: copy queue fills, direct
  draws, compute samples and overwrites, direct copies out. The plan has ≥ 2
  cross-queue waits; pixels are checked as above.
- `frames_reuse_owners_and_layouts_carry_across_frames`: three frames reuse
  the same allocator, texture, buffers and pipelines (rebound to a new graph
  each frame, initial layout = previous final layout); then a graph with a
  wrong initial layout is rejected as `LayoutMismatch` with all owners back.
- `subresource_barriers_are_accepted_by_validation`: a 2-mip texture with
  per-mip barriers (mip 0 render target → shader resource, mip 1 copy dest →
  shader resource); tracked layouts and compute output checked.
- `missing_declaration_is_caught_by_synchronization_validation`: two buffer
  fills of the same buffer, the second in a pass that does not declare it, so
  no barrier separates the writes in one command buffer; the layer must report
  a hazard error. This shows the layer is active, so the clean runs mean
  something, and that the backend trusts the graph's buffer declarations (it
  cannot see which bytes a command or shader touches). Earlier versions of the
  test used an undeclared compute-shader write instead; validation layers
  1.3.275 flagged it but 1.4.341 (ubuntu-latest) did not, so the test now uses
  a transfer write-after-write, which both versions report. The whole suite was
  run locally against both layer versions.
- `lost_device_is_a_typed_error_and_pending_owners_stay_held`: after
  `Device::inject_device_lost`, polls report `DeviceLost`, object creation fails
  with `DeviceLost`, and shutdown is `Incomplete` with the frame pending.
- `dropped_owner_is_counted_not_released`, and
  `work_on_the_wrong_queue_and_unbound_resources_are_rejected`.

`tests/schedule.rs` (3 tests, no device): real planner output through the real
lifecycle runtime; every wait follows its signal, every body command and signal
is submitted once; never-signalled waits stall; scratch, unknown timelines and
stale signals are typed errors. Unit tests cover the translation tables and
shader compilation/reflection (7 tests).

Without a loader or device the device tests print the reason and pass, unless
`VESPER_IGNIS_VULKAN_REQUIRE=1`. The new workflow
`.github/workflows/ignis-vulkan.yml` installs `mesa-vulkan-drivers`,
`vulkan-validationlayers` and `vulkan-tools` from Ubuntu on `ubuntu-latest`,
prints `vulkaninfo --summary`, and runs the crate's tests with
`VESPER_IGNIS_VULKAN_REQUIRE=1 VESPER_IGNIS_VULKAN_VALIDATION=required` on every
PR touching `crates/vesper-ignis*`, the workflow, or the root manifests. Since
`ash` loads libvulkan at run time the crate builds under hermetic Bazel, so its
Bazel targets are not `manual`; there the device tests report the device as
unavailable.

## Checks run (cloud host, Ubuntu 24.04)

CI (`ubuntu-latest`, now Ubuntu 26.04): Mesa 26.0.8 lavapipe, device API
1.4.335, `vulkan-validationlayers 1.4.341.0-1`.

Local device: `llvmpipe (LLVM 20.1.2, 256 bits)`, driver `Mesa 25.2.8-0ubuntu0.24.04.4`,
device API 1.4.318, conformance 1.3.1.1; `vulkan-validationlayers 1.3.275.0-1`.

```sh
VESPER_IGNIS_VULKAN_REQUIRE=1 VESPER_IGNIS_VULKAN_VALIDATION=required \
  cargo test -p vesper-ignis-vulkan      # 7 unit + 8 lavapipe + 3 schedule: pass (3 repeated runs)
VESPER_IGNIS_VULKAN_VALIDATION=off cargo test -p vesper-ignis-vulkan       # pass
VESPER_IGNIS_VULKAN_DEVICE=nonexistent cargo test -p vesper-ignis-vulkan   # device tests skip, pass
cargo test -p vesper-ignis --locked                                        # pass
cargo +1.100.0 clippy -p vesper-ignis-vulkan --all-targets -- -D warnings  # clean
cargo clippy -p vesper-ignis-vulkan --all-targets -- -D warnings           # clean (1.97)
cargo fmt --all -- --check                                                  # clean
python3 tools/bazel/sync_build_files.py                                     # created the crate BUILD.bazel
```

The full `cargo test --workspace` and `bazel test //...` were left to CI (no
Bazel on this host). The whole lavapipe test binary runs in about 0.3 s here;
no other performance was measured and none is claimed.

## Root patch (own commit)

Root `Cargo.toml`: `crates/vesper-ignis-vulkan` in `members` and
`default-members` before `experiments/native-pulse`. `Cargo.lock`: the new
crate plus `ash`, `naga`, `naga-types`, `spirv`, `codespan-reporting`,
`libloading`, `bit-set`, `cfg_aliases`. `crates/vesper-ignis-vulkan/BUILD.bazel`
from `sync_build_files.py`. Lane 33 (GPU compute, #357) should adopt the same
`ash`/`naga` pins.

## Remaining gaps

- **DirectX 12 and Metal stay deferred.** This lane is the Vulkan path only; the
  D3D12 vocabulary is translated, not executed on D3D12.
- Only lavapipe was exercised (no hardware GPU on the host). Multi-family
  `CONCURRENT` sharing and multiple hardware queues are implemented but
  untested; barrier stage masks are not filtered by queue-family capability.
- Device loss is exercised by fault injection at the result-mapping boundary
  (`inject_device_lost`); a real driver-reported loss was not produced.
- Not implemented: work-graph scratch (D3D12-only, rejected), split barriers,
  ray tracing, transform feedback, VRS, depth/stencil textures, formats other
  than RGBA8, swapchains/presentation, `VK_EXT_descriptor_heap`, queue-family
  ownership transfers, buffer sub-range barriers (whole-buffer only), and
  Pulse/io_uring polling of exported sync fds (§19.9). The frame-start full
  barrier (decision 1) is conservative.
- The backend trusts the graph's declared buffer accesses; it validates texture
  layouts against barriers but cannot check what shaders access.
- No Lean model was added; nothing is proved about this backend.
