# Wave seven lane 33: GPU compute for Tensor and Kinetic on Vulkan (lavapipe)

Claim: [#357](https://github.com/robot-head/vesper/issues/357). Design: §28
(Tensor), §36 (Kinetic). Base: `283c8115d2203232d0e9b473cc4bb0312c3a8509`
(origin/main at start, a descendant of the required `c025b9a`).

Edited paths: the new `crates/vesper-gpu-compute/**`, the new
`.github/workflows/gpu-compute.yml`, this report, and the root registration
patch (own commit, see "Root patch").

The owner chose a software stand-in for GPU hardware on 2026-11-14. Every
device result below is from **Mesa lavapipe** (`llvmpipe (LLVM 20.1.2, 256
bits)`, driver info `Mesa 25.2.8-0ubuntu0.24.04.4 (LLVM 20.1.2)`, Vulkan
1.4.318, `VkPhysicalDeviceType::CPU`) on the x86_64 cloud host, the same
packages the new workflow installs on `ubuntu-latest` (where CI got Mesa
26.0.8 with LLVM 21.1.8). lavapipe runs the
SPIR-V on the CPU through LLVM. It validates the kernels, the runtime
protocol and the Vulkan API usage. It says nothing about GPU performance or
about any hardware driver, and no performance is claimed. **Runs on a real
GPU are deferred**: there is no GPU on the cloud host or the CI runners.

## What was built

| Module | Contents |
| --- | --- |
| `runtime` | A small Vulkan compute runtime over `ash` 0.38.0 (dynamically loaded loader): `Context` (instance, first Vulkan 1.2+ device with a compute queue and timeline semaphores, `VESPER_GPU_DEVICE` filter, `DeviceInfo` with driver name and version), host-visible coherent storage `Buffer`s mapped for their lifetime, `Kernel` (compute pipeline from WGSL with `n` storage bindings and an immediate/push-constant block), `Recording` (command buffer; descriptor sets allocated per dispatch from chained pools; a compute→compute/host barrier after each dispatch) and `Submission` (one timeline-semaphore value; each submission waits for the previous value, so submissions complete in order and see each other's writes). The only `unsafe` in the crate is here (`#![deny(unsafe_code)]` elsewhere), each block with its safety argument. |
| `shader` | WGSL → SPIR-V 1.3 with `naga` 30.0.2: parse, validate (`Capabilities::IMMEDIATES`), check the binding interface (storage buffers in group 0 at `0..n`, no uniforms or textures, at most one immediate block) and emit with naga's `Restrict` bounds checks on every buffer and array access. |
| `tensor` | `TensorKernels`: `matmul` (`[m,k] × [n,k]ᵀ`, optionally accumulating, the llama2.c weight layout), `rms_norm` (per row, 256-wide tree reduction), `rope` (interleaved pairs; frequencies from the host by the CPU formula), `swiglu`, `kv_write` (one token into a 16-token page through a page table) and paged grouped-query `attention` (one workgroup per query head, two-pass softmax as in `AttentionMode::TwoPass`). `GpuModel` uploads an FP32 `vesper_tensor::llama::Model` and runs a whole decode step as one submission (`14·layers + 2` dispatches); `generate_greedy` has the CPU function's exact control flow. `GpuKvCache` leases 16-token pages from one free list when `position % 16 == 0`, top of the range first, so page tables are never the identity. |
| `broadphase` | `GpuBroadphase`: the Kinetic broadphase as sort and sweep on the device. A bitonic sort over `(key, index)` (a strict total order), a counting sweep, a one-workgroup exclusive prefix sum, and a writing sweep. Every comparison is an integer comparison of an order-preserving `u32` encoding of the f32 bounds (with `-0` encoded as `+0`, as IEEE compares them), so no device float arithmetic is involved and the result is exact. |

### Public API

- `Context::{new, info, buffer, buffer_from, kernel, record, completed_value, issued_value}`, `DeviceInfo` (`is_lavapipe`), `DeviceType`.
- `Buffer::{len, is_empty, write, read, to_u32}`; `Kernel::name`.
- `Recording::{dispatch, dispatches, submit}`; `Submission::{value, is_complete, wait}` → `Completed { value }`; `leaked_submissions()`.
- `shader::{compile_wgsl, Compiled, ShaderError, MAX_SOURCE_BYTES}`.
- `tensor::{TensorKernels, GpuModel, GpuKvCache, GpuSession, rope_frequencies, leaked_sessions, SOURCES, MAX_ATTENTION_TOKENS, MAX_HEAD_DIM}`.
- `broadphase::{GpuBroadphase, BroadphaseError, order_key, SOURCES, MAX_PRIMITIVES, MAX_PAIRS}`.
- `GpuError::{Unavailable, Vulkan, Shader, Shape, Limit, Unsupported, ForeignObject, NonFinite, Tensor}`; `test_context()`.

### Obligations

`Submission` and `GpuSession` are non-Clone, non-Copy, `#[must_use]` owners.
A submission's intended exit is `Submission::wait`. Memory safety does not
rest on that obligation: each recording holds an `Arc` of every buffer and
kernel it binds until its command buffer is freed after completion, so a
submission leaked with `mem::forget` leaks those resources instead of
freeing them under the device. Host access to a buffer is only by copying
in or out, under the queue lock, after waiting for the last submission that
bound it, so the host never reads or writes memory the device may be using
(both points were review findings on the first push). Dropping a submission
unwaited is counted by `leaked_submissions()`, and the drop still blocks on
the timeline value. A
session's only exit is `GpuKvCache::release`, which returns its pages; a
release into another cache hands the session back with
`GpuError::ForeignObject`, and a dropped unreleased session is counted by
`leaked_sessions()` with its pages left leased. This is host-library
discipline enforced by Rust ownership and counters. It is not evidence that
the Vesper source typing rules (linear resources, [T-Let]) are implemented.

## Tests and evidence

23 integration tests in four files. Device tests call `test_context()`:
without a Vulkan device they print `GPU unavailable, test not run` and
return, unless `VESPER_GPU_REQUIRE=1` (set by the new workflow), which turns
that into a failure, so the differential tests cannot pass vacuously there.

- `tests/generation.rs` (3): the bundled `stories260K` model from
  `crates/vesper-tensor/models`.
  - 200 greedy steps from BOS on the device give **the same 200 tokens** as
    `vesper_tensor::llama::generate_greedy` (FP32 weights, FP32 KV, two-pass
    attention), and the decoded text equals llama2.c's reference
    `stories260K-greedy-200.txt`. The session leases 13 pages, the first
    being page 63 of 64; all are returned on release.
  - Logits for 40 positions: worst relative error against the CPU decoder
    8.8e-6 (asserted < 1e-4).
  - Q8_0 weights are `Unsupported`; foreign caches, an exhausted page pool,
    an out-of-vocabulary token and a leaked session are typed or counted.
- `tests/kernels.rs` (6), each against `vesper-tensor`'s scalar CPU kernel:
  - `matmul` at 7 shapes from 1×1×1 to 16×130×300 including non-multiples of
    the workgroup, plain and accumulating (tolerance 1e-6 relative).
  - `rms_norm` at 7 shapes, up to 4 rows of 1000 (1e-5).
  - `rope` at head widths 2, 8, 64, 128 and positions 0 to 2047 (2e-5; worst
    measured 1.1e-7).
  - `swiglu` (1e-6).
  - Paged attention: the same tokens are appended to a CPU F32 `KvPool` and,
    through `kv_write`, to device pages under a permuted page table, for 5
    head configurations (GQA ratios 1, 2, 4; head widths 8 to 130) at 1, 15,
    16, 17, 32, 33, 100 and 300 tokens, compared with both
    `AttentionMode::TwoPass` and `AttentionMode::Online` (1e-5; worst
    measured 1.7e-7).
  - Typed rejection of short buffers, bad epsilon, bad RoPE shapes, a bad GQA
    ratio, an empty or over-long sequence and a short page table; nothing is
    recorded for a rejected call.
- `tests/broadphase.rs` (6):
  - 13 random scenes from 2 to 4,096 boxes (0 to 7,353 pairs) give **exactly**
    `Lbvh::overlapping_pairs`' pair list, and for n ≤ 2048 the CPU LBVH also
    equals `brute_force_pairs`.
  - Degenerate scenes: 300 identical boxes (44,850 pairs, all sort keys
    equal), boxes that only touch, `-0`/`+0` and subnormal bounds, zero-extent
    boxes, ±3e38 coordinates, 0 and 1 boxes.
  - 3,000 sphere boxes made with `Aabb::around`, as the world broadphase
    makes them.
  - Determinism: the device's raw output order, its sorted keys and the
    final pairs are identical over 3 runs on each of 2 broadphases and a
    second `Context`; the device sort equals the host's sorted `(key, index)`
    list.
  - `order_key` preserves the f32 order over 10,000 random finite values and
    the edge values.
  - NaN, infinite and inverted boxes are `LbvhError::InvalidBox` with the
    same index as the CPU; too many boxes is `CapacityExceeded`; too many
    pairs is `PairOverflow` with `out` cleared; the broadphase is reusable
    afterwards.
- `tests/runtime.rs` (8):
  - Every bundled kernel compiles to SPIR-V (runs everywhere, including the
    Bazel, macOS and Windows jobs, which have no device).
  - No workgroup-memory array carries an `ArrayStride` decoration (see
    "Decisions"); buffer arrays keep theirs.
  - Malformed WGSL, a missing or non-compute entry point, a uniform binding,
    a binding gap, group 1, an ill-typed store and oversized source are
    typed `ShaderError`s.
  - Two submissions chained without an intermediate wait complete in
    timeline order and the second sees the first's writes.
  - A dropped submission is counted and still completes.
  - A submission leaked with `mem::forget` keeps its buffers and kernel
    alive; host reads wait for it; dropping the handles frees nothing in use.
  - A kernel that indexes far past its buffer writes the clamped last
    element (naga `Restrict`), not other memory.
  - Zero-sized and over-limit buffers, out-of-range reads and writes, a
    kernel whose bindings or immediates differ from the request, a buffer of
    another context, wrong buffer and immediate counts, and zero or
    over-limit workgroup counts are typed errors.

The whole suite was also run with `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`
(Ubuntu `vulkan-validationlayers` 1.3.275; `VK_LOADER_DEBUG=layer` confirmed
the layer was inserted): no validation messages. The workflow repeats that
run and fails on any `Validation Error`.

Commands (private target directories, `CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`):

```sh
apt-get install mesa-vulkan-drivers vulkan-tools vulkan-validationlayers   # Mesa 25.2.8
VESPER_GPU_REQUIRE=1 cargo test -p vesper-gpu-compute                       # 23 passed
VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation VESPER_GPU_REQUIRE=1 \
  cargo test -p vesper-gpu-compute -- --test-threads=1                     # 23 passed, 0 validation messages
VK_ICD_FILENAMES=/nonexistent.json cargo test -p vesper-gpu-compute        # device tests report unavailable; compile tests pass
VK_ICD_FILENAMES=/nonexistent.json VESPER_GPU_REQUIRE=1 cargo test ...     # fails, as intended
cargo fmt --all -- --check                                                  # pass
cargo +1.100.0 clippy -p vesper-gpu-compute --locked --all-targets -- -D warnings   # pass
cargo test --workspace --locked --no-fail-fast                              # this crate passes; failures only in the root
                                                                            # Lean §42 acceptance tests (no Lean binary here) and
                                                                            # vesper-loom-build build/cli (host-specific, as in the
                                                                            # wave-six Tensor report); none touches this crate
```

## Decisions

- **Dependencies.** `ash` `=0.38.0` (`loaded` feature: the loader is opened
  at run time, so the crate builds under hermetic Bazel and on macOS and
  Windows without Vulkan headers or libraries) and `naga` `=30.0.2`
  (`wgsl-in`, `spv-out`), both the latest releases; lane 32 (Ignis Vulkan)
  had not merged, so there was no earlier pin to follow. `bytemuck`
  `=1.25.2` is already in `Cargo.lock`. No shaderc or glslang dependency.
- **Bazel.** The crate's targets are not tagged `manual`: everything builds
  hermetically, the naga compile tests run there, and the device tests
  report the device unavailable. The new workflow runs the device tests on
  every PR that touches the crate or the CPU references it compares against.
- **Exactness and tolerances.** The matmul, SwiGLU and K/V kernels follow the
  CPU summation order, but SPIR-V allows the driver to contract `a*b + c`, so
  they are held to 1e-6, not bit equality. RMSNorm and softmax reduce in a
  tree. RoPE takes its frequencies from the host by the CPU formula: with
  lavapipe's `pow`, one ulp of frequency at position 2047 moved the result by
  8e-5. The broadphase needs no tolerance because it does no float
  arithmetic on the device.
- **Sort and sweep, not LBVH, on the device.** The prompt allows either.
  WGSL has no 64-bit integers, so the 63-bit Morton LBVH would need emulated
  64-bit keys, and its result would only match the CPU if the f32 centroid
  quantisation also matched bit for bit (WGSL division is not correctly
  rounded). Sort and sweep yields the same pair set with integer-only
  comparisons. The CPU LBVH remains the reference it is compared with.
- **Workgroup layout fix-up.** naga 30 decorates every array type with
  `ArrayStride`, including arrays in workgroup memory, which Vulkan forbids
  without `VK_KHR_workgroup_memory_explicit_layout`. The Ubuntu 24.04
  validation layer (1.3.275) does not check this; the newer one on the CI
  runner reported `VUID-StandaloneSpirv-None-10684` for the RMSNorm,
  attention and scan kernels. `shader::strip_workgroup_layout` deletes
  those decorations after naga emits the module, and
  refuses a module whose array type is shared between workgroup memory and
  a buffer. Lane 32 uses the same naga and may need the same pass.
- **Memory.** All buffers are host-visible and coherent. On lavapipe all
  memory is; on a discrete GPU this would be correct but slow.
- **Synchronisation.** One queue; a full memory barrier after each dispatch;
  each submission waits for the previous timeline value. This is simple and
  correct, not the minimal barrier set.

## Remaining gaps

- Real GPU execution (NVIDIA, AMD, Intel, Apple through MoltenVK, DirectX 12,
  Metal) is deferred; nothing here was run on one.
- No quantized GPU kernels (Q8_0, Q4_0 are `Unsupported`), no BF16 KV on the
  device, no online (FlashDecoding) attention schedule, no batched prefill,
  no device-local memory or staging, no subgroup operations, no workgroup
  tiling in matmul, and no argmax on the device (logits are read back).
- The attention kernel is limited to 2,048 tokens and head width 1,024; the
  broadphase to 65,536 boxes and 2^24 pairs. Larger inputs are typed
  `Limit` errors.
- The GPU broadphase is not wired into `vesper_kinetic::World`
  (`BroadphaseMode` is in lane 5's crate); callers use `GpuBroadphase`
  directly. Kinetic's own determinism rules (DetFloat) do not cover device
  code; the broadphase is exact because it uses no device float
  arithmetic, and the tensor kernels make no bit-reproducibility claim.
- No Vesper source syntax or compiler lowering targets these kernels; they
  are a host library. No Lean model was written for this lane.
