# Ninth-wave integration contracts

Baseline: the `main` commit named in the [wave-nine prompt index](../agent-prompts/wave-nine/README.md)
(`37f7b8b`) or a descendant. This is an ownership and interface agreement so
that thirty-four lanes can run alongside Codex's open work with as little
overlap as possible. It is not a soundness theorem.

The [section status review](wave-nine-review.md) maps each remaining gap to a
lane or to a later wave.

## Work held by others (do not touch)

- Codex agents: §16.4 attribute and item macros (no lane adds attribute or
  item macro syntax); the §39 formatter (#170, #214); the §42 resource model
  (`lean/ResourceModel42*`, `docs/rfcs/resource-model42-*`), rewrite
  witnesses and optimizer certificates (`src/optimize*`, `tests/rewrite_*`,
  `scripts/check-rewrite-witness.py`, `docs/rfcs/rewrite42-*`,
  `lean/RewriteWitness*`) and the Lean4Lean replay (`lean/Lean4Lean*`,
  `tests/lean4lean*`, issues #402-#560 and their PRs).
- Codex CI repairs claimed on 2026-11-14 and 2026-11-15, each holding exact
  files until its PR merges:

  | Issue | Held paths |
  | --- | --- |
  | #567 | `src/check.rs` (integer facts across `if` joins, for #497) |
  | #568 | `lean/ReferenceEval.lean`, `lean/ReferenceEval/Soundness.lean` |
  | #569 | `tests/checked_arithmetic.rs`, `tests/fixtures/lockstep/**` |
  | #571 | `buildbuddy.yaml` |
  | #572 | `crates/vesper-fiber-abi/tests/unwind.rs` |
  | #573 | `crates/vesper-pulse/src/pool.rs`, `crates/vesper-pulse/tests/pool.rs` |
  | #574 | root `BUILD.bazel` |
  | #575 | `src/lsp/lens_tests.rs` |
  | #576 | `src/native.rs`, `src/native_batch.rs`, `src/native_session.rs` |

- Wave-eight lane 11 (egglog fusion, #460, PR #487): `crates/vesper-fusion/**`,
  `lean/FusionRules.lean`.
- Excluded by the owner: the eBPF linear builder API (PR #165).
- The owner's website (`website/**`).

Re-check the open board before starting: new Codex claims may appear.

## Ownership of shared files

| Path | Owner in wave nine | Narrow hunks allowed |
| --- | --- | --- |
| `src/main.rs` | No owner | Routing hunks from lanes 1 (`highlight`), 2 (`shot`), 15 (`build --target`), 34 (`.vsys` imports), each in its own commit |
| `src/check.rs` | Codex (#567) until its PR merges, then nobody | Hooks from lanes 10 and 11, each after #567 |
| `src/native.rs` | Codex (#576) until its PR merges, then nobody | Lane 9 (handler ABI), after #576 |
| `src/native_session.rs` | Codex (#576) until its PR merges, then nobody | Lane 8 (link step), after #576 |
| `src/native_windows.rs` | Lane 8 | — |
| `src/native_vector.rs`, `src/vector*` | Lane 10 | — |
| `src/native_freestanding.rs`, `src/freestanding/**` | Lane 15 | — |
| `crates/vesper-syntax/**` | No owner; additive only | Lanes 10, 11 (`[r: Region]` parameters) and 12 (`wire` in the item schema), each in its own commit; the later one rebases |
| `crates/vesper-highlight/**` | Lane 1 | Lane 2 owns `src/shot/**` and its dependency lines, after lane 1 merges |
| `packages/vesper-compiler/**` | Lane 3 (existing files, `parse_*`, `syntax_*`) | Lane 4 owns new `ast_*`, `resolve_*`, `check_*` files |
| `bootstrap/**`, `crates/vesper-codegen-verified/**` | Lane 5 | — |
| `src/verified_codegen*` | Lane 6 | — |
| `lean/Metatheory/**`, `lean/MetatheoryFull.lean` | Lane 7 | — |
| `crates/vesper-fiber-abi/**` | Lane 21 (except `tests/unwind.rs`, Codex #572) | Lane 8 reads it |
| `crates/vesper-pulse/**` | Lane 22, after #573 | Lane 8 reads it; lane 23 uses its pollable fd after lane 22 merges |
| `crates/vesper-simd/**`, `crates/vesper-serde/**` | Lane 31 | Lane 10 reads `vesper-simd` |
| `crates/vesper-alloc/**` | Lane 33 (additive API) | Lane 11 reads it |
| `crates/vesper-gpu-compute/**` | Lane 27 (keeps the broadphase API stable) | Lane 25 calls `GpuBroadphase` from `vesper-kinetic` |
| `crates/vesper-lsp/**`, `src/lsp*` (except `src/lsp/lens_tests.rs`) | Lane 17 | — |
| Existing crates named in a lane's boundary | That lane | — |
| `.github/workflows/{flux-kafka,aura-gtk,gpu-compute,ignis-vulkan,scalable-vectors}.yml` | Lanes 20, 23, 27, 28, 31 | — |
| NEW `.github/workflows/aether-qemu.yml` | Lane 29 | — |
| Root `Cargo.toml` dependency lines, `Cargo.lock` | Each lane commits its own minimal patch in its own commit | — |
| `tools/bazel/**`, root `BUILD.bazel`, other CI workflows | Not edited (root `BUILD.bazel` is Codex's until #574 merges) | — |

A lane that needs a hunk in a file it does not own keeps it additive, puts it
in its own commit, explains it in the PR, and merges `origin/main` right before
merging. Where two lanes touch the same file, the later one rebases onto the
earlier one; it never reverts the other's hunk.

## API stability between concurrent lanes

- Crates another concurrent lane depends on (`vesper-highlight`,
  `vesper-alloc`, `vesper-pulse`, `vesper-fiber-abi`, `vesper-simd`,
  `vesper-gpu-compute`, `vesper-ffi`, `vesper-syntax`, `vesper-doc`) change their
  public API additively during this wave. A breaking change needs a
  deprecation shim kept until the wave ends, and a line in the lane's report.
- Dependants build only on APIs already on `main`, never on an unmerged lane.
- No library crate depends on the root `vesper` package.
- Fiber ABI v2 and the native frame layout are unchanged. Lane 9's handler ABI
  is a new, versioned, additive contract.

## §43 Chroma lanes

- Lane 1 owns the crate layout of `crates/vesper-highlight`, the
  `VesperTokenClass` numbering (the `repr(u16)` values in §43.5 are frozen
  once merged: the 16-byte `ChromaTokenSpanWire` record shares them with Lens
  and the wasm module), the palettes and the contrast gates.
- Lane 2 adds `src/shot/**` behind lane 1's public API and does not change the
  token classes or palettes; it may add a feature flag for its renderer
  dependencies so the wasm build stays small.
- Lane 17 consumes the token classes for LSP semantic tokens after lane 1
  merges.
- `@shot` and the `//^?` / `//^!` callouts are directives Chroma reads from
  source text. They are not attribute syntax and are not compiled.
- No lane edits `website/**`.

## §42 lanes

- Lane 3 produces a tree from the self-hosted parser; lane 4 consumes it. Until
  lane 3 merges, lane 4 builds on the parser on `main` through an adapter in its
  own files.
- Lane 5 owns the bootstrap lock format and extends it with a Stage 1 lock; it
  calls lane 6's backend only as it is on `main`.
- Lane 6's verified code generator stays a separate backend selected by
  `vesper build --codegen verified`; it does not change `src/native*.rs`.
- Rewrite-certificate checking stays Codex's; lane 5 wires it only through an
  API already on `main`.

## Root and lockfile

New dependencies are pinned exactly and reuse versions already in `Cargo.lock`
where possible. Only lanes 1 and 8 add workspace members: each registers its
crate in `members` and `default-members` before `experiments/native-pulse` and
generates its `BUILD.bazel` with `tools/bazel/sync_build_files.py`. Merges that
change `Cargo.lock` are serialized: a lane merges `origin/main` into its branch
right before merging, regenerates the lockfile with cargo instead of
hand-merging it, and waits for CI on that head.

## Evidence and claims

Unchanged from [wave eight](wave-eight-contracts.md#evidence-and-claims), plus:

- Chroma size and latency targets (26 KiB wasm module, 0.2 ms Tier 0, 4 ms
  card render, 1.2 KiB CSS) are reported as measured, with the host; a missed
  target is reported, not hidden.
- Contrast claims name the formula (WCAG 2.x relative luminance, APCA Lc) and
  show the computed values.
- QEMU runs (lanes 6, 15, 29, 30, 31) validate results, never performance.
  Lane 29's guest kernel and QEMU versions are named.
- Interop claims name the peer and version (Kafka broker in lane 20, the
  OpenTelemetry Collector in lane 26, `wtransport`, `h3` and Chromium in
  lane 32).

## Unsafe code allowed

| Lane | Allowed gated module |
| --- | --- |
| 1 | the wasm32 C ABI shim, in one gated module |
| 8 | `vesper-pulse-rt`'s FFI entry and switch shims |
| 21 | fiber switch, stack mapping, image, unwinding and write-tracking modules |
| 22 | the existing driver modules and the new deque glue |
| 24 | the aligned-buffer module for `O_DIRECT` |
| 25, 27 | the existing `std::arch` SIMD modules with a scalar reference |
| 28 | the existing Vulkan FFI modules |
| 29 | the existing userfaultfd, memfd and seccomp modules, plus the vhost-user shared-memory mapping |
| 31 | `std::arch` and `asm!` SIMD with a scalar reference, in the SVE/RVV/AVX modules |
| 33 | the existing arena/slab modules |
| 9, 15, 34 | none beyond what the native backend and freestanding stubs already have |
| others | none |

## Sequencing

- Lane 2 starts after lane 1 merges; lane 17's semantic tokens and lane 23's
  fd fusion wait on lanes 1 and 22.
- Lane 22 starts after Codex's PR for #573 merges.
- Lanes 8 and 9 merge their hunks in `src/native_session.rs` and `src/native.rs`
  after Codex's PR for #576; lanes 10 and 11 merge `src/check.rs` hooks after
  Codex's change for #567.
- Lanes 1 and 8 merge one at a time (root registration).
- Lanes 10, 11 and 12 serialize their `crates/vesper-syntax` commits.
