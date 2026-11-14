# Eighth-wave integration contracts

Baseline: the `main` commit named in the [wave-eight prompt index](../agent-prompts/wave-eight/README.md)
or a descendant. This is an ownership and interface agreement so that
twenty-seven lanes can run alongside the open wave-seven PRs with as little
overlap as possible. It is not a soundness theorem.

The [section status review](wave-eight-review.md) maps each remaining gap to a
lane or to a later wave.

## Work held by others (do not touch)

- Codex agents: §16.4 attribute and item macros (no lane adds attribute or
  item macro syntax); the §39 formatter (#170, #214); and these §42 paths: the
  resource model (`lean/ResourceModel42*`, `docs/rfcs/resource-model42-*`),
  rewrite witnesses and optimizer certificates (`src/optimize*`,
  `tests/rewrite_*`, `scripts/check-rewrite-witness.py`,
  `docs/rfcs/rewrite42-*`, `lean/RewriteWitness*`), and the Lean4Lean replay
  (`lean/Lean4Lean*`, `tests/lean4lean*`, issues #402-#448). Wave-eight §42
  lanes (1-4) build beside these and use them only through what is on `main`.
  Re-check the open board before starting.
- Excluded by the owner: the eBPF linear builder API (PR #165).
- Open wave-seven PRs keep their paths until they merge: #384 (`src/native*.rs`),
  #379 (`src/vector*`, `src/check.rs`, `src/ir.rs`, `src/runtime.rs`,
  `src/wasm_backend`), #375 (`src/capability_effects.rs`, `src/check.rs`),
  #373, #369, #360, and the crate PRs #389, #376, #381, #383, #393, #368, #365,
  #362, #441, #434.

## Ownership of shared files

| Path | Owner in wave eight | Narrow hunks allowed |
| --- | --- | --- |
| `src/native.rs`, `src/native_*.rs` | Wave-seven lane 23 (PR #384) until it merges, then nobody | Lanes 8 (region payloads, runtime link) and 20 (foreign calls, trampolines), each after #384 |
| `src/check.rs` | No owner | Hook calls from lanes 10, 13 and 20, each in its own commit, after #379 and #375 |
| `src/main.rs` | No owner | Lane 2 (one routing hunk for `--codegen verified`) |
| `crates/vesper-syntax/**` | No owner; additive only | Lane 10 if unavoidable |
| `src/lean.rs`, `src/lean_ir.rs`, `src/lean_nodes.rs`, `crates/vesper-lean-syntax/**` | Lane 12 | — |
| `src/wasm_backend*` | Lane 14 (after #379) | — |
| `src/regions*` | Lane 8 | — |
| `crates/vesper-alloc/**` | Lane 7 (additive API; lane 8 reads it) | — |
| `crates/vesper-doc/**` | Lane 17 | Lane 16 (preflight bound, own commit) |
| `crates/vesper-simd/**`, new SVE/RVV modules in `crates/vesper-serde` | Lane 9 | — |
| `lean/Metatheory/**`, `lean/MetatheoryFull.lean` | Lane 5 | Lanes 4 and 6 read only |
| `crates/vesper-fiber-abi/**`, `crates/vesper-chrono/**` | Lane 19 (after #441) | — |
| `crates/vesper-pulse/**` | Lane 21 | — |
| `crates/vesper-silicon/**` | Lane 26 (additive; #373 depends on it) | — |
| Existing crates named in a lane's boundary | That lane | — |
| NEW `.github/workflows/scalable-vectors.yml` | Lane 9 | — |
| `.github/workflows/aura-gtk.yml` | Lane 25 | — |
| Root `Cargo.toml` dependency lines, `Cargo.lock` | Each lane commits its own minimal patch in its own commit | — |
| `tools/bazel/**`, root `BUILD.bazel`, other CI workflows | Not edited | — |

A lane that needs a hunk in a file it does not own keeps it additive, puts it
in its own commit, explains it in the PR, and merges `origin/main` right before
merging. Where two lanes touch the same file, the later one rebases onto the
earlier one; it never reverts the other's hunk.

## API stability between concurrent lanes

- Crates another concurrent lane depends on (`vesper-alloc`, `vesper-silicon`,
  `vesper-quarry`, `vesper-pulse`, `vesper-fiber-abi`, `vesper-simd`,
  `vesper-ffi`, `vesper-beacon`, `vesper-tensor`, `vesper-resonance`) change their
  public API additively during this wave. A breaking change needs a
  deprecation shim kept until the wave ends, and a line in the lane's report.
- Dependants build only on APIs already on `main`, never on an unmerged lane.
- No library crate depends on the root `vesper` package.
- Fiber ABI v2 and the native frame layout are unchanged.

## §42 lanes

- Lane 1 owns the bootstrap lock format (`bootstrap/stage3_ddc.lock`, §42.2)
  and the fixed-point gate. It does not reimplement the rewrite-certificate
  checker; it calls the one on `main` or reports the hook as not wired.
- Lane 2's verified code generator is a separate backend selected by
  `vesper build --codegen verified`; it does not replace Cranelift and does
  not change `src/native*.rs`.
- Lane 3's Vesper-source compiler lives under `packages/vesper-compiler/`; it
  is compiled by the Rust seed and compared with `vesper-syntax`. Missing
  language features are reported, not worked around in the seed.
- Lane 4's reference evaluator is new Lean under `lean/ReferenceEval*`; it
  imports lane 5's metatheory only as it is on `main`.

## Root and lockfile

New dependencies are pinned exactly and reuse versions already in `Cargo.lock`
where possible. Only lanes 1 and 11 add workspace members: each registers its
crate in `members` and `default-members` before `experiments/native-pulse` and
generates its `BUILD.bazel` with `tools/bazel/sync_build_files.py`. Merges that
change `Cargo.lock` are serialized: a lane merges `origin/main` into its branch
right before merging, regenerates the lockfile with cargo instead of
hand-merging it, and waits for CI on that head.

## Evidence and claims

Unchanged from [wave seven](wave-seven-contracts.md#evidence-and-claims), plus:

- QEMU user-mode runs (lane 9, and AArch64 in lane 2 when no native host is
  available) validate results, never performance.
- Differential claims name every engine compared (evaluator, Cranelift JIT, AOT
  executable, Wasmtime, verified codegen, Lean reference).
- Interop claims name the peer and version (Kafka broker in lane 18, AT-SPI in
  lane 25).

## Unsafe code allowed

| Lane | Allowed gated module |
| --- | --- |
| 7 | the existing arena/slab modules in `vesper-alloc` |
| 9 | `std::arch` and `asm!` SIMD with a scalar reference, in the new SVE/RVV modules |
| 15 | the existing PMU modules in `vesper-bench-pmu` |
| 19 | fiber switch, stack mapping and image modules; new write-tracking modules |
| 21 | the existing driver modules; a new AFD module on Windows |
| 23 | the existing `std::arch` SIMD modules with a scalar reference |
| 27 | the existing userfaultfd, memfd and seccomp modules |
| 8, 20 | none beyond what `src/native*.rs` already has |
| others | none |

## Sequencing

- Lanes 18, 19 and 23 start when the PR they wait on merges (#381, #441, #434).
- Lanes 8 and 20 merge after #384; lanes 10, 13 and 20 merge their
  `src/check.rs` hooks after #379 and #375; lane 14 merges after #379.
- Lanes 1 and 11 merge one at a time (root registration).
- Lane 17 merges after lane 16 if both changed `crates/vesper-doc`.
