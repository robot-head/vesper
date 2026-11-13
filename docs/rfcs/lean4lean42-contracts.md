# Frozen external model-replay contract (#297)

This document adopts the exact schema refinement, report types, compiler-library
binding and streaming rules in `lean4lean42-contract-plan.md` at commit
`45ff829`, with the normative resolutions below. The earlier plan's tentative
language is superseded by this adoption. Source writers may begin only after
this revision is pushed and their child issue is created. Changes to this
shared seam must be published before dependent edits.

The first profile exposes exactly two bundled models. It establishes complete
eligible fresh-closure replay and bound controls, not axiom hygiene, verified
implementation proofs, whole-compiler refinement or full §42 completion.

## Exclusive implementation lanes

- Kernel/control lane #298 retains all paths in its published component
  contract. Its reviewed source checkpoint is `1364376`, report `6806082`, and
  independent review `3b3f889`. The actual 89 controls pass independently; final
  assembled model replays and stored constructor/recursor mutation checks
  remain required.
- Machine lane owns only `tools/lean4lean42/patches/replay.patch` and
  `docs/rfcs/lean4lean42-replay-report.md`. The patch modifies upstream
  `Main.lean` only. Use the existing executable target and import the fixed
  `Lean4Lean.VesperControls` umbrella; no additional Main library is needed.
- Rust lane owns new `src/bin/lean4lean42.rs` and
  `src/bin/lean4lean42/{mod.rs,protocol.rs,process.rs,artifact.rs}`. Additional
  Rust module filenames need a published ownership amendment. It owns the
  ordinary CLI, exclusive `--seal-setup STAGING_DIRECTORY` dispatcher and
  private `--fault-peer MODE` dispatch to the acceptance-owned peer.
- Setup lane owns `tools/lean4lean42/{setup.py,README.md}` and
  `docs/rfcs/lean4lean42-setup-report.md`. It does not edit patches, controls,
  pins or Rust. Use Python's standard library and the explicit genuine Rust
  driver for BLAKE3 sealing.
- Acceptance lane owns `tests/lean4lean42_acceptance.rs`,
  `src/bin/lean4lean42/fault_peer.rs`, and
  `docs/rfcs/lean4lean42-acceptance-report.md`. The peer exposes
  `run(mode: &str) -> Result<(), super::Error>` and emits only malformed/fault
  traffic or transport-only markers, never positive replay evidence.
- Root owns these contracts, `tools/lean4lean42/{pins.json,source-manifest.json}`,
  integration evidence and separately claimed narrow CI/test metadata.

All lanes use isolated Vesper and upstream worktrees. No Cargo/dependency,
main/lib/native/checker, existing oracle/model/witness/proof-audit/metatheory,
or external lane edits. Runtime/kernel helper text is fixed handwritten Lean;
any dynamically generated Lean uses `vesper-lean-syntax` exactly once.

## Shared stored-metadata comparison controls

The #298 umbrella exposes `constructorMetadataMatches` and
`recursorMetadataMatches`, each taking generated and imported Lean metadata and
returning exact structural equality over every field. The four existing Main
BEq instances move into the umbrella. Production postponed comparisons in
#299 call these same helpers; successful lookup alone never supplies coverage.

Five additional required condition controls use constructor/recursor metadata
from an actual freshly checked Tree0 block: unchanged constructor and recursor
positives, and constructor type, recursor rule RHS and recursor rule constructor
mutations as negatives. Expected reasons are `metadata_equal`,
`constructor_metadata_mismatch` and `recursor_metadata_mismatch`. Preserve all
89 existing controls. The manifest becomes 94 required cases after execution;
these condition refusals are not raw-declaration kernel refusals. Update the
existing registry, source hashes and report without changing kernel semantics.
This amendment was published on #298 before dependent source edits.

## Required roots and inventory accounting

The arithmetic roots `checkExpr`, `checkProgram`, `evalSteps` and
`processRequest` in `Vesper.ArithmeticOracle` exist and are eligible definitions.
Require all four. RewriteWitness's 33 catalog/support roots are exactly its
33 existing `#print axioms` roots, represented by exact Name components rather
than dotted-string parsing. Target0 requires those 33; target1 requires the
same 33 plus the four arithmetic roots. Root membership, origin and eligible
status must match the fixed modules before replay; coverage must follow.

The independent native inventory probe observed 86647/88337 eligible constants,
including 1178/1252 constructors and 767/815 recursors respectively. These
observations explain the old CLI's smaller declaration-addition totals; they
are not golden acceptance counts. Require every eligible inventory ID exactly
once, successful postponed comparisons, all required roots and zero unresolved
work. Quotient initialization covers its four actual names once after Eq.

Native and replay inventories use the same bounded exact Name encoding and
ordinals. Unknown forms, origins, owner relationships, mixed/invalid groups,
duplicate coverage or skipped required roots fail. A successful environment
lookup or an unchanged generic count never establishes coverage.

## Control-baseline and generated-artifact binding

