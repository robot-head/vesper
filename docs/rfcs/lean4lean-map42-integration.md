# Actual normalizer map construction (#418–#423)

The fixed Map packet and all three preserved packets pass their complete
strict matrices. This adds 46 public theorems for the actual pinned
`NormLevel`/`Std.TreeMap` updates and syntactic current-path coverage.
Existing helper definitions and parent proof statements are unchanged.

## Proven behavior

`MapValue` has fourteen roots: actual max-fold denotation, lookup bridges,
generic fold bounds, stored-row lower bound and pointwise monotonicity,
erasure monotonicity, empty-map WF and exact insertion/decomposition using
the erased-key context. Insertion overwrites its key. Its equation is
`mapValue (insert p n) = max (mapValue (erase p)) (nodeValue p n)`;
union with the old row still present would mask an invalid replacement.
All comparator arguments use actual `List.compareLex Name.cmp` and its
proved lawful instances.

`MapHelpers` has twenty-two roots. Actual addNode adds its gated variable
value; actual addVar does so with an explicit present-key premise, since it
uses modify. Actual addConst retains the exact zero/one-nonempty skip
condition. The simplified constant equation requires only the narrowly
identified skipped-one bound. WF is preserved with the required inserted
variable membership, or unconditionally for constants. Actual lookup and
represented-name preservation helpers cover coincident witness/update keys
and changed offsets. Three real counterexamples expose missing-key modify,
and skipped constant one in both empty and present-zero-node WF maps.

`PathPriming` has ten roots. Its syntactic invariant supplies current-key
presence for nonempty paths and a stored variable witness for EVERY path
variable at a subset key. This proves enabled-variable/constant-one bounds,
survives all three actual updates at arbitrary keys, and is established by
actual ordered path-extension/addNode. It discharges the skipped-one premise
without assuming the intended map-value equation.

The two-variable witness has an enabled present key [u,v] with only v+0,
map value one and valuation u=three. Skipped constant one is safe there, but
the actual zero-offset parameter-u branch returns the insufficient map
unchanged. The second witness genuinely extends from empty to [u] and then
[u,v], retaining the old variable at a strict subset key.

These are actual construction/helper and coverage results. Whole recursive
normalizeAux correctness, subsequent minimization/subsumption, fresh-name
cache priming, whole-checker implementation proofs, compiler refinement and
native equivalence remain separate. Existing implementation-proof audit
fields are unchanged.

## Preserved fixed acceptance

Closed `Packet::Map` selects exactly eleven source modules. All three old
required arrays and packet specifications, and the complete validator/control
implementation from `const POLICY` to EOF, are byte-identical to #417.
Twenty-seven required APIs/witnesses are presence guards; unchanged command
extraction audits all 99 actual authored roots in the imported closure:
46 new and 53 imported. ALL actual proof aliases remain roots too.

Every provenance/source, original/projection hash association, occurrence/
partition, strict standard-axiom, capture and real refusal gate is retained.
There is no arbitrary-module, expected-answer or weaker-policy mode. Source/
artifact limits and the 300-second per-matrix budgets are unchanged.

## Genuine integrated execution

Executed source: `74b01b5283624daf1dc32580757625d27831c3ba` on
`lean4lean-map42/integration`. The four Rust targets built in 1m05s.
All four complete matrices pass with zero failures, ignores or filtered cases:

| Matrix | Authored roots audited | Canonical declarations replayed | Matched compiler helpers | Complete target seconds |
| --- | ---: | ---: | ---: | ---: |
| Original construction | 37 | 337 | 13 | 124.37 |
| Original foundations | 49 | 356 | 14 | 129.26 |
| New Map | 99 | 448 | 13 | 141.25 |
| Original Order | 64 | 371 | 13 | 133.70 |

The Map projection preserves 448 of 461 original declarations, visits
1,625,591 logical expression nodes, and has zero actual proof aliases.
Canonical ConstantInfo equality, kernel replay and fresh reimport all pass;
extensions and IR are empty. Thirteen matched partial native compiler
helpers remain separately inventoried under the unchanged projector policy.

Map object: 5,153,008 bytes, BLAKE3
`995159db11d56eb1f4629601d2489983575c4028648eca893ea2883030f6f864`.
All three original objects remain byte-identical:

| Packet | Bytes | BLAKE3 |
| --- | ---: | --- |
| Construction | 4,268,008 | 2e27b0f4fa2b1d09632399bf0f346986f608989dafca251d4a94dbc7fd805c40 |
| Foundations | 5,058,968 | 4d1483789427ed0be66c1245852d00c584ffc7d84a673cc4966142ae00459d7e |
| Order | 4,473,304 | 11dd016c1c16acdfebe41b6163c9ef23ef43f47d6cd63644df9ba7ca5e42c595 |

These are measured evidence, not golden acceptance counts/digests. Each
matrix also completes the real changed-source/provenance, missing/corrupt
original/projection and clean-canonical versus forbidden-alternate-alias
controls. Zero actual aliases does not skip the adversarial alias fixture.
All audits report accepted/kernel-replayed, caching disabled and no forbidden
axioms. The actual union remains Classical.choice, Quot.sound and propext
under the unchanged standard-three policy; this is not an axiom-free claim.

Strict four-target Clippy passes in 25.36s. Workspace formatting, Bazel
BUILD-file synchronization, whitespace and exact source associations pass.
Independent mathematical review of the frozen integrated sources and static
packet found no actionable defect. No runtime/compiler, actual helper/vendor,
parent proof, auditor/projector/profile, Cargo, CI or scheduling changes.

Evidence: `/tmp/lean4lean-map42-root-acceptance.log`, separately captured
`lean4lean-map42-{construction42,foundations42,map42,order42}-evidence.json`,
and root build/Clippy logs. Lane reports distinguish genuine development
failures/compilation from this final assembled execution. New source bytes
match the published genuinely compiled lane copies:

| Source | Bytes | SHA256 |
| --- | ---: | --- |
| MapValue.lean | 5,638 | 875688bc654b950ef09eafc321d311cab123ceb707ec01eb5e69b11ae647b071 |
| MapHelpers.lean | 11,663 | 4ba34b4255000586cce552d4fe38cb3b68bddfc83484a83aa895e3ce9af1ade8 |
| PathPriming.lean | 6,765 | 29e97d7b6a0f83e34bac0dd64a4f840753156cbb0997c4ba5cbb7ae715902e4c |

PR #423 is stacked on #417, with no auto-merge onto its unprotected parent.
Hosted checks and parent merge gates remain separate. During this work,
#323 merged into protected main; #399 and #407 were retargeted to main with
protected auto-merge enabled, incorporating main while preserving all their
own file byte associations. Their hosted validation remains independent.
