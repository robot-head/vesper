# Kernel-only resource artifacts (#358)

This fixed Lean 4.24 tool exports a logical kernel projection. It leaves the
model semantic sources and Rust proof-audit CLI/options unchanged. The separate
issue #380 corrects the auditor's replay mechanism; its declaration-safety,
metadata and axiom policies remain strict. The auditor is no longer claimed
byte-unchanged. It does not verify native implementation equivalence or
claim that the original executable artifact passed the strict audit.

## Invocation and data

Run the pinned real Lean executable with a toolchain-only LEAN_PATH:

```text
lean -j1 -M4096 --run tools/resource-model42/KernelSnapshot.lean REQUEST.json
```

The caller must bind the actual helper source, compiler/toolchain and original
and projected files by hashes before/after use, and supervise the child with a
finite deadline and bounded output. Module loading and kernel normalization
are not bounded merely by the tool's logical traversal counters. The caller
must reject panic output and missing/malformed success records. This is not an
OS sandbox.

Request schema1 has exactly these fields:

```json
{"schema_version":1,"root":"ABSOLUTE ORIGINAL ROOT","toolchain_prefix":"ABSOLUTE PINNED PREFIX","module":"Original.Target","output_root":"FRESH NONEXISTENT ABSOLUTE ROOT","output_module":"Projected.Target"}
```

The three roots must be disjoint. The prefix must equal the canonical prefix
of the actual running Lean executable. Module names are bounded nonreserved
ASCII identifiers. The output root must have an existing parent and not exist;
the exporter creates a fresh private caller-selected tree, never overwriting
original model files. All imported original package declarations participate;
there is no requested-theorem filter or caller-supplied accepted flag.

On success the tool prints exactly one
`VESPER_RESOURCE_KERNEL_SNAPSHOT_V1:`-prefixed JSON record. On failure it exits
one with a bounded diagnostic and no success marker. Partial output after a
failure is not an accepted artifact.

The report contains schema_version1, scope `kernel_logical_projection`,
original_module, projection_module and absolute projection_path;
original_modules/trusted_modules arrays of `{name,path}`; original_constants
(unique original logical declarations), retained_constants, omitted_constants
and full omitted records `{name,parent,module,reason:"matched_partial_unsafe_rec"}`.
It reports original_constant_occurrences, duplicate_constants and duplicates
`{name,first_module,repeated_module,reason}`. A duplicate reason is
`identical_safe_theorem_realization` if its full info equals the first raw
copy, otherwise `safe_theorem_proof_variant`; both require the same full
non-proof theorem header. This comparison is against the first raw occurrence,
not necessarily the canonical imported proof body.

The additional alias_scope is `synthetic_theorem_proof_variants`.
proof_alias_count and proof_aliases identify every additional theorem:
`{name,original_name,source_module,canonical_module,source_occurrence,original_group,reason:"checked_safe_theorem_variant"}`.
Names are fresh `ResourceKernelProofAliases.realization_N` identifiers.
source_occurrence is the zero-based index in that source module's raw
constants array. canonical_module is the first raw occurrence whose complete
ConstantInfo actually matches the original imported canonical constant; it
makes no claim about Lean's internal winner tag when copies are identical.
The occurrences array maps every raw entry
`{name,module,index,disposition,alias}`. Indices are module-local; disposition
is retained_canonical_info, omitted_auxiliary or proof_alias; alias is null or
its associated fresh theorem name.

Unique originals equal retained plus omitted; raw occurrences equal unique
originals plus duplicate occurrences. output_constants equals retained plus
proof_alias_count and is the required strict-auditor checked-constant count.
Every proof_alias must also enter that auditor's requested theorem root union,
under the same axiom policy, partitioned into at most128 roots per request.
Auditing only canonical source theorem names is insufficient.

Logical expression work is reported as logical_expression_nodes. The flags
exact_inventory, constant_info_equal, kernel_replayed, reimport_verified,
extensions_empty and ir_empty describe checks actually completed; the final
two describe output, not whether input metadata was deserialized. There are
no self-asserted cryptographic digests in this Lean report.

## Selection, preservation and verification

