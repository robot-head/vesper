# Executable resource model: independent acceptance (#322)

Final independent integration passed on Linux at source `94c0e4753a8ac6a9d9a4f8187698449e17e82cb5`: all 78 computed semantic assertions compiled, all 284 manual theorem roots plus the one preserved proof-variant alias passed the unchanged standard-three axiom policy, and every hostile/control case completed. The audited artifact is a separately bound logical projection; the original executable artifacts are not relabeled as passing the raw-artifact audit. Exact results and earlier genuine failures are recorded below.

## Scope and initial design checkpoint

This lane owns only `lean/ResourceModel42/Acceptance.lean`, `tests/resource_model42.rs`, and this report. The published contract is `ddf5d6c`, frozen against main `30de7fef8be040a890e34f26370ee15b1429642e` and specification snapshot `8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d`, with subsequent coordinator clarifications. The initial design commits `c27e30e`/`84b8e23` contained only this report and claimed no code or execution. Source authoring began only after the coherent compiled syntax checkpoint `d51a21b` was published and authorized.

This is a finite logical reference model, not an observation adapter. Its own Mint/Consume trace is not a native or evaluator backend trace. Existing runtime-provider, arithmetic-campaign and external Lean4Lean results do not establish this new model's correctness or coverage.

## Fixed semantic matrix

The eventual handwritten acceptance module will prove named equalities about actual validation/checking/invocation/evaluation APIs. It will not receive expected answers as authoritative model inputs, fabricate checked wrappers, or replace computation with caller-supplied typing assumptions. Exact constructors, result accessors, error names and theorem names follow the coherent syntax/checker/evaluator checkpoints.

| Control | Required property |
| --- | --- |
| Terminal unit, fuel zero | Completed without a transition or invented trace event |
| Bare mint, fuel one | Completed owned handle, one Mint, one live residual; no blanket exhaustion guard |
| Let mint then consume | Exactly three Full transitions: mint in let context, value substitution, consume; fuel three completes, fuel two retains Mint-only prefix and the live handle |
| Two simultaneous handles | Distinct fresh serials, actual ordered mints, forward/reverse consume order distinguished; serial allocation does not reuse a consumed identity |
| Move through new lexical slot | Same handle identity, no extra terminal event; stale reuse/alias is rejected by checking |
| Pair and full unpack | Structural transfer preserves child IDs, left binds at de Bruijn index one and right at zero; selected consume order is observable |
| Owned result and pair result | Completed handle/pair of handles retains exactly the owned residual; copy-result exhaustion is a separately conditional theorem |
| Strict unused initializer | An unused copy result whose initializer mints and consumes still executes that initializer; an unused linear initializer fails checking |
| De Bruijn scope/new slots | Introducing a scalar does not overwrite an older owner; new index zero versus older index one remains distinct; scope leaks and type errors stay separate |
| Both branches checked | A constant-zero condition cannot hide an ill-typed or linearly mismatched other arm; runtime executes only the selected accepted arm |
| Affine-only demand disagreement | Explicit profile refusal rather than a falsely claimed semantic ownership error or full Rust diagnostic match |
| Modal pair fields | `k_i ⊑ m_i` holds independently per field; ticket under copy/affine field override is rejected; copy/affine nominal floors do not relax a child obligation |
| Naturals at the boundary | Static literals zero and `2^64 - 1` accepted; `2^64` rejected by literal inference; invocation values checked separately and never change static acceptance |
| Copy-only initial inputs | Declared types are checked independently of actual values; invocation shape/range/cap refusals are distinct; closing lets count as actual fuel with input zero innermost |
| Compact copy doubling | Expanded type, per-value and whole post-substitution expression bounds account for duplicated children, even when host objects share storage |
| Dynamic refusal after mint | A lower independent value/expression cap refuses before allocation or ledger/event commitment, retaining the actual prior term, Mint prefix and live handle |
| Global fuel | Nested initializers/branches do not reset it; exact predecessor fuel exposes the current admitted state and prior prefix without cleanup consumes |
| Resource/event zero/exact caps | Deliberate first-operation refusal is not success; tombstones count toward total minted history; all proposed step growth is prepaid before commitment |

