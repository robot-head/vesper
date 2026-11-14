# Addability foundations and raw reification acceptance (#532)

All nine genuine fixed matrices passed on source revision
`0714ae39c83ee107e9fc559d79514e5c7e330fbb`. The coordinator executed the
harnesses and Lean processes, with at most two concurrent Lean processes.
This acceptance lane independently checked the actual source catalogs,
original/projected identities, executable/log hashes and execution receipts
without running Cargo or Lean.

| Genuine packet | Authored/audited roots | Audit batches | Observed seconds |
| --- | ---: | --- | ---: |
| Foundations | 49 | 49 | 139.983 |
| Construction | 37 | 37 | 138.915 |
| Order | 64 | 64 | 149.116 |
| Map | 99 | 99 | 152.172 |
| Normalize, before subsumption | 131 | 128 + 3 | 179.134 |
| Normalization, including subsumption | 173 | 128 + 45 | 200.209 |
| Comparison | 191 | 128 + 63 | 201.356 |
| Shape | 226 | 128 + 98 | 206.513 |
| Addability | 179 | 128 + 51 | 182.80 |

The eight prior timings are measured harness-process durations from their
complete receipt; Addability's timing is the test harness result. Every
matrix reported one passed test, zero failed and zero ignored tests, including
all real refusal controls. The unchanged internal whole-matrix limit is 300
seconds.

Execution uses exact repository Rust test sources compiled directly with
CI-pinned Rust 1.100.0 and real dependency artifacts. These are neither
Cargo-built nor Bazel-built test binaries. They call the archived production
`vesper` CLI built by Bazel 9.3.1 with Rust 1.100.0 at producer revision
`d6357113e08805064e438498b81a81fa0dcf1122`. The CLI's proof command and
embedded audit/projection sources are byte-identical to the proof-stack
parent. The separate build origins are part of the evidence scope.

The fixed IDs are label `lean4lean_addability42`, source module
`Lean4LeanLevel42.AddabilityFinal`, projection
`Lean4LeanAddability42Kernel`, scope
`audited_addability_foundations_and_raw_reification_boundary` and marker
`LEAN4LEAN_ADDABILITY42_EVIDENCE`. The coordinator owns the imports-only final
module, new test entry point, narrow BUILD additions and integration report.
This closed packet adds no runtime module/proof selector, supplied expected
answer or weaker-policy flag.

The nineteen selected modules are ordered as follows:

1. `Lean4Lean.List`
2. `Lean4Lean.Level`
3. `Lean4LeanLevel42.Semantics`
4. `Lean4LeanLevel42.ListProofs`
5. `Lean4LeanLevel42.OrderedInsert`
6. `Lean4LeanLevel42.VarInsert`
7. `Lean4LeanLevel42.NameOrder`
8. `Lean4LeanLevel42.SortedKeys`
9. `Lean4LeanLevel42.MapValue`
10. `Lean4LeanLevel42.MapHelpers`
11. `Lean4LeanLevel42.PathPriming`
12. `Lean4LeanLevel42.LevelSemantics`
13. `Lean4LeanLevel42.MapAll`
14. `Lean4LeanLevel42.MapEquality`
15. `Lean4LeanLevel42.SortedSubset`
16. `Lean4LeanLevel42.MapAny`
17. `Lean4LeanLevel42.Addable`
18. `Lean4LeanLevel42.ReifyCycle`
19. `Lean4LeanLevel42.AddabilityFinal`

Traversing the actual source imports from the committed final module,
which imports Addable and ReifyCycle, recovered exactly these nineteen
modules. The four proof-module import graphs share eighteen modules; the
imports-only final module completes the closure. Every dependency precedes
its user in the fixed source order. LevelSemantics is needed by the cycle's
actual level denotation statement. The closure uses
light existing dependencies and excludes Shape, ShapeFinal, recursive
normalization and comparison-fold/geq proof modules. It fits the unchanged
hard source-count bound of 24. The unchanged import-closure check requires
every selected source to enter the replayed original artifact closure.

The 38 unique required theorem guards use the authors' public ASCII names
in namespace `Vesper.Lean4LeanLevel42`:

- Sorted subset APIs: `subset_sublist`, `keySorted_erase`,
  `subset_complete`, `subset_eq_true_iff`, `erase_subset_iff_subset_cons`.
- Subset boundary witnesses: `subset_needs_left_sorted`,
  `subset_needs_right_sorted`, `subset_duplicate_left_counterexample`,
  `erase_subset_needs_nodup_counterexample`.
- Actual map-any traversal: `norm_any_eq_toList_any`,
  `norm_any_eq_true_iff`, `norm_any_witness`, `norm_any_of_lookup`,
  `norm_any_congr_lookup`.
- Actual addability: `addable_iff_witness`, `addable_iff_subset`,
  `addable_mono_sorted_acc`, `addable_mono_keys`, `addable_orderedInsert`,
  `addable_lookup_congr`, `addable_beq_congr`.