Original artifacts are imported with no plugins or persistent-extension
initialization. Lean may deserialize private, IR and extension data during
import; the tool does not execute package initializers and does not export
those data. It independently loads a clean pinned-toolchain base with the
artifact root removed from the search path. Raw package module entries are
checked for trusted-name shadowing before import coalescing or filtering.
The redundant raw constNames array must exactly equal constants.map name.
Every imported trusted ConstantInfo must equal its independently loaded base.

All safe original logical declarations remain unchanged. Explicit unsafe
package declarations and every package opaque are rejected, even if unused.
The latter excludes a source `partial def`'s safe opaque logical facade.
Package axioms remain unchanged so the existing strict theorem-closure axiom
policy can reject a forbidden dependency; projection is not axiom approval.

The only omission shape is a partial definition named by appending the exact
`_unsafe_rec` component to a retained safe definition. Types, universe
parameters, opaque auxiliary hints and mapped mutual-group metadata must
match. The group must be nonempty, distinct, contain its owner, and have all
safe owners/partial auxiliary members in the same original module. This is a
narrow exclusion shape, not proof of compiler provenance or native behavior.
A safe user declaration with the same suffix is retained. An explicitly
unsafe declaration with a plausible suffix is rejected.

Repeated raw entries may be safe theorem realizations with the identical
complete non-proof header (name, type, universe parameters and mutual group).
Every non-theorem, type, universe or group mismatch is rejected. Lean's importer
can select among different proof bodies for one theorem; the canonical map
therefore comes from original.find? exactly and must match a real raw occurrence.
Its complete original ConstantInfo remains unchanged, including the selected
proof body. Each different raw proof occurrence becomes an additional alias,
even if it was the first raw occurrence encountered. Identical canonical-body
copies need no alias. All module associations and occurrence indices remain
visible in the report.

The entire canonical world is kernel-replayed first. Each alternate raw type,
universe parameter list and proof body is then copied unchanged into a fresh
theorem label; only the new name and singleton all=[alias] group differ. Its
original group is validated, equal to canonical and recorded. The alias is
kernel-checked against the already proved canonical context and included in
the final exported/reimported expected inventory. Canonical and alias names
cannot collide. These additional proof witnesses are not changes to model
source declarations and do not assert compiler provenance or native behavior.
They must all be separately requested in the unchanged strict axiom audit:
kernel typechecking alone does not exclude a forbidden axiom hidden in an
otherwise unused alternate proof body.

Every retained declaration is a root of complete logical dependency checking,
including reachable toolchain declarations. The visitor checks safety flags,
actual types/values, trusted opaque values, projection structure names,
inductive families/constructors, constructor ownership and recursor families,
constructor names and rule right-hand sides. Missing references or unsafe/
partial dependencies fail. Free variables and metavariables fail.

All canonical retained ConstantInfos are replayed through the real Lean kernel
at trust level zero. The shared CheckedKernelReplay block is adapted, with
Apache2 attribution, from Lean.Replay: it retains dependency ordering, complete
inductive groups and postponed constructor/recursor checks, but invokes
Kernel.Environment.addDeclCore directly. Environment.ofKernelEnv reconstructs
an inspection wrapper; post-replay lookups inspect its checked kernel map.
It does not call WithoutChecking or use elaborator private-name normalization.
The snapshot block is byte-identical to the reviewed audit helper block from
issue #380. Aliases use the same checked kernel primitive. The exact full resulting inventory and every ConstantInfo
are compared, including regenerated constructor/recursor metadata. The tool
uses saveModuleData to write only trusted imports and the unchanged retained
canonical-plus-alias constant array, with no extension entries, extra IR names
or IR artifact.
Fresh reimport under only toolchain plus projection root must preserve the
exact inventory, constNames, imports, metadata and full ConstantInfos. The
strict CLI then independently replays and audits the projection under its
unchanged policies and interface, using the separately corrected replay backend.

The fixed logical envelopes are request65536 bytes, module count8192,
package declarations65536, duplicate occurrences65536, raw occurrences131072,
logical dependency names262144, expression visits20000000, constant names4096
bytes/512 components with numeric components below2^64, and report16777216
bytes. Module names are at most1024 bytes/128 components. These are host-tool
profile limits, not a proof of all loader/kernel allocation or CPU bounds.

## Executed evidence and remaining release gate

The owned helper compiled with real pinned Lean 4.24. On checkpoint2231117,
all six independently authored fixed controls had the intended outcomes:

- Total recursion: 32 unique originals,30 retained,2 matched partial helpers.
- Safe exact user `_unsafe_rec` name: all3 constants retained,0 omitted.
- Explicit unsafe and plausible-suffix unsafe: both rejected.
- Source partial definition: rejected at its opaque facade.
- Project axiom: both2 logical constants retained for the strict policy.

The unchanged strict CLI rejected the original total artifact at its compiler
partial, and accepted the projected total artifact's two real theorems with
30 checked constants and no forbidden axioms. It also accepted the safe suffix
control with3 checked constants. The projected axiom control was actually
kernel-replayed and rejected by the unchanged explicit axiom policy.

Logs are under `/tmp/resource-kernel42-snapshot-v1`: compile-v3.log,
controls-v3-summary.json and the control-*-audit-v3.log files. The Axiom CLI's
expected nonzero error message follows its JSON in the combined validation
log; an initial summary script mistakenly parsed that combined stream as one
JSON value. That script failure is not counted as a policy pass; the actual
JSON records and stderr independently establish the expected rejection.

The broader theorem-only policy followed concrete diagnostics, not a silent
relaxation. Immutable `/tmp/resource-model42-evaluator-proofs-v4` predates Repr
removal and lacks final Main/Acceptance modules. It first exposed identical
RuntimeAtom.subst.eq_2 copies, then same-header/different-proof
RuntimeExpr.asValue.eq_3. Independent acceptance reproduced the latter from all
twelve final sources before the alias amendment was publicly approved.

On actual helper ce116b8, both independently authored universe-polymorphic
alias fixtures exported successfully: the clean case retained2 canonical
constants plus1 alias; the forbidden case retained3 plus1 alias. The canonical
proof is selected from the original imported environment in both cases. The
unchanged strict CLI accepted the clean canonical-plus-alias roots (3 checked
constants). In the forbidden fixture it accepted the canonical root alone,
but rejected the complete canonical-plus-alias union with the explicit project
axiom `ResourceSnapshotAliasAxiom.forbidden` (4 checked constants, actual kernel
replay). This is a meaningful negative control for omitted alias coverage.
Logs and alias-audit-summary.json are under
`/tmp/resource-kernel42-snapshot-v7`.

The fresh final twelve-source artifact root
`/tmp/vesper-resource-model42-782806-0`, target ResourceModel42.Acceptance,
contains the actual compiled final modules and78 semantic equalities. Its
first projection attempt correctly aborted with LEAN_ABORT_ON_PANIC in the
old canonical replay wrapper, before any success record. Lean's elaborator
AsyncConsts normalizes distinct kernel private names: MetricsProofs and
ErasureProofs private RuntimeContext.plug.match_1.eq_2 declarations collided in
that table. No name was silently renamed, dropped or treated as trusted.

The separately authorized #380 backend correction preserves those full kernel
names and performs actual kernel checks. Snapshot db6a433 adopted the exact
reviewed block920b6ac; followup50fd041 uses checked-kernel-map lookups after
wrapping the environment. Both compiled successfully with pinned Lean4.24.
The genuine final twelve-source projection then passed:

| Measured inventory | Count |
| --- | ---: |
| Unique original declarations | 3839 |
| Retained canonical declarations | 3791 |
| Matched partial compiler auxiliaries omitted | 48 |
| Raw declaration occurrences | 3853 |
| Repeated theorem occurrences | 14 |
| Additional proof aliases | 1 |
| Exact exported/reimported declarations | 3792 |
| Logical expression visits | 1313466 |

All twelve package modules participated. Actual replay, full ConstantInfo
comparison, export and fresh reimport completed without a panic, and both
private names remain intact. The output is
`/tmp/resource-kernel42-snapshot-v8/final12-output/ResourceModel42KernelSnapshot.olean`;
its full report is final12-export.log in that private root. The sole extra root
is `ResourceKernelProofAliases.realization_0`. Successful compile logs are
compile.log/compile-v2.log in the same root. Compiler leases were explicitly
released after commands exited; this lane ran no Cargo command.

The final complete CLI audit still requires a rebuilt executable embedding
the corrected audit backend, all284 manually catalogued theorem roots plus
the reported alias, the same standard-three axiom allowlist, fresh associated
artifact/source hashes and the independently owned adversarial acceptance.
This successful snapshot alone is not that final policy gate. No native
implementation proof, full language completion or unreported source rewrite
is claimed.