Pair floors are restricted to copy/affine by this contract; nominal-linear aggregate-root identities are not tested as supported. Source atoms are unit/nat/variables, not computed pair expressions or raw resource literals. Runtime values produced by substitution can be pairs/handles. Every fixed source occurrence uses a unique bounded logical label; those labels are distinct from slots and serials and are not Vesper byte spans.

Type/admission refusal and dynamic expansion refusal must be exercised independently. A test that is rejected by the checker before evaluation is not evidence of evaluator allocation preflight. Conversely, a dynamic refusal must retain a genuine already-committed prefix, not a syntax-derived expected ledger. Whole proposed substitution growth includes both branches, enclosing let contexts, and intermediate as well as final unpack substitutions.

The checker promises a first structured semantic error only. Acceptance will distinguish syntax/profile limits, semantic type/use failures, affine-only profile refusal, invocation failures, runtime invariant errors, fuel exhaustion and measured execution limits. It will not claim ordered Rust diagnostic equivalence. Accepted reachable execution must not be justified solely by a success-soundness theorem that would also permit an always-refusing evaluator; the progress/refusal classification and prefix correspondence are required audit roots.

## Private wrappers and proof scope

The raw program contains input types and source syntax, never inferred result types, usage masks, acceptance booleans, raw handle IDs or expected traces. `ValidatedProgram`, `CheckedProgram`, and any validated invocation expose only the frozen read-only API and genuine validation/inference evidence. Fixed handwritten compile-time privacy controls may check that public unchecked constructor names are unavailable; this is elaborator/API evidence, not a kernel theorem or reusable approval certificate.

The model proof roots must establish erasure, value/substitution correspondence, actual inference-to-Full.HasType soundness, input closure, finite ledger freshness/update, successful Full.Step correspondence, complete Full.Steps prefix correspondence, completed-value soundness and progress/refusal classification. The completed-copy exhaustion/exact-once claims keep their copy-result premise. No returned-owner empty-ledger test or posthoc guard substitutes for them.

## Planned actual kernel harness

The new Rust integration target will use the actual pinned Lean 4.24 compiler and actual existing `vesper proof-audit`. Both missing default tools and an explicitly supplied missing/wrong tool hard-fail; there is no skip path. Temporary artifacts contain only fixed handwritten model/acceptance source files copied from the checkout and genuine compiled dependencies. Any generated negative-control Lean, if needed, uses `vesper-lean-syntax` nodes and one render, never Rust text templates.

The fixed dependency order and complete all-new-named-theorem catalog will follow the actual writer checkpoints. Every public named theorem must be selected in actual replay/audit requests. The existing auditor accepts theorem roots, not direct definition roots: authoritative validation/checking/step/evaluation/substitution/metric definitions must occur in the selected theorems' actual transitive dependency closure. Computed semantic equalities provide that linkage; any additional fixed definition-identity witness is association evidence, not an extra semantic theorem. Requests have at most 128 names; a larger catalog is explicitly partitioned, with an asserted complete union and no omissions. One bounded serial matrix will avoid a Lean process per theorem or needless recompilation of the unchanged Full dependency.

Actual success assertions include schema version, exact selected root union, `accepted = true`, `kernel_replayed = true`, `cache_eligible = false`, nonempty checked constants and artifact inventory, actual tool/source associations and unchanged artifact bytes. Transitive axiom sets must be subsets of exactly the sorted standard allowlist `Classical.choice`, `Quot.sound`, `propext`; those are permitted axioms, not required ones. No sorry/admit/project axiom is accepted. Fixed `#print axioms` output is supplementary to actual replay, not a substitute for it.

