# Offline dependency audits

`vesper audit` checks a validated, nonempty reachable Loom solution against a
caller-selected local advisory catalog and license policy. It emits JSON and
runs no package code, network requests or file writes:

```sh
cargo run -- audit crates/vesper-loom-audit/examples/solution.json \
  --catalog crates/vesper-loom-audit/examples/catalog.json \
  --policy crates/vesper-loom-audit/examples/policy.json
```

This fixture deliberately reports an illustrative native advisory and exits
unsuccessfully. It is synthetic test data, not a real CVE assertion.

| Outcome | Meaning | CLI exit |
| --- | --- | --- |
| `clean` | No matching violations or unknown license declarations in the supplied inputs | 0 |
| `findings` | Known policy violations, including affected versions or denied licenses | 1 |
| `unknown` | Missing, malformed or unsupported license assessments; known violations remain visible separately | 1 |
| Input/budget error | Invalid snapshot/catalog/policy or exhausted limits; no audit conclusion | 1 |

Every finding identifies the exact selected version and distinct Vesper/native
identity domain, with one shortest deterministic dependency path from each
reaching root. Unreachable selections and inactive optional/feature edges do not
produce findings. These are dependency paths, not call-graph exploitability
analysis. Native selections describe the lock snapshot, not installed binaries.

Catalog schema 1 uses explicit inclusive `introduced` and exclusive `fixed`
SemVer versions. `fixed` must be present (a version or `null`), and each range
explicitly specifies `include_prerelease`. Advisory comparisons ignore build
metadata; native license records match exact selected versions. Duplicate IDs,
overlapping ranges, malformed versions and unsupported schemas fail closed.

The policy sets a severity threshold and exact allowed license/exception grants.
The documented SPDX subset supports `WITH`, `AND`, `OR` and parentheses; every
branch is parsed. Freeform text, unknown identifiers and absent licenses remain
Unknown. Allowing a license does not automatically allow exception combinations.
See the [library contract](../crates/vesper-loom-audit/README.md) for the supported
identifier list, schema and complete budgets. CLI files are each capped at
1 MiB before parsing, with stricter structural/work/path limits inside the library.

Reports bind the complete solution, catalog, policy and evaluation limits using
versioned content digests. A catalog label is not a substitute for those inputs.
Digests do not authenticate the catalog, policy or source. A clean result against
an empty or stale advisory catalog cannot establish that a package has no CVEs.
Feed authentication/freshness and trusted orchestration remain separate inputs.