- Addability witnesses: `addable_needs_sorted_target`,
  `addable_needs_sorted_keys`, `addable_nonzero_offset_witness`.
- Concrete raw-cycle properties: `reifyCycle_lookup`, `reifyCycle_WF`,
  `reifyCycle_keysSorted`, `reifyCycle_varsSorted`, `reifyCycle_noEmpty`,
  `reifyCycle_pathPrimed`, `reifyCycle_addable_a`, `reifyCycle_addable_b`,
  `reifyCycle_feasible`, `reifyCycle_lexChain`, `reifyCycle_toTree_reify`,
  `reifyCycle_mapValue`, `reifyCycle_reifiedValue`,
  `reifyCycle_counterexample`.

Guards only require presence. The unchanged source-command extractor derives
every actual public authored theorem in the selected helper closure,
including the generic map-any helper and any additional helpers absent from
the guard list. The unchanged root union adds every actual alternate-proof
alias. Existing 128-root batches audit the complete union, without a golden
root or alias count. Actual original compilation, projection and audit must
establish the complete inventory and source/object associations; guard counts
and static source inspection do not establish a successful replay.

Static byte comparison against base
`164562d9b9371d2e810af11ed05cfea90e37802f` confirmed that all eight older
required catalogs and complete Packet specification arms remain identical.
Measured including their terminating newline, the byte counts are:

| Prior packet | Required catalog | Complete specification arm |
| --- | ---: | ---: |
| Foundations | 1,243 | 796 |
| Construction | 1,656 | 777 |
| Order | 1,398 | 822 |
| Map | 1,445 | 920 |
| Normalize | 1,668 | 1,139 |
| Normalization | 1,293 | 1,426 |
| Comparison | 685 | 1,556 |
| Shape | 1,390 | 1,653 |

The complete runner from `const POLICY` through EOF remains byte-identical:
47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Removing only the new enum variant, required catalog, specification arm and
introductory eight-to-nine comment restores the base runner byte-for-byte.
Static assertions also passed for the nineteen unique selected module names,
the five fixed IDs, all 38 unique namespace-qualified guards, their catalog
association and the unchanged source-count bound. All 38 guards also
matched actual public theorem commands in the four new proof modules. These
checks and the inspected import graph/source hashes are retained in
`/tmp/lean4lean-addability42-acceptance-static-check.json`. Direct
`rustfmt --edition 2024 --check tests/common/lean4lean_proof_packet.rs` and
`git diff --check` passed. This lane changes only the narrow runner extension
and this new report; the existing endpoint registration and every BUILD
attribute remain coordinator-owned.

The unchanged validator retains all actual source/provenance binding,
original/projected source and artifact associations, occurrence/partition
validation, complete authored-root/alias discovery, strict audit checks,
bounded regular-file capture, missing/corrupt original and projected
artifact controls, changed-source refusal and the forbidden alternate-proof
control. The source cap of 24, 128-root batching, standard-three axiom policy,
all other caps and finite 300-second whole-matrix budget are unchanged.

The proved mathematical claims concern the actual prefix-first sorted
subset test, actual short-circuit `NormLevel.any`, and actual addability.
Map-any and lookup congruence require no sorting, map WF or physical tree
equality premise. The unconditional addability witness retains an arbitrary
stored offset; it is not restricted to zero. The subset characterization
requires sorted stored keys and accumulator. The two monotonicity forms keep
their distinct sorting premises, while actual ordered insertion preserves
addability under sorted accumulation without a map-key premise.

The concrete raw cycle addresses a separate boundary. WF, sorted keys,
sorted variable lists, syntactic nonempty rows and current PathPrimed
coverage do not establish an admissible ordering or reification denotation.
Its actual failed starts, feasibility test, fallback and denotation
counterexample are authored mathematical statements, not supplied expected
answers or a second checker. No claim says that normalize emits this raw map.

Sorted subset completeness and addability foundations prepare later greedy
completeness work. This packet does not prove feasible completeness,
fallback unreachability, canonicality, reification correctness on normalized
maps, reflection of opaque native Level.beq, whole-checker correctness,
compiler/native equivalence or a successful source campaign. Existing
implementation-proof audit status is unchanged. All nine completed matrices
and the observed checks are recorded here. The actual Bazel proof-target
build is pending separately at this report's commit; no successful Bazel
proof-target deployment or hosted CI result is claimed.


Independent inspection of
`/tmp/lean4lean-addability42-addability42-evidence.json` and
`/tmp/lean4lean-addability42-root-addability-v1.log` confirmed the complete
new packet, including real refusal controls and exact original/projected
identities. The actual source catalog has 179 public authored roots,
exactly equal to the audited set: 139 existing roots and forty new roots.
The new modules contribute nine SortedSubset, six MapAny, eleven Addable
and fourteen ReifyCycle roots. These are the 38 guarded declarations above
plus `list_forIn_any_eq` and `addable_eq_toList_any`, both automatically
included by the unchanged extractor. The imports-only final module adds no
theorem. An independent inventory of every selected helper source recovered
exactly the same 179 root names.

