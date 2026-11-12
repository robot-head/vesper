Offline audits of a **caller-selected local catalog** against the exact reachable
Vesper and native selections in a calculus-validated `Solution`. Package paths
are declared dependency reachability, not call-graph/CVE exploitability, observed
installed native binaries, source provenance, live feed freshness, or catalog
authenticity. `Clean` means no matching policy violation/unknown declaration in
these supplied inputs; an empty or stale catalog cannot establish CVE absence.

`parse_solution`, `parse_catalog`, `parse_policy` accept bounded JSON and reject
duplicate keys at every nesting level and unknown schema/DTO fields. `audit`
revalidates programmatic DTOs and the exact solution. Inactive optional/feature
edges and unused selections do not generate findings. Cycles terminate. Findings
include one shortest deterministic path from each reaching root, with explicit
Vesper/native identity domains. Invalid inputs and exhausted budgets return
errors, never partial clean reports.

Reports bind the complete solution, catalog, policy and limits DTO contents using
separate BLAKE3 domains under `vesper.loom.offline-audit/1`. These are input
context digests, not authenticated provenance. Struct/map serialization is stable;
vector order is retained, so reordered source inputs can have different contexts
while producing identical findings.

Catalog schema 1 uses explicit SemVer intervals, **introduced inclusive, fixed
exclusive**, with required `fixed` (version or null) and `include_prerelease`
fields per interval. Build metadata does
not affect SemVer precedence. `fixed: null` has no upper bound. No OSV/npm range
or event syntax is implicitly interpreted. Overlapping/duplicate/empty intervals,
duplicate advisory IDs and duplicate native license identity/version claims are
rejected. Native license claims use the exact selected version including build
metadata; they are local declarations, not installation/license inspection.

License expressions support case-sensitive `WITH`, `AND`, `OR`, parentheses,
with precedence WITH > AND > OR. WITH attaches only to a single license ID.
AND needs all grants; OR needs one complete granted branch. Every branch is
parsed before evaluation: unknown identifiers even in an unused OR branch yield
`Unknown`. Freeform text, absent licenses, LicenseRef/DocumentRef, legacy `+`,
unknown SPDX IDs/exceptions, malformed syntax and unsupported expressions remain
`Unknown`; they never assert a valid SPDX license or silently pass.

Supported SPDX license IDs: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC,
MPL-2.0, GPL-2.0-only, GPL-2.0-or-later, GPL-3.0-only, GPL-3.0-or-later,
LGPL-2.1-only, LGPL-2.1-or-later, LGPL-3.0-only, LGPL-3.0-or-later,
AGPL-3.0-only, AGPL-3.0-or-later, Unlicense, Zlib, BSL-1.0, CC0-1.0.
Supported exceptions: LLVM-exception, Classpath-exception-2.0,
GCC-exception-3.1. This is a documented finite subset, not the entire SPDX list.
Policy grants are exact single IDs or explicit `ID WITH exception` pairs;
allowing a base license does not automatically allow every exception combination.

`Outcome` is `clean`, `findings` (known policy violations), or `unknown` (one or
more unknown license assessments). Unknown takes precedence in mixed reports;
`has_policy_violations` separately retains known violations. Vulnerability
findings are emitted at or above the policy's explicit severity threshold.

Limits bound serialized input bytes, JSON string/node/depth, packages, roots,
declared edges, catalog entries/ranges, license tokens/depth, retained path
nodes/depth, finding count, and logical work. Existing calculus validation is
preceded by a conservative size-derived work estimate; it is not instruction or
wall-clock metering. JSON recursion protection remains enabled. License limits
have hard depth/token ceilings to avoid unchecked recursive parsing/evaluation.

Run the standalone example:

```sh
cargo run --manifest-path crates/vesper-loom-audit/Cargo.toml --example audit -- \
  Loom.lock advisories.json policy.json
```

The example emits a versioned JSON report and exits 0 for clean, 1 for findings,
2 for unknown, and 3 for invalid input/budget/I/O errors. No network is used.
See the example fixtures under `examples/` for catalog and policy schemas.
