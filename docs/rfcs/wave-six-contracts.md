# Sixth-wave integration contracts

Baseline: the `main` commit named in the [wave-six prompt index](../agent-prompts/wave-six/README.md)
or a descendant. This is an ownership and interface agreement so that integration
B and twenty-four lanes can run with as little overlap as possible. It freezes
nothing new in the compiler, and it is not a soundness theorem.

Wave five gave every design section a tested host library. Wave six adds depth
to those libraries, wires them into the CLI and compiler (integration B), and
starts the source-language side with capability effects (lane 15). The
[section status review](wave-six-review.md) maps each remaining gap to a lane or
to a later wave.

## Work held by others (do not touch)

- Codex agents: §42 rewrite witnesses and optimizer certificates, issues
  #230-#242 and PR #238 (`src/optimize*`, `tests/rewrite_*`,
  `scripts/check-rewrite-witness.py`, `docs/rfcs/rewrite42-*` and their Lean
  files). Re-check the open board before starting.
- Excluded by the owner: the eBPF linear builder API (PR #165).

## Ownership of shared files

Most lanes own one existing crate outright (see the index). Files several
steps could reasonably touch are assigned like this:

| Path | Owner in wave six |
| --- | --- |
| `src/main.rs`, `src/lib.rs` module lines, new `src/*_commands.rs` for fuzz/refute/migrate/replay/ebpf | Integration B |
| `src/native.rs` | Integration B (verifier hook), then lane 23 (coverage hook) after B merges. No other edits. |
| `src/check.rs` | Lane 15 (narrow hook calls into new `src/capability_effects*`) |
| `src/lsp.rs`, `src/lsp/**` | Lane 8 |
| `src/doc_commands.rs`, `src/doctest*.rs`, `src/tooling/doc*`, `src/lint_commands.rs` default paths | Lane 14 |
| `src/bench_commands.rs` | Lane 13 |
| `README.md` | Lane 13 (`vesper bench` lines) and lane 14 (`vesper lint`, `vesper test --doc` lines) only |
| `crates/vesper-warp/**`, `crates/vesper-warp-tls/**` | Lane 5; lanes 6 and 17 depend on them read-only |
| `crates/vesper-fiber-abi/**`, `crates/vesper-pulse/**` | Lane 22, Windows modules and `cfg` dispatch only; lane 2 depends on them read-only |
| `crates/vesper-ffi/**`, `crates/vesper-migrate/**` | Lane 1; integration B uses `vesper-migrate` read-only |
| Root `Cargo.toml` dependency lines, `Cargo.lock` | Each step commits its own minimal patch in its own commit |
| CI workflows, `tools/bazel/**`, root `BUILD.bazel` | Not edited, except integration B may add root test data |

## API stability between concurrent steps

- Crates that another concurrent step depends on (`vesper-warp`,
  `vesper-warp-tls`, `vesper-migrate`, `vesper-crucible`, `vesper-chrono`,
  `vesper-ebpf`, `vesper-silicon`, `vesper-vault`, `vesper-resonance`,
  `vesper-kinetic`, `vesper-aegis`) change their public API additively during
  this wave. A breaking change needs a deprecation shim kept until the wave
  ends, and a line in the lane's report.
- Dependants build only on APIs already on `main`, never on an unmerged lane.
- No library crate depends on the root `vesper` package.

## Root and lockfile

New dependencies are pinned exactly and reuse versions already in
`Cargo.lock` where possible. Because many lanes will touch `Cargo.lock`, a lane
merges `origin/main` into its branch right before merging, regenerates the
lockfile with cargo instead of hand-merging it, and waits for CI on that head.
Only lane 6 adds a crate (`vesper-prism-h3`); it registers it in `members` and
`default-members` before `experiments/native-pulse` and generates its
`BUILD.bazel` with `tools/bazel/sync_build_files.py`.

## Shared vocabulary (carried from wave five)

- **Time** is `u64` nanoseconds on a caller-supplied virtual or monotonic clock.
- **Backend verifiers** take `cranelift_codegen::ir::Function` (Cranelift
  0.125) and return `Vec<VerifierFinding>` with
  `VerifierFinding { function, inst, rule, message }`. Integration B adds the
  hook that runs them on the functions `src/native.rs` emits; lane 24 then
  builds their policies from lane 15's `capability_policies`.
- **Call graphs** use `CallGraph { functions: Vec<FnNode>, calls: Vec<(FnId, FnId)> }`.
- **Linear obligations** in host libraries are non-Clone, non-Copy
  `#[must_use]` owners with documented consuming exits; dropping one is
  detected. This is host-library discipline, not the source typing rules, and
  every report says so. Lane 15 is the first step that enforces a source rule
  in the checker, and it says exactly which rules.

## Evidence and claims

- "0 Alloc" claims need a counting global allocator test over the warmed-up
  steady state.
- Hardware and kernel features are reported unavailable unless exercised. At
  planning time the cloud host had no `/dev/kvm`, no `pku` CPU flag, no
  `intel_pt` or hardware `cpu` PMU, `perf_event_paranoid = 2` and
  `unprivileged_userfaultfd = 0`. Record which CI job, if any, exercised a
  feature.
- Real vendor roots (lane 4) are claimed only when a test verifies a real
  published vendor sample end to end, with the sample's provenance recorded.
- Interop claims (lanes 5, 6, 9) name the peer implementation and version.
- Lean files compile with Lean 4.24.0, contain no `sorry`, `admit` or new
  axioms, run from the owning crate's tests under `VESPER_LEAN`, and under
  Bazel declare `extra_test_data = ["//:lean"]`.

## Unsafe code allowed

| Step | Allowed gated module |
| --- | --- |
| 1 | `pkey_*`/WRPKRU (x86_64 Linux) and POR_EL0 (AArch64 Linux) |
| 2 | `perf_event_open` trace capture (Linux) |
| 3, 7, 10, 21 | `std::arch` SIMD, optional, with a scalar reference |
| 12 | userfaultfd and KVM ioctls (Linux) |
| 13 | RDPMC/perf mmap (existing PMU module), macOS kpc via dlopen |
| 18 | the existing `bpf()` module |
| 22 | Windows fiber switch and stack allocation |
| B, 23 | none beyond what `src/native.rs` already has |
| others | none |

## Sequencing

- Now: integration B once #149 and #151 merge; lanes 1-16, 18, 20-22.
- Lane 17 after #151; lane 19 after #149.
- Phase two: lane 23 after B; lane 24 after B and lane 15.