The control fixture's imported environment is a separate input to its actual
checks. Bind it without claiming a fresh replay of its imports:

- MODE0 also permits TARGET2, meaning native inventory of exactly
  `Lean4Lean.VesperControls` with its closure. MODE1 still permits only targets
  0/1; modes2/3 still require2. No arbitrary module or new public target option.
- The same hello/inventory/module/constant/inventory_end/done forms apply to
  MODE0/TARGET2. Work fields remain zero and skips repeat inventory totals.
  Require the actual control module and `Vesper.Lean4Lean42Controls.runControls`
  to be present; this inventory is binding evidence, not external coverage.
- Sealing obtains this inventory and its resolved artifact envelope before
  running controls. Ordinary run repeats it, compares the receipt's identities,
  and hashes the resolved files before and after controls. Controls mode imports
  exactly the same fixed umbrella, with no ambient module substitution.
- Add `control_inventory_blake3` and `control_import_manifest_blake3` to the
  receipt and ToolInfo. Use the existing inventory/import typed hash domains
  with internal target byte2. No synthetic fresh-coverage counters are created
  for this binding-only phase.

Also bind generated tool/dependency build inputs used by these callbacks. Add
`build_output_manifest: Vec<FileDigest>` and `build_output_manifest_blake3` to
the receipt, and its digest to ToolInfo. Enumerate all regular files recursively
under `.lake/build` and `.lake/packages/batteries/.lake/build`, including the
checker executable, helper oleans/sidecars/IR and any runtime DLLs. Reject
symlinks/nonregular entries in this fresh-build envelope; do not skip unknown
new files. Sort normalized tool-root-relative paths by UTF-8 bytes and hash
file contents incrementally. This conservatively binds outputs; it does not
claim every retained object/trace was executed or that hashes prove derivation.

Defaults are8192 files/1 GiB, HARD16384/2 GiB, separate from compiler libraries,
source files and model-import artifacts. Add `build_output_files: usize` and
`build_output_bytes: u64` to Limits. The manifest must fit the receipt's 4 MiB
cap. Hash domain is `vesper.lean4lean42.build_outputs\0`, schema1 u16LE,
count u32LE, then each FileDigest in order: path length u32LE+UTF-8, bytes u64LE,
32-byte BLAKE3. Compare the complete set, lengths and hashes before/after use.
Setup logs and the receipt live outside these generated directories.
Their exact root names are `setup-inputs.json`, `build.stdout`, `build.stderr`
and `setup-receipt.json`. The validated setup input must name exactly those
two log files; a source file cannot be disguised as an excluded log path.

Inventory/replay children use only the private model and pinned sysroot search
roots. Control-baseline/control children additionally use the candidate and
pinned Batteries libraries. Module routing is exact: the two model module names
resolve to the private model root; Lean4Lean to the candidate library; Batteries
to its pinned library; Init/Std/Lean/Lake to the selected sysroot. An unexpected
module/root is a binding failure. Clear ambient search and loader overrides,
preserving only the required platform environment and controlled loader paths.

## Setup inputs, pins and receipt association

Root will populate final pins after the replay patch exists. Missing pins or
an unassembled candidate cannot become a successful setup/runtime result.
Rust may be authored/formatted before those data exist; it must wait for root's
assembled inputs before compilation/positive execution. No fake digest or
successful placeholder receipt is introduced to make parallel work appear done.

Before the named build, setup invokes the exclusive private operation
`DRIVER --verify-setup-source STAGING_DIRECTORY`. This validates the embedded
pins, patch/overlay/known-fixes bytes and the complete actual patched source and
dependency tree against the embedded source manifest. It writes no receipt,
runs no compiler/checker and supplies no positive replay evidence. This lets
the standard-library-only Python setup verify BLAKE3 through the genuine Rust
driver before executing build inputs. The post-build sealer must repeat source
validation and all of its other gates; a preflight result cannot replace it.

`pins.json` has schema1 and exact fields: profile, upstream_commit,
batteries_commit, lean_release, lean_commit, patches, controls_overlay,
known_fixes_blake3, source_manifest_blake3, build_argv,
checker_relative_path. Together with schema these are the only top-level fields.
Profile is `vesper.lean4lean42/1`; the immutable base/compiler/dependency are
those in the kernel component contract. Patches are the exact ordered
`{path,blake3}` entries for toolchain.patch, kernel.patch, replay.patch.
Controls_overlay entries have exact fields source,destination,blake3, initially
only the fixed umbrella copied to `Lean4Lean/VesperControls.lean`.
Build_argv is `["{lake}","build","lean4lean"]`; `{lake}` is a data token replaced
by the validated actual Lake path, never shell interpolation. The relative
checker path is `.lake/build/bin/lean4lean`; append `.exe` only on Windows.

