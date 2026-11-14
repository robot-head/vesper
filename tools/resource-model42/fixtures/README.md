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
