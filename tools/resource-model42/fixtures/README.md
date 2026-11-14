# Fixed kernel snapshot controls (#358)

These are fixed handwritten Lean controls, isolated from the production model.
No ordinary model or acceptance module imports them. The exporter receives the
actual compiled module inventory, not expected answers or acceptance flags.

| Module | Public theorem roots | Boundary exercised |
| --- | --- | --- |
| `ResourceSnapshotTotal` | `ResourceSnapshotTotal.total_correct`, `ResourceSnapshotTotal.tree_correct` | Genuine total Nat/tree recursion, exact retained safe declarations, compiler-helper accounting, final strict replay. |
| `ResourceSnapshotUnsafe` | `ResourceSnapshotUnsafe.anchor` | An unrelated safe root cannot hide an explicit unsafe declaration. |
| `ResourceSnapshotPartial` | `ResourceSnapshotPartial.anchor` | Source partial recursion's opaque public facade cannot become projected proof authority. |
| `ResourceSnapshotFakeHelper` | `ResourceSnapshotFakeHelper.safe_suffix_correct` | A safe user declaration named with `_unsafe_rec` must be retained exactly. |
| `ResourceSnapshotFakeUnsafe` | `ResourceSnapshotFakeUnsafe.anchor` | An unsafe declaration named as an existing safe parent's helper, without compiler association, must be refused. |
| `ResourceSnapshotAxiom` | `ResourceSnapshotAxiom.exposed` | Honest retention is not proof approval: the final strict axiom policy must reject the project axiom. |

The unsafe and divergent declarations are metadata/refusal controls; they are
never executed. None of the fixtures contains a generated source template,
projector expected-result input, native-decision shortcut, or fake model answer.
Execution and actual projection/audit outcomes are recorded separately after
using the coherent production snapshot API.

## Actual fixed-source validation

The source writer compiled all six modules sequentially with the coordinator's
short Lean compiler lease, using the actual pinned
`/home/agent/.local/share/vesper-lean/bin/lean` (Lean 4.24),
`LEAN_NUM_THREADS=1`, and a private output root. Every compile exited 0 with an
empty module log. The source checkpoints are `83f9082` and `977d0d1`.
Artifacts and logs are outside the checkout at
`/tmp/resource-kernel42-controls-v1`.

A separate fixed handwritten metadata probe imported those compiled controls
and inspected the actual ConstantInfo records:

| Declaration | Actual form and safety metadata |
| --- | --- |
| `ResourceSnapshotTotal.total`, `.leaves` | Safe definitions, `unsafe=false`, `partial=false`; each has a compiler-generated helper with `partial=true`. |
| `ResourceSnapshotPartial.sourcePartial` | Opaque facade, `unsafe=false`, `partial=false`; its generated helper has `partial=true`. |
| `ResourceSnapshotUnsafe.explicitUnsafe` | Definition, `unsafe=true`, `partial=false`. |
| `ResourceSnapshotFakeHelper.user._unsafe_rec` | Safe definition, `unsafe=false`, `partial=false`, with the actual safe `user : Nat` parent. |
| `ResourceSnapshotFakeUnsafe.ordinary._unsafe_rec` | Definition, `unsafe=true`, `partial=false`, with an actual safe `ordinary : Nat` parent. |
| `ResourceSnapshotAxiom.projectFalse` | Axiom, `unsafe=false`, `partial=false`. |

The actual `#print axioms` results were empty for both Total roots and the
FakeHelper root, and exactly `[ResourceSnapshotAxiom.projectFalse]` for
`ResourceSnapshotAxiom.exposed`. The probe exited 0; its full output is
`/tmp/resource-kernel42-controls-v1/metadata.log`.

These observations verify that the controls exercise distinct real metadata
conditions. This lane has not run snapshot export, final strict artifact
replay, or axiom-policy acceptance/rejection. Those outcomes require the real
coherent exporter and the independent integration lane. No Cargo command was
run, no `.olean` is tracked, `git diff --check` passed, and the compiler lease
was released immediately after inspection.