Meaningful negative controls include a missing/corrupt actual object and, if included, a separate AST-built forbidden-axiom theorem that genuinely compiles but fails the standard policy. The existing syntax AST has no axiom-declaration node; a control can reference an existing forbidden `sorryAx` through ordinary term nodes without adding a project axiom or changing the syntax library. Such a deliberately rejected control is isolated from the successful model catalog. Tool/process outputs, deadlines and capture are finite; processes are serial and reaped, and failures remain failures rather than empty successful evidence. No missing model or tool can silently pass.

Cargo/Bazel discovers the new integration target, but existing BUILD/CI ownership stays with #320/#321. Actual target elapsed time will be measured before requesting any coordinator timeout adjustment. No new dependency, registration, source adapter, external Lean4Lean target/profile, native integration, ChoiceTape campaign, 100,000-program result, source-checker refinement or full §42 completion is authorized by this acceptance slice.

## Current status

The writer imported syntax `d51a21b` and checker `b9ce0c0` as dependency copies, not owned implementation work. The first owned acceptance checkpoint contains **28** explicitly named admission/type/metric/substitution assertions against the actual Syntax API. Their computed equalities use `rfl`; they do not assume a nonexistent `DecidableEq (Except ...)` instance or copy any unexecuted scratch smoke result.

Under a coordinator-granted short exclusive Lean lease, the writer copied the fixed handwritten acceptance file into `/tmp/resource-model42-acceptance-syntax-nVXK0M/ResourceModel42/Acceptance.lean` and executed the actual `/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o ResourceModel42/Acceptance.olean ResourceModel42/Acceptance.lean`, with `LEAN_PATH=/tmp/resource-model42-syntax-api-d51a21b` and `LEAN_NUM_THREADS=1`. The immutable import root contains the syntax owner's actual compiled Full/Syntax dependencies. Initial compilation reported only elaborator recursion-depth limits. The owned test file now uses fixed `maxRecDepth 8192`; model admission/execution limits were unchanged. The retry exited zero in 0.75s with empty output, compiling all 28 named assertions. Log: `/tmp/resource-model42-acceptance-syntax.log`. The lease was released promptly afterward. An earlier message estimated 29 cases; the actual source inventory is 28 and this report uses that count.

The next owned checkpoint imports the real compiled checker `b9ce0c0` and evaluator `26ec38f` APIs. It has **75** explicitly named assertions total, including the prior 28. Under a second granted short Lean lease, the writer copied the fixed file into `/tmp/resource-model42-acceptance-semantic-qmQ3I8/ResourceModel42/Acceptance.lean` and ran the same actual Lean command with immutable `LEAN_PATH=/tmp/resource-model42-evaluator-api-v1`. Compilation exited zero on its first attempt with empty `/tmp/resource-model42-acceptance-semantic.log`. All 75 assertions compiled; the compiler lease was released immediately. No exact wall time was instrumented for this second short compile.

Those new computed equalities cover complete model traces/actions/current terms and residuals, exact/predecessor/zero fuel, two live returned owners, reverse consume order, full unpack and strict initializers, both checked branches versus one executed arm, distinct semantic/profile/invocation failures, mixed linear/affine branch priority, input-closing transitions, and an independently lower dynamic value cap refusing after a real mint before substitution/ledger commitment. Requested fuel above the configured bound remains invocation failure even for a terminal value. A completed owned result retains its residual.

`tests/resource_model42.rs` fresh-copies fixed checkout bytes and discovers their actual import graph and explicit public theorem namespaces, partitions the complete catalog into at most 128 roots per request, and runs actual compiler/auditor processes. It follows trusted Bazel runfile symlinks when reading fixed source, resolves the real Cargo/Bazel executable before isolating the child environment, and uses bounded regular capture files outside the artifact tree to avoid a timeout-induced pipe-reader join. Those mechanics are authored behavior, not an executed portability claim. Missing/corrupt artifact and isolated AST privacy/sorry-policy controls follow a genuine positive baseline.

