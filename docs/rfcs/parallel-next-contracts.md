# Independent implementation lanes alongside wave three

Baseline: `1ffd0aec27dfcc24c5564585ae9068255490bf61`, a descendant of the
structured Lean emitter integration. Wave three is owned by the user's external
agent. Its syntax/checker/interface/Pact, native source execution, Forge runner,
source simulation, SIMD proofs, fibers and Prism paths are read-only here.

| Lane | Exclusive production paths | Required boundary |
| --- | --- | --- |
| Bridge sys-specs | `crates/vesper-ffi/**` | Versioned bounded declarative C ABI descriptors, target-specific layouts, validated foreign returns; preserve wire APIs |
| Strata slab | `crates/vesper-alloc/**` | Bounded real allocations, explicit owners, origin/generation checks, safe shutdown; preserve regions/collections |
| Aura delta | New `crates/vesper-aura/**` | Bounded keyed reconciliation, sparse properties/slots, explicit teardown and backend failure recovery |
| Ignis lifetimes | `crates/vesper-ignis/**` | Existing graph plans plus owned command submission/completion and allocator/resource retention |
| Loom audit | New `crates/vesper-loom-audit/**` | Validated reachable dependency graph, local advisory/version matching, explicit license policy and Unknown results |
| Artifact admission | New `crates/vesper-loom-admission/**` | Recheck current configured signatures/policy and actual artifact bytes before any CAS import |

Each lane owns only its matching new report under `docs/parallel-results/`.
Root manifests/lock, root module/CLI registration, existing CI/global docs and
shared representations remain coordinator-owned. Read dependencies without
editing another lane. New crates may use a temporary standalone workspace for
checks; remove it at integration and retain all existing registrations.

No compiler-derived source ownership, portability or authority claim follows
from a host library. Aura's thread-local Rust API is not `eff [UiThread]` typing;
Rust consuming owners can still be dropped or forgotten. Ignis fixture completions
exercise the protocol, not a D3D12 queue or GPU. Foreign-call descriptors do not
implement source lowering or hardware isolation. Slab claims require actual
allocation/thread evidence, with unsupported transfers rejected explicitly.

Audit catalogs and expected artifact identities are explicit caller trust inputs.
Local matching does not authenticate a CVE feed, namespace, source contents,
native installation, independent builder or execution environment. Admission
never accepts an old public verification report as authorization and repeats
current byte/policy checks; it does not automatically approve a build cache,
publish a package or install system software. Failed consuming operations return
their owners, and rejected admission does not initialize or mutate a store.

Arithmetic remains `vesper.arith.checked/1`; interface semantics version 2 and
fiber ABI v2 stay fixed. The user-approved field premise `k_i ⊑ m_i` remains in
force. Lean source extraction constructs syntax nodes and renders once.

Use isolated worktrees/build targets with one build job, debug information and
incremental caches disabled. Deliver working supported slices, clear rejection
of unsupported inputs, focused positive/negative tests, formatting, strict
Clippy and a commit. The coordinator reviews owner/failure paths, integrates
root wiring and reruns combined validation before publication. Cross-compilation
is never reported as runtime execution; no performance or soundness theorem is
claimed without evidence.
