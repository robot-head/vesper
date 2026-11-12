# Declared-permission upgrade guard

Coordinator work outside the seven active implementation lanes. `vesper_loom_calculus::check_upgrade` validates two complete solution snapshots, requires matching nonempty root identities, and compares reachable package requirements/ceilings, dependency-edge budgets and unsafe-FFI policy. Added permissions are rejected; removals and new pure dependencies are allowed. Typed effect strings remain distinct, and per-package comparison catches authority moved between dependencies despite an unchanged root union.

`vesper resolve --baseline old.lock` exposes this guard on the freshly resolved candidate before serialization or output replacement. Failed checks preserve an existing output snapshot. Successful guarded JSON includes the complete upgrade report; default resolution without the optional flag is unchanged. This guard does not search for a less-privileged alternative candidate or provide an override flag.

Validation: 12 new calculus regressions plus its 17 existing tests and doctest passed; five new upgrade CLI tests plus 18 existing package/locked/release tests passed (53 total). Workspace formatting and strict all-target Clippy passed. Coverage includes metadata movement, unused edge-budget widening, FFI relaxation, capability reductions, invalid inputs, unrelated roots and unchanged output on rejection.

The report concerns supplied declared metadata only. It is not a compiler authority audit, signature/source/provenance verification, SemVer/ABI check, version-progression policy or complete automatic `vesper update` implementation. No package code, builds or tests run during this command.