After committing the coherent 75-assertion/harness checkpoint `51baeb2`, the writer merged the coordinator-approved already-merged main `d1caa0a` without conflicts. Under an explicitly granted Cargo **build-only** lease, the writer executed `cargo test -p vesper --locked --test resource_model42 --no-run` at resulting head `27ac6c644d21c0406b391de5e696045fb2ba750c`. Environment used shared `CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next`, one job, dev/test debug info zero, incremental off, the real pinned `VESPER_LEAN`, `LEAN_NUM_THREADS=1`, and the explicit worktree Cargo lock path. Actual compilation exited zero in 1m25s with no warnings. Binary: `/workspace/vesper/target/parallel-next/debug/deps/resource_model42-946b8f6cd7deb246`; log: `/tmp/resource-model42-acceptance-rust-build.log`. The lease was released immediately. The command did not execute the Rust target or start a Lean/auditor process.

The proof catalog and aggregate imports remain in flight. The Rust target's runtime behavior, separate `proof-audit`, native/backend resource observation, external resource-model replay and large campaign remain unexecuted by this lane. Successful compilation of these 75 fixed examples and the Rust harness is not a completed general soundness or full integration claim.

Three additional fixed assertions are now authored for actual move identity, later serial freshness after consumption, and the total-history resource cap retaining consumed tombstones. The source currently contains 78 explicit named acceptance theorems; the last three await the final fresh compilation. The previously executed semantic checkpoint remains 75, not an extrapolated 78.

The writer subsequently added the fourth generated privacy control, `TypeInfo.mk`, in owned `f879cc2`. An explicitly granted focused `cargo clippy -p vesper --locked --test resource_model42 -- -D warnings` passed in 36.95s at that source; log `/tmp/resource-model42-acceptance-clippy.log`. `cargo fmt --all -- --check` also passed with empty `/tmp/resource-model42-acceptance-fmt.log`. Neither command executed the kernel target or started Lean. That Cargo lease was released promptly.

The final harness now additionally requires actual compiled checker-soundness, step/prefix/safety, conditional copy-exhaustion and metric-completeness roots, plus the separate `step_refusal_witness`/`runMachine_refusal_witness` and event-site roots from `ede8c75`. It does not mistake `Full_progress` for a measured-refusal proof. Every fixed source module must be transitively imported by the acceptance artifact, preventing orphan definitions from escaping replay. Public theorem inventory respects real namespaces and fails closed on unsupported declaration layouts. These final gate/read-bound changes are awaiting the full target compile/run and final lint refresh; they are not covered by the earlier lint timing.

## First complete target: genuine policy rejection

Before the first complete run, the writer compared all 12 fixed Lean source files byte for byte with coordinator integration `211628ae57126e86c54d32a07400f08481ebaf4e`: unchanged Full, production aggregate, and all ten folder modules including acceptance. They matched. The actual run used owned/source head `49f7cc619cb8280ec6d2218b44dc10f72f014fbc`, the same one-job/debug-zero/incremental-off shared Cargo environment, real pinned Lean, and `cargo test -p vesper --locked --test resource_model42 -- --nocapture` under an exclusive Cargo plus one-Lean lease.

Rust compilation passed in 1.31s. Fresh isolated compilation passed for all 12 modules, including **all 78** named acceptance assertions (6.00s for the acceptance module). The first real audit partition then failed with actual structured `audit_failed` JSON:

```text
unsupported unsafe or partial artifact declaration:
Vesper.ResourceModel42.instReprRuntimeContext.repr._unsafe_rec
```

This is a genuine model artifact-policy failure from recursive derived `Repr`, not a fixture expectation error, forbidden-axiom false positive, or reason to weaken the auditor. The actual run exited 101; zero target tests passed, one failed, none ignored. Target time was 49.92s and the independent outer wrapper measured 51.294s. Logs: `/tmp/resource-model42-acceptance-full.log` and `/tmp/resource-model42-acceptance-full-timing.json`.

