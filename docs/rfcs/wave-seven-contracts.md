# Seventh-wave integration contracts

Baseline: the `main` commit named in the [wave-seven prompt index](../agent-prompts/wave-seven/README.md)
or a descendant. This is an ownership and interface agreement so that thirty-three
lanes can run with as little overlap as possible. It is not a soundness theorem.

Wave six added depth to every started section and wired the wave-five crates
into the CLI and native backend. Wave seven finishes the host-library gaps the
wave-six reports recorded (lanes 1-22) and starts the compiler work those
reports kept deferring: native-backend depth, three new code-generation
targets, and three source-language features (lanes 23-30). Lanes 31-33 run
Aura, Ignis and GPU compute on software stand-ins (Xvfb, Mesa lavapipe), as the
owner chose on 2026-11-14. The
[section status review](wave-seven-review.md) maps each remaining gap to a lane
or to a later wave.

## Work held by others (do not touch)

- Codex agents: §16.4 attribute and item macros (no lane adds attribute or
  item macro syntax), and §42: the resource model (#322, PR #323:
  `lean/ResourceModel42*`, `docs/rfcs/resource-model42-*`), rewrite witnesses
  and optimizer certificates (`src/optimize*`, `tests/rewrite_*`,
  `scripts/check-rewrite-witness.py`, `docs/rfcs/rewrite42-*` and their Lean
  files). Re-check the open board before starting.