The nineteen-module actual transitive closure matches all selected sources
and 38 original source/object records. Its 649 original constant occurrences
partition into 633 canonical constants and sixteen matched compiler recursion
helpers. The complete omission records match the earlier Normalization
checkpoint exactly. There are zero duplicates and zero alternate-proof
aliases, exact inventory and constant-info equality, empty extensions and
IR, successful kernel replay and verified reimport. Logical traversal
visited 1,695,656 expression nodes. The projection
`Lean4LeanAddability42Kernel.olean` is 5,984,688 bytes, BLAKE3
`e33177656ba1fd8f1975a1c004ac5242c475dae293393a17a52dfe1ad2ebc9be`.

The 128- and 51-root audit batches are accepted with status `audited`,
`kernel_replayed = true`, `cache_eligible = false`, 633 checked constants
and no forbidden axioms. Each batch's actual theorem axiom union is exactly
`Classical.choice`, `Quot.sound` and `propext`. The two disjoint batch sets
cover every authored root, all of which occur in the retained canonical
partition. Audit artifact filenames, byte counts and BLAKE3 match the exact
projection. The original module is consistently
`Lean4LeanLevel42.AddabilityFinal`; evidence records
`whole_checker_verified = false`.

All original records shared with historical checkpoints match exactly:
twenty-two with Normalization, twenty-four with Comparison and twenty-eight
with Shape. All 139 existing roots are present in the earlier Shape catalog.
The five current new source hashes match the proof authors' final identities.
The combined immutable original compilation root
`/tmp/lean4lean-addability42-integration-v1` has a verified 69-file SHA-256
manifest; its five new source files exactly match the current repository and
each has a genuine original executable object. This separate compilation
manifest does not replace the nineteen-module packet evidence.

The build receipt is
`/tmp/lean4lean-addability42-rust100-harnesses-v1/receipt.json`.
Independent checks verified the actual compiler version
`rustc 1.100.0 (0ef1bc386 2026-11-09)`, all nine harness executable/source
hashes, the shared runner hash and empty successful compile logs. The
recorded real blake3/serde/serde_json dependency artifacts exist. CLI receipt
`/tmp/vesper-proof-auditor-bazel-d635-v1/receipt.json` binds both the original
Bazel output and archive to 150,460,744 identical bytes, SHA-256
`8ee12bf3d266b6a0f1a47784ed1b5752b574134138d72444f6d14f46a7a31a32`.
All six logged proof-audit invocations in the new matrix use that archive.
`src/proof_commands.rs`, `tools/proof-audit/Audit.lean` and
`tools/resource-model42/KernelSnapshot.lean` match between parent, producer
and executed source tree. The audit report's `auditor_blake3` identifies the
embedded Audit.lean text; the CLI executable identity is supplied separately
by this receipt. No stub or substituted mathematical report is involved.

The cycle result is exact: both empty-accumulator starts fail, feasibility is
false and lexChain falls back to the original two-name path. Actual
`toTree.reify` reconstructs `imax (imax b a) b`. At `ρ(a)=0`, `ρ(b)=1`,
the raw map denotes zero while that level denotes one. WF, strict key and
variable sorting, nonempty rows and PathPrimed all hold. The formal boundary
is about this raw map; it does not assert that normalization emits it.

Independent checks are retained in
`/tmp/lean4lean-addability42-independent-check.json` and
`/tmp/lean4lean-addability42-independent-build-check.json`. The complete
prior-eight receipt is
`/tmp/lean4lean-addability42-prior-matrices-receipt.json`, and its progress
log ends `ALL EIGHT PRIOR MATRICES PASSED`. Independent checks verified every
recorded harness and full log SHA-256. For each earlier packet, the complete
authored/audited catalogs, all original source/object fingerprints, exact
projected artifact fingerprint, provenance and snapshot-tool identity match
the corresponding earlier Shape-wave checkpoint. All records shared with
Addability also match. Every audit is accepted, kernel-replayed,
cache-ineligible and free of forbidden axioms, with complete root coverage.
Each packet's actual theorem axiom union is the standard three; individual
batches use only that allowed set. The final three-root Normalize batch uses
`propext` alone. No historical fingerprint mismatch or source/CLI association
defect was found.

Strict Clippy of all nine exact repository test sources passed in 8.295
seconds under Rust 1.100.0 via a minimal real-dependency package, with
`--tests -- -D warnings`. This is targeted harness linting, not a claim about
the whole root package or workspace. The package uses actual serde 1.0.229,
serde_json 1.0.154 and blake3 1.8.7 dependencies. Its manifest, log and the
nine repository source paths/hashes were independently checked against
`/tmp/vesper-addability-ci-clippy-1.100-v1/receipt.json`. Rustfmt 1.100
`--edition 2024 --check` passed for the new test and common runner; BUILD
synchronization and whitespace checks passed. These checks do not relabel the
runtime matrices as Cargo-built or Bazel-built test executions. The archived
production CLI's successful Bazel build and the separately pending proof
target build have distinct scopes.