No positive audit partition or later privacy/policy/missing/corrupt control completed in that run. The lease was released immediately and the coordinator assigned removal/replacement of unsafe derived debug helpers to the actual source owners. Successful compilation is recorded separately from the failed artifact audit. A fresh complete run after that source correction, plus final focused lint, remains pending.

The coordinator's fix `5d00aa0` removed unused `Repr` derivations without changing algorithms, proof statements or the strict auditor. The writer imported it as dependency copy `f8ba5416e26c53c8d110687014a7a841d542db68`, rechecked all 12 source files against integration, and reran the entire target under a new exclusive lease. All 12 fresh modules and 78 assertions again compiled. The first actual audit then exposed a second generated unsafe helper, `Vesper.ResourceModel42.instDecidableEqSourceExpr.decEq._unsafe_rec`. That run exited 101 after 44.14s target time / 44.587s outer wall time. Logs: `/tmp/resource-model42-acceptance-fixed.log` and `/tmp/resource-model42-acceptance-fixed-timing.json`.

Again no audit partition or later control passed; the lease was released immediately. Recursive `DecidableEq` derivations cannot be assumed safe merely because the source definitions compile. Source owners must remove unused instances or supply an audit-safe implementation where equality is required. The same unmodified strict policy and acceptance assertions will be rerun on that coherent source correction.

## Separately authorized kernel projection (#358), integration pending

Public issue #358 subsequently authorized a new fixed exporter under `tools/resource-model42/`, without editing the existing strict auditor or CLI. The raw executable artifacts remain separate and are not relabeled as having passed either failed audit. The exporter is a trusted logical-projection tool, not a native-code equivalence proof or reusable approval certificate. It preserves every retained logical `ConstantInfo` exactly, rejects explicit unsafe declarations, package opaques and unsafe logical dependencies, and inventories only narrowly matched generated partial implementation helpers. Identical safe theorem realizations in multiple original modules may be coalesced with exact occurrence/module records; mismatched or non-theorem duplicates are not permitted.

The owned Rust draft now compiles the original fixed sources first, invokes the actual fixed exporter under the same bounded process watchdog with toolchain-only `LEAN_PATH`, and requests a fresh nonexistent sibling output directory. It verifies the exact scope/schema, actual original/trusted module inventories, retention/omission/duplicate counts and associations, replay/reimport/same-info flags, and a standalone output object without side artifacts. Rust independently hashes original files before and after export, the actual projected object and actual tool source. The unchanged strict auditor then receives only that projected root/module; its object hash, complete theorem partitions and checked-constant count must match the measured projection. Authoritative computation definitions enter the replayed artifact closure unchanged.

Input loading may deserialize existing private/IR/extension data; disabling initialization is not a claim that such data was never read. Empty IR/extensions describe the **output** projection. The strict auditor remains the final kernel/axiom check on that output, with `cache_eligible = false`. Neither successful export flags nor artifact hash association prove equivalence of the original native executable.

Privacy controls retain the original executable imports. The isolated AST-built `sorryAx` control instead imports the projection, so it can reach the unchanged axiom policy. Missing/corrupt controls target the actual audited projection, and additional missing/corrupt original-input controls must fail export without a success record. Six separate fixed tool fixtures exercise genuine total recursion, safe user-owned `user._unsafe_rec` retention, explicit unsafe refusal, opaque partial-facade refusal, deceptive unsafe suffix refusal, and a projected project axiom that the final strict policy must reject. These controls are authored against the published request/report API; compilation, actual exporter/auditor results and portability remain pending for this integration revision. The two actual raw-artifact failures above remain part of the evidence history.

## Fresh full-model projection: genuine duplicate-proof rejection

