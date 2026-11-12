Offline local-catalog resolution with deterministic newest-first, bounded
depth-first backtracking. `resolve(&ResolveRequest)` fixes the supplied root
manifest, unifies additive features through cycles, selects one version per
package, and returns a `vesper_loom_calculus::Solution` only after full validation.
Catalog schema version 1 uses the calculus manifest and native selection DTOs.

`max_decisions` counts explored non-root package assignments. A root-only graph
needs zero decisions. Native candidates carry no dependencies and are selected
directly against all active SemVer constraints. `BudgetExhausted` means unknown,
distinct from an exhaustively searched `Unsatisfiable` result. Encountered
conflicts are structured diagnostics, not minimal incompatibility proofs.
Malformed or duplicate catalog identities are errors even when unreachable.
All input collection ordering is normalized. Selected packages with nonempty
optimization requests, positive reproducibility-attestation requirements, or
namespace-vetting requirements fail as unsupported policies. An unsupported
candidate can be avoided by selecting a different version. If no supported
solution exists and a candidate was blocked by an unsupported policy, the
result is `UnsupportedPolicy`, not a claim of unsatisfiability. Serde defaults
namespace vetting to true when a policy table exists; offline callers must omit
that table or explicitly disable vetting to request unverified resolution.

This milestone is not Resolvo CDCL, Max-SMT, a performance guarantee, or an
authenticated package manager. It does not access registries, execute code,
download packages, prove namespace ownership, check CVEs, or verify attestations.
Native identities and versions use only the calculus SemVer namespace, not
Debian/RPM versions or platform-specific ABI constraints. No default features
are implicit: requested features are explicit and transitively additive.

The returned DTO can be serialized as an offline solution snapshot. It is not
the specification's complete cryptographic Loom.lock: there are no source,
interface, provenance, builder, or proof digests. Capability checks constrain
supplied declarations; they do not audit actual compiler bodies, privileged
handler installation, FFI, or macro emission. Source confinement needs that
separate compiler audit; public effect rows do not establish it.
