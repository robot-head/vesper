# Release-version policy

Coordinator work outside the seven active implementation lanes. `vesper_semver::check_release` computes the actual interface comparison and checks a proposed pair of SemVer versions. `vesper semver old.vhir new.vhir --old-version X --new-version Y` exposes the gate; omitting both flags preserves the existing source-compatibility behavior and JSON format.

Policy/report version 1 requires a strictly increasing precedence and a numeric bump at least as large as the known Patch/Minor/Major classification. Build metadata alone does not increase precedence. Higher bumps are allowed; numeric pre-1.0 components receive no special breaking-change exemption. Prerelease transitions are explicitly unsupported. Unknown interface contracts fail even with a major bump.

An accepted intentional major release remains incompatible with old callers where the comparison says so. Breaking witnesses and explicit reborrow-adapter requirements stay in the nested report; the gate does not synthesize adapters or claim substitutability. It performs no publication, provenance verification or binary ABI check.

Validation: all 34 SemVer library tests (nine new) and 25 package/graph/locked/release CLI tests (five new) passed. Workspace formatting and strict all-target Clippy passed. Tests cover insufficient/excessive bumps, ordering/build metadata, prerelease/pre-1.0 policy, unknown interfaces, migration adapters, flag errors and the unchanged default comparison mode. Compiler and backend code remain unchanged.