Under a coordinator-granted exclusive Cargo plus one-Lean lease, the writer executed the complete target at owned head `5be840d30cf1cba4f09fe662bb56918686c181f8`, using the actual narrow snapshot implementation `5797515` and its prerequisites. Before execution, all 19 fixed model/tool/control sources matched coordinator integration bytes. The actual command remained `cargo test -p vesper --locked --test resource_model42 -- --nocapture`, with the established single-job/debug-zero/incremental-off environment and pinned Lean 4.24. The unchanged strict auditor was not edited.

Rust compilation passed in 1.60s. All 12 fresh original modules compiled, including all 78 acceptance equalities. The input-profile assertions passed: precisely the 12 fixed `.lean`/legacy `.olean` pairs, regular files without symlinked package inputs or `.ir`, `.olean.private`, or `.olean.server` sidecars. This is a deliberately restricted fresh legacy-artifact profile, not a claim that arbitrary modern package sidecars are bound by base-object checks.

The real snapshot process then exited one after 12.38s, before emitting a success record:

```text
kernel snapshot failed: conflicting theorem duplicate:
Vesper.ResourceModel42.RuntimeExpr.asValue.eq_3;
type_equal=true; levels_equal=true; group_equal=true; value_equal=false
```

This establishes that differently realized proof bodies also occur in the final freshly compiled model, not only an old mixed-source experiment. The current policy permits exact identical safe theorem copies only; it correctly refused this different-body case. No duplicate policy was relaxed, no projected object was accepted, and no strict theorem partition, privacy control, or later artifact/tool-fixture control completed in this run.

The target exited 101: zero passed, one failed, none ignored; target time 47.92s and independently measured outer time 49.597s. Actual log: `/tmp/resource-kernel42-acceptance-narrow.log`; wrapper evidence: `/tmp/resource-kernel42-acceptance-narrow-timing.json`. The failure-only retention path preserved all 24 original files at `/tmp/vesper-resource-model42-782806-0` (26,945,728 bytes). Their exact paths, lengths and SHA-256 values are recorded in `/tmp/resource-kernel42-acceptance-narrow-preserved-artifacts.json`; fixed-source preflight is `/tmp/resource-kernel42-acceptance-source-preflight.json`. These temporary diagnostics are execution evidence, not a portable proof certificate. The Cargo/Lean lease was released immediately after the failed run.

A separately reviewed and publicly authorized resolution of distinct equation-theorem realizations is required before another complete run. The two earlier raw-artifact policy rejections remain separate historical failures. Neither the fresh compilation nor this failed projection establishes kernel acceptance of the model projection or native executable equivalence.

## Authorized proof-variant alias follow-up (authored, execution pending)

After the actual fresh-model failure, the coordinator publicly amended #358 to preserve the imported canonical `ConstantInfo` exactly while separately kernel-checking every differing same-header safe theorem body under a fresh named alias. Definitions, theorem header mismatches and non-theorem duplicates remain refusals. Each alias changes only its own declaration name and singleton group metadata; original type, levels and proof body are retained. This is an additional synthetic proof-variant inventory, not a claim that canonical names contain every original realization.

The acceptance follow-up requires the exact alias report and every raw occurrence mapping, with source module and zero-based module-local index, original/canonical associations, original group metadata and bounded count equations. Every reported alias is added as an extra strict theorem root, disjoint from the complete 284 manual roots. The exact union is partitioned into requests of at most 128 under the same standard-three axiom policy, and the auditor's checked constant count must match canonical retained constants plus aliases. This closes the risk of accepting an unrequested alternate theorem whose proof uses a forbidden axiom. New fixed clean/forbidden-alternate controls are being authored by their separately assigned lane. The authored follow-up has not yet been compiled or executed; the narrow-profile failure remains the latest actual full run.

The owned control follow-up `bdbfce6` imports the fixture lane's six new fixed source files from `3a80b0b`. It requires the observed canonical/alternate module associations and one alias for each pair. The clean pair requests both canonical and alias roots. The hostile pair first requires its genuinely clean canonical root to pass with no axioms, then requests canonical plus every alias and requires exact rejection of `ResourceSnapshotAliasAxiom.forbidden` on the alternate proof. The alternate helper is not requested independently, so that failure cannot hide a missing alias-root request.