- Excluded by the owner: the eBPF linear builder API (PR #165).
- Metronome (`crates/vesper-bench*`, `src/bench_commands.rs`) until PR #283 merges.

## Ownership of shared files

Lanes 1-22 own existing crates outright (see the index). Files several lanes
could reasonably touch are assigned like this:

| Path | Owner in wave seven | Narrow hunks allowed |
| --- | --- | --- |
| `src/native.rs`, `src/native_batch.rs`, `src/native_session.rs`, `src/native_coverage.rs`, `src/native_verifiers.rs` | Lane 23 | Lane 24 (target selection), 27 (freestanding entry point), 28 (one vector dispatch arm) |
| `src/check.rs` | Lane 29 | Lane 28 (hook calls into `src/vector*`) |
| `src/capability_effects.rs` | Lane 30 | Lane 28 may register `Isa[P]` through one additive hook |
| `src/ir.rs`, the evaluator in `src/runtime*` | Lane 28 (additive vector operations) | Lane 29 only if new operations are unavoidable |
| `crates/vesper-syntax/**` | No owner; additive only | Lanes 28 and 29, each in its own commit |
| `src/main.rs` | Lane 14 (help text) | Lanes 25, 26 (one routing hunk each for `vesper build --target`) |
| `src/lsp.rs`, `src/lsp/**` | Lane 10 | — |
| `src/doc_commands.rs`, `src/doctest*.rs`, `src/tooling/doc*`, `README.md` doc and build lines | Lane 14 | — |
| `crates/vesper-fiber-abi/**` | Lane 3 (additive API) | — |
| `crates/vesper-pulse/**` | Lane 2 | — |
| `crates/vesper-warp/**`, `crates/vesper-warp-tls/**` | Lane 7; lanes 8 and 16 depend on them read-only | — |
| `crates/vesper-ffi/**`, `crates/vesper-migrate/**` | Lane 6 | — |
| `crates/vesper-ebpf/**` | Lane 17; lane 26 depends on it read-only | — |
| `crates/vesper-silicon/**` | Lane 18; lane 27 depends on it read-only | — |
| `crates/vesper-nexus/**` | Lane 22; lane 25 depends on it read-only | — |
| `crates/vesper-quarry/**` | Lane 11; lane 12 depends on it read-only | — |
| `.github/workflows/platforms.yml` | Lane 24 (Windows host-native step only) | — |
| NEW `.github/workflows/aura-gtk.yml`, `ignis-vulkan.yml`, `gpu-compute.yml` | Lanes 31, 32, 33 respectively | — |
| `crates/vesper-aura/**`, `crates/vesper-ignis/**`, `crates/vesper-tensor/**`, `crates/vesper-kinetic/**` | Read-only for lanes 31-33 (Tensor and Kinetic are owned by lanes 20 and 5) | — |
| Root `Cargo.toml` dependency lines, `Cargo.lock` | Each lane commits its own minimal patch in its own commit | — |
| `tools/bazel/**`, root `BUILD.bazel`, other CI workflows | Not edited | — |

A lane that needs a hunk in a file it does not own keeps it additive, puts it
in its own commit, explains it in the PR, and merges `origin/main` right before
merging. Where two lanes touch the same file, the later one rebases onto the
earlier one; it never reverts the other's hunk.

## API stability between concurrent lanes

- Crates another concurrent lane depends on (`vesper-warp`, `vesper-warp-tls`,
  `vesper-fiber-abi`, `vesper-ebpf`, `vesper-silicon`, `vesper-nexus`,
  `vesper-quarry`, `vesper-simpulse`, `vesper-alloc`) change their public API
  additively during this wave. A breaking change needs a deprecation shim kept
  until the wave ends, and a line in the lane's report.
- Dependants build only on APIs already on `main`, never on an unmerged lane.
- No library crate depends on the root `vesper` package.
- Fiber ABI v2 and the native frame layout are unchanged; lane 3's stack-image
  API reads them, and lane 23's capability-passing ABI is a new calling
  convention for the functions it names, versioned in the object metadata.

## IR additions (lane 28) and the backends (lanes 25-27)

Lane 28 adds vector types and operations to the Place IR additively, with
evaluator semantics for each. Every backend (native, Wasm, eBPF, freestanding)
matches IR operations with a wildcard arm that returns a typed
"unsupported operation" error, so a new operation never miscompiles a backend
that does not know it. Lanes 25-27 lower from the checked Place IR and reuse
`src/native.rs` only through its public entry points.

## Root and lockfile

New dependencies are pinned exactly and reuse versions already in `Cargo.lock`
where possible. Only lanes 31-33 add workspace members (`vesper-aura-gtk`,
`vesper-ignis-vulkan`, `vesper-gpu-compute`): each registers its crate in
`members` and `default-members` before `experiments/native-pulse` and generates
its `BUILD.bazel` with `tools/bazel/sync_build_files.py`. If a crate's system
libraries (GTK 4, Vulkan loader) cannot build under hermetic Bazel, its Bazel
targets are tagged `manual` with a comment naming the lane's own workflow, and
that workflow runs the tests on every PR touching the crate. Lanes 32 and 33
use the same `ash` and `naga` versions; the second to merge adopts the first's. Because many lanes will touch
`Cargo.lock`, merges that change it are serialized: a lane merges `origin/main`
into its branch right before merging, regenerates the lockfile with cargo
instead of hand-merging it, and waits for CI on that head. When several green
PRs are waiting, stack them as in the wave-four merge procedure (each branch
contains the previous one) so CI runs once per PR.

## Shared vocabulary (carried from wave six)

- **Time** is `u64` nanoseconds on a caller-supplied virtual or monotonic clock.
- **Backend verifiers** take `cranelift_codegen::ir::Function` (Cranelift
  0.125) and return `Vec<VerifierFinding>`. Lane 23 adds a source span to each
  finding additively (`Option<Span>`); existing fields keep their meaning.
- **Call graphs** use `CallGraph { functions: Vec<FnNode>, calls: Vec<(FnId, FnId)> }`.
- **Linear obligations** in host libraries are non-Clone, non-Copy
  `#[must_use]` owners with documented consuming exits; dropping one is
  detected. Lanes 28-30 change source rules in the checker and say exactly
  which ones.

## Evidence and claims

- "0 Alloc" claims need a counting global allocator test over the warmed-up
  steady state.
- Hardware and kernel features are reported unavailable unless exercised. At
  planning time the cloud host had no `/dev/kvm`, no `intel_pt`, no hardware
  `cpu` PMU, no GPU, `perf_event_paranoid = 2`, `io_uring_setup` denied and
  `unprivileged_userfaultfd = 0`; it does have PKU. Record which CI job, if any,
  exercised a feature.
- Real vendor samples (lane 4) are claimed only when a test verifies a real
  published sample end to end, with its provenance and licence recorded.
- Interop claims (lanes 1, 7, 8, 16) name the peer implementation and version.
- Lean files compile with Lean 4.24.0, contain no `sorry`, `admit` or new
  axioms, run from the owning crate's tests under `VESPER_LEAN`, and under
  Bazel declare `extra_test_data = ["//:lean"]`.
- Backend claims (lanes 24-28) are differential: the same program gives the
  same result on the evaluator and on the target.

## Unsafe code allowed

| Lane | Allowed gated module |
| --- | --- |
| 2 | the existing io_uring module; new epoll, kqueue and IOCP modules |
| 3 | the fiber switch modules, stack mapping and image copy in `vesper-fiber-abi`; `perf_event_open` capture in Chrono |
| 5, 9, 20, 21 | `std::arch` SIMD with a scalar reference (20 also mmap for weights) |
| 13 | userfaultfd, memfd, seccomp and KVM ioctls (Linux) |
| 17 | the existing `bpf()` module |
| 32, 33 | Vulkan calls through `ash`, confined to one backend/runtime module |
| 23, 24, 27, 28 | none beyond what `src/native*.rs` already has (JIT and object emission) |
| others | none |

## Sequencing

- All thirty-three lanes start now. Lanes 31-33 merge one at a time (root registration).
- Lane 24 merges after lane 23 if both changed `src/native.rs`; lanes 27 and 28
  merge `origin/main` first for the same reason, as do lanes 28 and 29 for
  `src/check.rs` and `crates/vesper-syntax/**`.
- A Metronome follow-up is planned after PR #283 merges.
