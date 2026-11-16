# Source interfaces and release-version policy

`compare(old, new)` conservatively classifies the supported source-interface fragment as patch, minor, major or unknown. Its compatibility result remains independent of package version numbers. See [package workflows](../../docs/package-workflows.md) for the interface fragment and its limits.

Resolved interface DTOs can also represent an inclusive constant `i64` range as
`{"kind":"i64_range","min":0,"max":10}`. Range inclusion supports
contravariant input and covariant result comparisons; `inout` and other
invariant positions remain invariant. Reversed bounds produce an unknown
comparison. Source interface export does not yet emit these refinements, and
this fragment does not decide general predicates or synthesize SMT witnesses.
Incompatible direct input or result ranges include a concrete distinguishing
`i64` value in the human-readable witness; it is not a checked caller program.
The analogous `{"kind":"u64_range","min":0,"max":10}` represents an
inclusive `u64` interval; its full domain is 0 through `u64::MAX`. Incompatible
direct unsigned parameter or result intervals include a distinguishing value
in the human-readable witness, not a checked caller program.

Resolved interface DTOs may attach a constant `ceiling` to an effect capability.
By default this is an upper bound: lowering it is compatible and raising it is
breaking. Comparing a bound to an absent bound is unknown. Setting
`"grade_relation":"exact"` makes the value invariant: lowering, raising,
removing the capability, or replacing equality with an upper bound is breaking.
An exact grade with no value is malformed and yields unknown. Source export
does not yet produce grades; symbolic grades and SMT comparisons are unsupported.

`check_release(old, new, old_version, new_version)` computes that comparison itself and checks the proposed version change. It returns a versioned `ReleaseReport`, or a typed `ReleaseVersionError` identifying an invalid old/new version.

Release policy 1:

- Candidate SemVer precedence must strictly increase. Build metadata is preserved in reports but ignored for precedence; changing only metadata is not a release bump.
- Patch, minor and major classifications require at least that numeric component increase. Larger bumps are allowed.
- The same conservative numeric rules apply before 1.0.0. There is no automatic exception allowing a breaking `0.4.0` to `0.5.0` transition; that requires a numeric major bump under this policy.
- A prerelease on either side is explicitly unsupported and cannot pass this policy.
- Unknown/unsupported interface contracts never pass, including with a major version bump.

A known breaking interface may pass an intentional major release. Its nested comparison still records incompatibility, counterexample descriptions and required reborrow adapters. Acceptance does not synthesize adapters or claim existing source callers remain compatible. It is only a version-policy decision, not package authentication, a proof certificate or permission to publish.