`source-manifest.json` is schema1 plus files, the sorted complete expected
FileDigest array for the patched upstream and pinned dependency sources and
fixed overlay. Bind every source file; reject additions/removals/substitutions.
Ignore only Git metadata, the exact root and Batteries `.lake/build` trees,
and the four fixed root setup input/log/receipt files. Include every other
regular entry, including unknown entries under `.lake`; no blanket Lake-state
omission is allowed. Actual pinned TOML builds have no other generated cache
leaves. A future generated exception requires a published contract amendment.
Do not treat arbitrary new `.lean`, lakefile, manifest or native build inputs
as generated state. Document the exact
source walk in setup and reuse the same rules in the Rust sealer/runtime.
Source-manifest hash domain is `vesper.lean4lean42.sources\0`, schema1 u16LE,
count u32LE, then FileDigest fields in the same encoding as build outputs.

The pinned Batteries tree contains exactly one tracked documentation alias:
`docs/README.md`, Git mode120000, blob
`32d46ee883b58d6a383eed06eb98f33aa6530ded`, with exact target text
`../README.md`. Before source preflight, setup verifies that identity and the
actual alias (or Windows' literal Git alias file), then materializes only this
owned leaf as a regular file containing the pinned regular Batteries README.md
bytes. The expected source manifest binds those materialized bytes. All other
source aliases/reparse/nonregular inputs remain rejected; none are ignored.
This explicit recipe normalization was published on #301 after clean assembly
exposed the pinned alias, before dependent source edits.

Source/patch/pin/control data are repository-owned compile-time inputs to the
Rust binary; ordinary replay cannot silently select mutable replacement pins
from an unrelated cwd. Runtime compares the actual tree against the embedded
expected manifest. Data files and manifests have the stated finite caps.

Launcher probing preserves the invocation basename, then binds actual
`SYSROOT/bin/lean` and `SYSROOT/bin/lake` under the same validated4.24 prefix and
commit. Before/after checks include the selected compiler-runtime envelope and
its stable in-sysroot aliases. `driver_blake3` identifies the Rust executable
that sealed a receipt; it is observational provenance, not a requirement that
an integration-test executable or later compatible driver has identical bytes.
Ordinary run revalidates the profile, pinned inputs, candidate/compiler bytes,
generated outputs, control baseline and actual controls; it never trusts a
prior matched=true field alone. Incompatible input/profile changes fail.

Ordinary CLI without `--lean` uses the bounded receipt's recorded actual
compiler path. `Config.lean` remains a concrete PathBuf: the thin CLI supplies
that path when no override was given. An override must resolve to the same
validated compiler/prefix and bytes before execution. Do not invoke a manager
proxy that could install a toolchain during an offline run; reject an unmatched
launcher as ToolIdentity instead. Explicit setup owns initial launcher probes
and records the actual compiler and Lake paths.

Atomic setup publication must preserve path association: the checker/source/
generated entries are tool-root-relative, while compiler paths identify the
external canonical sysroot. A receipt sealed in a sibling staging directory
remains valid after the one atomic rename to the requested absent destination.
Neither source/build/control records nor receipt hashes prove machine-code
derivation, host integrity or the unfinished implementation proof closure.

## Integration and validation

Actual control-baseline inventory resolves 6261 regular artifact paths across
1584 modules, totaling 1,463,175,880 bytes. Set `artifact_files` DEFAULT to8192,
preserving HARD8192 and the 2GiB default/4GiB HARD byte caps. The initial4096
default cannot accommodate this mandatory measured envelope. Counts remain
observations, never golden acceptance values; every actual file remains bound.
This measured refinement was published on #300 before the default changed.

The private acceptance seam exposes production protocol DTOs and a closed
`process::testing::probe(Command, ProbeKind, timeout_ms, &Limits)`. Its result
reports the observed error, actual child reap/reader/watcher joins, and retained
frame/stderr/queue counters. Closed modes are Malformed, Oversize,
TruncatedHeader, TruncatedPayload, WrongToken, Stall, StderrFlood, Crash,
ShortWrites, QueuePressure and QueueCancel. Transport completions emit only
one/64 markers; QueueCancel must time out under queue pressure. stdin is null.
No probe returns a run report, control verdict or external coverage. This seam
was agreed by the process/protocol/acceptance writers and published on #302
before dependent implementation.

Use the proposed default/HARD limits, exact report/sealing fields and aliases,
plus the additions above. Explicit build setup, ordinary offline run and
private seal are distinct operations. Missing/wrong tools fail hard. Actual
positive acceptance always uses the real candidate and bundled models; private
fault peers exercise only refusal/cleanup/transport paths. Success requires
both complete target replays and all binding/control conditions, with separately
reported not-run axiom and implementation-proof audits.

Default integration acceptance obtains the genuine prepared tool root from
`VESPER_LEAN4LEAN42_ROOT`; absence is a hard failure, never a skipped pass.
Ordinary CLI still requires its explicit `--tool-root` argument.

Root coordinates Cargo and complete replay leases, integrates final manifests,
and claims any narrow CI setup/timeout edits before writing them. The final
protected PR must include actual setup, complete replay/event coverage,
meaningful mutation/rejection and portable process-cleanup evidence. No local
or hosted result is invented for a platform that has not executed it.
