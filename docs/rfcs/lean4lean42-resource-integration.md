# Resource logical-model external replay (#395–#399)

Profile `vesper.lean4lean42/2` adds the bounded resource model to the existing
RewriteWitness and ArithmeticOracle external checks. It preserves all 94
controls and the complete eligible imported-closure checks. Pins, receipt and
result schema are 2; wire, known-fixes, upstream source manifest and setup-input
schemas remain 1.

Local validation is complete. PR #399 is ready for review, stacked on #323;
hosted CI and the parent PR's platform gate remain separate merge requirements.

This is logical declaration checking. Both `axiom_audit` and
`implementation_proof_audit` remain `not_run`. It does not establish native
implementation equivalence, Rust/compiler refinement, native resource
observations, the resource campaign or whole-checker self-verification.

## Bound inputs and actual setup

The twelve frozen sources, unchanged projector and historical SHA256 manifest
are bound by a separate fourteen-file BLAKE3 bundle (360,957 source bytes):
`97000097b449ddddf655e768968a6578c1ae3bb6dd18c654abf949759f723b70`.
The historical SHA256 records are never reinterpreted as BLAKE3 or proof of
machine-code derivation.

Fresh production setup exited zero and published
`/tmp/lean4lean42-resource-profile2-395`. All 94 actual controls matched. It
bound 336 source files, 289 generated files (240,371,587 bytes), and thirteen
compiler/runtime files (486,041,402 bytes). Upstream, Batteries and Lean pins
are unchanged. Only Main changes in the upstream source manifest; no external
kernel rule changed.

- Main: 31,718 bytes; BLAKE3
  `2f74d36e685ed3dd8ae40d86bf8d1de22a9660cb588692042a0c364d101c63b4`.
- Typed upstream source-manifest digest:
  `43b3da2d769951c1bb3903127fba2b5dcbc9ab8f1b98d135971fe24185a6a088`.
- Actual native checker BLAKE3:
  `063ed1d4c39de6bc7de89e084834749c9571526655d3ad6b2889b49c99e1160a`.
- Actual Cargo driver: 5,221,920 bytes; BLAKE3
  `56d6ee3078aeb7bf9faa8dcb1baacc15c8110550c74e5c8cebd12438096a8a07`.
  Its bytes still match the driver recorded by fresh setup.

## Complete production replay, followed by a real test failure

The first genuine production CLI run at source `fcf1d60` completed all three
targets in 821.15 seconds. Its actual report has thirty process observations,
all 94 controls matched, and zero timeouts, crashes or protocol errors.

| Actual target | Eligible declarations covered | Required roots covered | Constructor / recursor comparisons | Replay seconds |
| --- | ---: | ---: | ---: | ---: |
| RewriteWitness | 86,647 / 86,647 | 33 / 33 | 1,178 / 767 | 184.372 |
| ArithmeticOracle | 88,337 / 88,337 | 37 / 37 | 1,252 / 815 | 180.659 |
| ResourceModel42Kernel | 158,207 / 158,207 | 3,792 / 3,792 | 3,892 / 2,440 | 348.569 |

Every remaining/pending counter is zero. The resource closure has 1,732 modules
and 161,247 total declarations; its trusted-import skips are 438 unsafe-only
and 2,602 partial-only. No projected package declaration is skipped. These are
measured results, not golden acceptance counts.

The resource projection contains all 3,791 retained canonical declarations plus
one actual alternate-proof alias; 48 matched compiler auxiliaries are separately
inventoried. Its 21,858,464-byte object has BLAKE3
`511f9481e7bcefad5bb2adbf1940072a086f791dc00eb7a09f45d8cb55c8aed6`,
identical to the separately audited #323 object. That historical policy audit
is associated evidence, not a newly executed audit in this profile.

The acceptance target then genuinely **failed** at its new retained-source
assertion: the publisher labels files relative to the requested parent,
including a unique publication-directory component, while the test looked for
bare `resource/...` paths. All thirty-six published files had already passed
the generic file size/hash checks. The captured actual report independently
confirms that every fourteen-file bundle record and four resource associations
have the correct bytes/digests under the publication prefix.

Commit `65f7084` corrects the test's exact path mapping and checks the publication
is a single confined child directory. No production code, source bundle, root,
coverage requirement or refusal was weakened. Strict targeted Clippy passes
again. The initial target result was fifteen passed, one failed; it is not
reported as a successful complete acceptance run.

## Successful final validation

The complete target rerun at `65f7084515bc9e721d0101d400dd0fc7eb2904c8`
**passed sixteen tests**, with zero failures, ignores or filtered cases, in
926.01 seconds (930.643 seconds including Cargo/rebuild overhead). This includes
the real three-model positive, all existing controls/transport refusals and the
remaining actual prepared-artifact, alias, source/object, metadata, sidecar,
capacity and receipt refusals. The driver unit target separately passed all
fifteen cases.

The final genuine CLI positive took 821.69 seconds. Its replay times were
181.226, 180.967 and 353.787 seconds respectively; every coverage/root/comparison
count in the table above remained identical and all pending counters stayed zero.
The resource preparation took 70.903 seconds and its native inventory 5.203
seconds. All 94 controls matched, with thirty main-pipeline process observations
and zero timeouts/crashes/protocol errors. Auxiliary preparation/native probes
in the refusal matrix are additional stage checks, not extra external replay
successes.

Actual compilation, workspace formatting, strict two-target Clippy, Bazel
metadata synchronization and whitespace checks passed. The final source differs
from the first successful production CLI only in the corrected test path
assertion; the Cargo driver still byte-matches the freshly sealed receipt.
The existing external-checker target already has Bazel's eternal budget; its
measured 926-second complete matrix required no scheduling or bound increase.

Local evidence: `/tmp/lean4lean42-resource-profile2-395-setup.log`,
`/tmp/lean4lean42-resource-production-report.json`, the three captured producer/
request/tool sidecars, `/tmp/lean4lean42-resource-root-acceptance.log` and its
summary, the complete final production report
`/tmp/lean4lean42-resource-production-v2-report.json`, and the successful `-v2`
repeat log/summary. The independent pilot and
its corrected label guard remain documented in `lean4lean42-resource-pilot.md`;
its Python validator is not substituted for the actual Rust-driver run.

Read-only follow-up identified two possible independent proof lanes: conditional
semantic preservation for the actual patched universe-level normalizer, and
the small fresh-name reservation proof packet. These require separate claims
and genuine proof/audit work. The old Verify library's missing proofs and project
assumptions prevent a whole-checker self-verification claim today.
