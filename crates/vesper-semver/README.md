# Source interfaces and release-version policy

`compare(old, new)` conservatively classifies the supported source-interface fragment as patch, minor, major or unknown. Its compatibility result remains independent of package version numbers. See the report under `docs/parallel-results/semver.md` for the interface fragment and its limits.

`check_release(old, new, old_version, new_version)` computes that comparison itself and checks the proposed version change. It returns a versioned `ReleaseReport`, or a typed `ReleaseVersionError` identifying an invalid old/new version.

Release policy 1:

- Candidate SemVer precedence must strictly increase. Build metadata is preserved in reports but ignored for precedence; changing only metadata is not a release bump.
- Patch, minor and major classifications require at least that numeric component increase. Larger bumps are allowed.
- The same conservative numeric rules apply before 1.0.0. There is no automatic exception allowing a breaking `0.4.0` to `0.5.0` transition; that requires a numeric major bump under this policy.
- A prerelease on either side is explicitly unsupported and cannot pass this policy.
- Unknown/unsupported interface contracts never pass, including with a major version bump.

A known breaking interface may pass an intentional major release. Its nested comparison still records incompatibility, counterexample descriptions and required reborrow adapters. Acceptance does not synthesize adapters or claim existing source callers remain compatible. It is only a version-policy decision, not package authentication, a proof certificate or permission to publish.