Under a build/lint-only coordinator lease, actual `cargo test -p vesper --locked --test resource_model42 --no-run` passed at `bdbfce6792f523b973a7cb9dad9136b251659678` in 1.37s outer time (Cargo 1.30s), focused strict Clippy passed in 0.58s, and `cargo fmt --all -- --check` passed in 3.734s. Logs are `/tmp/resource-kernel42-acceptance-alias-{build,clippy,fmt}.log`, with exact commands/results in `/tmp/resource-kernel42-acceptance-alias-checks.json`. The lease was released immediately; these commands did not execute the target or Lean. The new alias tool and controls still require the complete actual runtime matrix.

## Final independent integration: executed kernel and hostile-control matrix

Public issue #380 separately authorized replacing the existing replay implementation's private-name-sensitive frontend staging with checked `Kernel.Environment.addDeclCore` replay, retaining exact `ConstantInfo` and regenerated constructor/recursor comparisons. This fixes a replay implementation bug without relaxing default unsafe/partial rejection or the axiom policy. The Rust CLI, options and report schema were unchanged. The projector uses the same checked replay block. The producer's intermediate private-name PANIC was not accepted as success and was not an independently completed acceptance run.

The writer imported the final auditor checkpoints `920b6ac`/`7cb63f1`, snapshot checkpoints `db6a433`/`50fd041`, fixed private-name fixtures `c3a6c1a`/`9a4c27e`, and independently authored private regression `0f3ca2d` as dependency copies. The coordinator-approved already-merged main `c025b9a` was merged cleanly before execution. At actual compiled source head `94c0e4753a8ac6a9d9a4f8187698449e17e82cb5`, all **33** fixed Lean/compiler/test inputs matched integration `40c97c6` byte for byte; both checked replay blocks were also byte-identical. Exact paths, lengths and SHA-256 values: `/tmp/resource-kernel42-acceptance-final-source-preflight.json`.

Under an exclusive Cargo plus one-Lean lease, the writer ran these actual commands sequentially with the established single-job/shared-target/debug-zero/incremental-off environment, real pinned Lean 4.24, and the explicit worktree lock path:

| Actual command target (`cargo test -p vesper --locked --test TARGET -- --nocapture`) | Result | Target time | Independent outer time |
| --- | --- | --- | --- |
| `proof_audit_cli` | 6 passed, 0 failed/ignored | 89.25s | 118.912s, including 29.40s rebuilt CLI/test compilation |
| `proof_audit_private` | 1 passed, 0 failed/ignored | 13.61s | 14.480s |
| `resource_model42` | 1 passed, 0 failed/ignored | 412.15s | 413.286s |

The first target retained every existing forged-object, unsafe/partial, missing/non-theorem, `sorryAx`, initializer and transitive-axiom-policy assertion. The new private-name target freshly compiled the actual colliding private-name modules, independently observed their distinct full names, and verified real replay together. It also compared the actual rebuilt CLI's reported auditor hash with the exact checked-in `Audit.lean` bytes, excluding a stale embedded auditor binary.

The model target freshly compiled all 12 original modules and all 78 computed semantic assertions. It verified exactly 24 regular original source/object files in the restricted fresh legacy profile, with no symlinked package inputs or sidecars. The actual logical snapshot reported:

| Measured inventory | Count |
| --- | ---: |
| Unique original constants | 3,839 |
| Raw original occurrences | 3,853 |
| Repeated original names | 14 |
| Exact retained canonical constants | 3,791 |
| Narrowly matched implementation-helper omissions | 48 |
| Separately preserved alternate proofs | 1 |
| Output constants actually replayed | 3,792 |
| Checked logical expression nodes | 1,313,466 |
| Manual theorem roots / added alias roots | 284 / 1 |
| Strict audit partitions | 3 (128, 128, 29) |

