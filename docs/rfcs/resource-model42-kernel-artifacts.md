# Kernel-only resource artifacts (#358)

This fixed Lean 4.24 tool exports a logical kernel projection. It leaves the
existing `tools/proof-audit/Audit.lean`, Rust proof-audit CLI and model semantic
sources unchanged. It does not verify native implementation equivalence or
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
(unique declarations), retained_constants, omitted_constants and full omitted
records `{name,parent,module,reason:"matched_partial_unsafe_rec"}`. It also
reports original_constant_occurrences, duplicate_constants and duplicates
`{name,first_module,repeated_module,reason:"identical_safe_theorem_realization"}`.
Unique originals equal retained plus omitted; raw occurrences equal unique
originals plus duplicate occurrences. Logical expression traversal work is
reported as logical_expression_nodes. The flags exact_inventory,
constant_info_equal, kernel_replayed, reimport_verified, extensions_empty and
ir_empty refer to checks actually completed; the final two describe output,
not a claim that importing never deserializes metadata. There are no
self-asserted cryptographic digests in this Lean report.

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

The only accepted repeated raw entries are field-identical safe theorem
copies; every repeated occurrence is recorded with both module associations.
Non-theorem duplicates or different theorem ConstantInfos are rejected. A real
example requiring this narrow rule is RuntimeAtom.subst.eq_2, identically
realized in both ErasureProofs and MetricsProofs. No different proof bodies are
silently selected or rewritten under this profile.

Every retained declaration is a root of complete logical dependency checking,
including reachable toolchain declarations. The visitor checks safety flags,
actual types/values, trusted opaque values, projection structure names,
inductive families/constructors, constructor ownership and recursor families,
constructor names and rule right-hand sides. Missing references or unsafe/
partial dependencies fail. Free variables and metavariables fail.

All retained ConstantInfos are replayed through the existing Lean kernel at
trust level zero. The exact full resulting inventory and every ConstantInfo
are compared, including regenerated constructor/recursor metadata. The tool
uses saveModuleData to write only trusted imports and the unchanged retained
constant array, with no extension entries, extra IR names or IR artifact.
Fresh reimport under only toolchain plus projection root must preserve the
exact inventory, constNames, imports, metadata and full ConstantInfos. The
existing strict CLI then independently replays and audits the projection.

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

A genuine larger diagnostic used immutable
`/tmp/resource-model42-evaluator-proofs-v4`, target
ResourceModel42.EvaluatorProofs. It predates Repr removal and lacks final Main/
Acceptance modules, so it is not final-current-model evidence. After identical
realizations were recognized, the exporter correctly refused
RuntimeExpr.asValue.eq_3: type, universe and group metadata agree, but proof
values differ. That broader realization case remains outside this currently
approved narrow duplicate policy. The full final twelve-source projection,
complete theorem audit and adversarial acceptance remain pending. No native
proof, policy relaxation, source rewrite or Cargo command is claimed here.
