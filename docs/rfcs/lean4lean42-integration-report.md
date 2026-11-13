# Fixed-model external Lean4Lean integration (#297)

This profile externally rechecks the eligible imported closures of exactly
RewriteWitness and ArithmeticOracle. Native compilation, input association,
actual external replay, finite controls, axiom auditing and implementation proof
completion are distinct. The two latter audits are explicitly not_run.
The untouched upstream Theory/Verify sources are unported/unbuilt and contain
known proof gaps; this work does not establish whole-checker verification,
compiler refinement, self-hosting/DDC or full §42 completion.

## Source and scope

The source is upstream 4faf51403bc4cb35cf6666d91fc9c938ae46d3aa with the reviewed
kernel, toolchain and Main-only replay patches. Lean is 4.24.0,
797c613eb9b6d4ec95db23e3e00af9ac6657f24b; Batteries is
8da40b72fece29b7d3fe3d768bac4c8910ce9bee. Generated upstream/helper Lean is not
emitted: these are fixed reviewed sources; the executable exchanges framed data.
The 94 required controls include actual raw checker cases, expression cases and
condition checks, including the same constructor/recursor comparisons used by
production replay. Condition refusals are not raw-declaration kernel refusals.

Setup explicitly obtains immutable source, verifies source identities through
the actual Rust driver before building, builds only the named checker target,
then seals actual source/compiler/library/generated-output/control evidence.
The sole pinned Batteries documentation alias is verified and materialized;
all other source aliases remain rejected. Ordinary replay is offline.
The default control-baseline file and metadata limits were raised to the
measured 8192 files / 80 MiB while preserving their 8192 / 128 MiB hard caps; no inventory
prefix is accepted to fit a lower cap.

## Actual validation checkpoints

- The first assembled Rust executable built successfully; six strict protocol
  unit checks passed. Its genuine source preflight accepted the independently
  assembled exact 336-file source tree. Later artifact-boundary unit checks also
  compiled and passed in the focused acceptance attempt.
- Independent actual transport validation passed all 11 closed fault/marker
  probes plus invalid-limit preflight in 1.756 seconds. Every expected actual
  child reap/reader/watcher join and queue/cap assertion passed, including full
  four-frame queue cancellation. These probes supply no mathematical result.
- First fresh setup completed successfully with a real 240,244,137-byte / 289-file
  generated-output envelope, 13 selected compiler libraries totaling 486,041,402
  bytes, and all 94 actual controls matched. Its source was the pre-String-order
  replay snapshot and is retained as historical setup evidence only.
- Native control inventory produced 139422 constants / 1584 modules and 141010
  frames. Production Rust parsing refused the original 64 MiB cap near the end
  and accepted the complete identical genuine stream at 80 MiB. The resolved
  control import envelope has 6261 regular paths / 1,463,175,880 bytes, requiring
  the measured 8192-file default. These are observations, not golden counts.
- Actual full replay of the first machine snapshot refused an unknown
  Char.ofNat dependency. The corrected source restores upstream's checked
  prerequisite before String; no preload or checker bypass was introduced.
  On the corrected source, independent target0 replay completed: 86647 eligible
  constants covered, 1178 constructor / 767 recursor comparisons, 33 required roots,
  179 target-owned constants, zero pending and clean EOF/exit0 in 186.391 seconds.

The first focused acceptance attempt failed before mathematical replay because
its synthetic quotient fixture omitted the required Eq prerequisite; the
fixture was corrected without relaxing production validation. The initial
strict Clippy pass reported nine source warnings; their routine refactoring
fixes await the final rerun. These failed attempts are not counted as complete
acceptance passes.

At this checkpoint the corrected target1 independent replay, fresh setup of the
corrected source, complete production Rust acceptance, final package regression
and hosted platform checks remain pending. No unexecuted result is claimed.

## Process and receipt limits

The selected source/build/compiler/import envelopes are conservative byte
associations, not machine-code derivation or hostile-host attestations. Kernel
internal allocations/OS thread stacks and arbitrarily stalled filesystem calls
are outside the logical Rust buffer bounds. The Rust supervisor manages the
fixed nonforking compiler/checker children; Python setup owns the Lake process
tree. An out-of-profile inherited pipe that does not close within cleanup grace
returns failure with actual joined=false rather than claiming cleanup; it may
leave a detached blocked reader. No successful profile result uses that path.