The sole alias is `ResourceKernelProofAliases.realization_0`, preserving the ErasureProofs raw occurrence at module-local index 76 of `Vesper.ResourceModel42.RuntimeExpr.asValue.eq_3`; the imported canonical originates in MetricsProofs. Its original singleton group, type, levels and proof association were recorded. All 285 requested roots passed actual strict replay and the sorted allowlist `Classical.choice`, `Quot.sound`, `propext`; every partition checked all 3,792 output constants and the exact requested-root union. Every positive report required `accepted = true`, `kernel_replayed = true`, and `cache_eligible = false`, with unchanged original/projected hashes and exact inventories.

The standalone projected object measured 21,858,464 bytes, BLAKE3 `511f9481e7bcefad5bb2adbf1940072a086f791dc00eb7a09f45d8cb55c8aed6`. Actual snapshot source BLAKE3 was `bd2a748c9886b06e5419cf5aa01711dc46d8d3da916d9e184b543afaa7c27eb2`. The bounded `RESOURCE_MODEL42_KERNEL_BINDING` line contains the complete actual snapshot report, all original/projected file hashes, and root/partition counts. Its independently extracted JSON is `/tmp/resource-kernel42-acceptance-final-binding.json`, with a concise `/tmp/resource-kernel42-acceptance-final-summary.json`. These hashes bind observed artifacts and source; they do not prove native execution equivalence or grant reusable proof status.

All subsequent controls actually completed: missing/corrupt original objects refused export without a success marker; all four AST-built private-wrapper constructor probes failed elaboration; the AST-built `sorryAx` theorem compiled against the projection and failed policy; missing/corrupt audited projections failed the actual auditor; raw total-recursion artifacts remained rejected while their exact logical projection passed; safe user-owned helper spelling survived without omission; explicit unsafe, opaque partial facade and deceptive unsafe helper all refused for their real causes; and the projected project axiom failed policy. The clean variant pair passed canonical-plus-alias audit. The hostile pair's genuinely clean canonical passed alone with no axioms, while adding its reported alternate alias failed exactly `ResourceSnapshotAliasAxiom.forbidden`, with `kernel_replayed = true`. No alternate helper was independently requested to conceal an omitted alias-root request.

The actual model log contains **29** Lean compilation attempts (25 successful source compilations plus four deliberate privacy refusals), **11** snapshot attempts (six successful projections and five deliberate refusals), **13** strict auditor calls, and two tool-identity probes. Requests/processes were serial within the model target and bounded by the unchanged per-process watchdogs and 900-second whole-target budget. No source or assertion repair was needed in this final run. The measured 412.15s exceeds Bazel's default medium 300-second timeout; the coordinator was notified to arrange the separately owned long/900-second test metadata. This lane made no BUILD/CI changes and ran no Bazel, macOS or Windows acceptance command.

After execution, focused strict Clippy over all three targets passed in 8.198s, and `cargo fmt --all -- --check` passed in 4.165s. Exact command/timing records: `/tmp/resource-kernel42-acceptance-final-checks.json` and `/tmp/resource-kernel42-acceptance-final-lint-checks.json`. Logs use `/tmp/resource-kernel42-acceptance-final-{proof_audit_cli,proof_audit_private,resource_model42,clippy,fmt}.log`. The writer released the Cargo/Lean lease immediately after these checks. These are writer-executed observations, distinct from the source owners' preliminary compilation/control results and the coordinator's later broader regression/CI evidence.

The delivered scope is the finite ANF logical resource model and independently audited projection. Returned owners retain live residuals; exhaustion remains conditional on a copy result. This work supplies no source adapter, native/backend observation equivalence, large resource/ChoiceTape campaign, resource-model Lean4Lean profile, full Rust-checker refinement, or full specification §42 completion. The two raw-artifact failures and the narrow duplicate-proof failure above remain genuine evidence history.
