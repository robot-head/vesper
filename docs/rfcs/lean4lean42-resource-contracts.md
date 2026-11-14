# Resource logical snapshot external-replay contract

This is the narrowly versioned successor to `lean4lean42-contracts.md` for
#395. It preserves that profile's two model replays, all 94 real controls,
offline identity checks and complete eligible imported-closure accounting.
Production implementation begins only after the genuine full resource pilot
succeeds and this contract is pushed. A native inventory or the old control
suite alone does not establish support for the broader Lean import closure.

## Ownership

- #395 machine lane: `tools/lean4lean42/patches/replay.patch` and the new
  `docs/rfcs/lean4lean42-resource-pilot.md`; the patch changes upstream
  `Main.lean`, without changing the external kernel's declaration rules.
- #396 artifact lane: `src/bin/lean4lean42/artifact.rs`, new
  `src/bin/lean4lean42/resource.rs`, and its resource artifact report.
- #397 protocol lane: `src/bin/lean4lean42/{mod,protocol}.rs` and its new
  resource protocol report.
- #398 acceptance lane: `tests/lean4lean42_acceptance.rs`,
  `src/bin/lean4lean42/fault_peer.rs`, and its new resource acceptance report.
- Coordinator: this contract, new integration evidence, and
  `tools/lean4lean42/{pins.json,source-manifest.json,setup.py,README.md}`.
  A CLI dispatcher adjustment, if actually required, is limited to
  `src/bin/lean4lean42.rs` under #395.

No lane edits PR #323's frozen model, metatheory, projector or auditor. Native,
compiler, library, Cargo, workspace registration and CI paths are excluded.
Any later measured CI change needs its own public claim. Writers use isolated
worktrees; Cargo requires the coordinator's exclusive lease.

## Version and routes

Use profile `vesper.lean4lean42/2` and report schema 2, with three fixed model
reports. Retain RewriteWitness tag 0 and ArithmeticOracle tag 1. Add
`ResourceModel42KernelSnapshot` tag 3, whose actual module is
`ResourceModel42Kernel`. Modes 0/1 permit the three model tags; mode 0 also
permits control tag 2. Modes 2/3 remain exclusively control tag 2. Reject every
other combination and unknown tag; no fallback to the control module.

The existing framed wire format need not change. New setup identities bind the
changed Main source, patch and outputs. Old receipts fail closed; never alter
or reuse an old sealed tree as a newly observed build.

## Source and artifact binding

The input is the exact twelve-module Acceptance closure at PR #323 head
`ad7208b825513d1e6095fcb80c9621d000f570e2`, including MetatheoryFull and
ResourceModel42's 78 semantic assertions. Compile it freshly with the pinned
actual Lean 4.24 compiler. Bind every source and original object, the unchanged
projector source and its request/report, and the disjoint projected object
before and after external replay. Confine sidecars and imports as in the actual
resource acceptance; base `.olean` confinement alone is insufficient.

The historical #323 manifest uses SHA256. Preserve and bind its exact bytes;
do not reinterpret those digests as BLAKE3 or treat its commit label as binary
derivation. Runtime BLAKE3 source/artifact associations have their own explicit
domain and field labels. Keep detailed bounded records as retained sidecars
with report digests instead of replacing them with unbound counts.

Validate all producer module, canonical, omission, duplicate, occurrence and
alternate-proof mappings, including partition equations. The projected owned
declaration set must equal every retained canonical name plus every actual
synthetic proof alias. No requested-root-only pruning, unexplained extra name,
unreported omission or alias substitution is permitted. The 3,792 declarations
observed in the accepted build are evidence, never a golden acceptance count.

Use an immutable prepared-resource descriptor with a private constructor and
read-only association access. Expose production revalidation for real file and
binding mutations, so acceptance can exercise refusals without repeating the
complete multi-minute replay for every mutation.

## Exact names, roots and coverage

Keep wire names as typed string/number component sequences. Never split or
invert the producer's display strings. For snapshot-owned names only, a
restricted forward renderer may match the existing producer records:

- String components match `[A-Za-z_][A-Za-z0-9_']*`, or the exact literal `_@`.
- Numeric components fit the existing u64 wire representation and render as
  ordinary decimal without leading zeroes.
- Render by joining components with dots, reject unsupported components, and
  require a unique image before exact set comparison.

This map is injective: no admitted string component contains a dot or has a
numeric spelling. Its `_@` exception is the exact generated hygiene component
observed in two genuine package names; it is not a general escaping fallback.
The pinned Lean producer's actual images must match exactly. Trusted imported
names retain the full existing typed codec without this package restriction.

Require every owned declaration to be eligible and a required root; reject
any owned skip before constructing roots. Additionally require the actual
`Vesper.ResourceModel42` definitions `validate`, `checkProgram`,
`validateInvocation` and `evalSteps`, all 284 fixed source theorem catalog
names as theorems, and every producer-bound alias as a theorem. The 284 already
includes the 78 assertions. Exact complete-set equality is stronger than this
public catalog check and must not be replaced by a count.

Prepay root storage using actual bounded inventory sizes, replacing the old
37-root assumption. Retain complete eligible imported-closure coverage,
constructor/recursor structural comparisons, quotient prerequisites, unique
coverage IDs and zero remaining/pending work. Preserve target/session/terminal
bindings and before/after import envelopes.

## Evidence and limits

Expand the existing genuine serial positive once to all three models; preserve
the old model assertions, all 94 controls and existing refusal/transport suite.
Add meaningful route, owned-skip, missing/duplicate coverage, root/name/form,
alias, corruption, source/receipt/identity and cap controls through production
seams. Synthetic fault traffic is refusal evidence, never checker success.

Record real process exits, timing, complete coverage and exact source/tool/
artifact associations. Do not enlarge bounds based on predicted success.
Choose any necessary scheduling change from genuine measured execution.

Both `axiom_audit` and `implementation_proof_audit` remain `not_run`: this
external stage checks declarations, without rerunning PR #323's separate axiom
policy audit or completing Lean4Lean's implementation proofs. All alternate
proofs are mandatory external roots; they are also mandatory policy roots in
the separate strict auditor. Never transfer that historical policy success to
a newly unexecuted audit. Native implementation equivalence, Rust/compiler
refinement, native resource observation and the 100,000-program resource
campaign remain outside this slice.
