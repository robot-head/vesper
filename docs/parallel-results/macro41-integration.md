# Explicit source expression macros — issues #103–109

The ordinary CLI previously rejected declarative source macro definitions. The
explicit `vesper forge check SOURCE` and `vesper forge expand SOURCE` commands now
compile the supported Tier1 expression profile through current host-owned
quotations, whole-function hygiene resolution, identity-based core AST lowering,
the full ordinary checker, compiler-derived authority and ordinary IR validation.
Only the private `ExpandedChecked` artifact represents success. The original
macro-bearing source never receives a manufactured `CheckedSource`.

Example:

```vesper
macro keep($value:expr)->expr { quote { $value } }
fn main()->i64 { let n=42; keep!(n) }
```

```sh
vesper forge check example.ves
vesper forge expand example.ves > expanded.ves
vesper run expanded.ves
```

Check emits bounded `vesper.forge.source/1` JSON. Expand emits exactly the fully
checked expanded bytes. Rejections never publish maps or a partial accepted
artifact; normal checker diagnostics may include the rejected candidate for
diagnostic use. Existing check/run/LSP/Lean paths do not implicitly execute macros.
The root dependency disables Forge default features, keeping Wasmtime out of the
default CLI graph.

Each fixed Expr parameter is imported from actual source, each expansion obtains
current host slots/scopes, and entire macro-bearing bodies resolve together.
Global template names retain definition-site claims; resolved local BinderIds
are alpha-renamed while serializing the explicit core AST. The parsed final body
must match that expected AST, ignoring spans only. All ordinary headers and
support declarations retain their original source text. Integer inference keeps
the normal i64 default; trailing semicolons and initializer scopes are preserved.

Authority derives from the actual final checked bodies and IR call targets,
including transitive helpers and concrete implicit runtime/overflow failures.
Declared effect rows alone do not establish performed effects. Generated uses
must satisfy both the configured macro grant and caller ceiling. The policy
schema is `vesper.forge.policy/1`; both grants default empty. Audits are conditional
local policy evidence, with each invocation explicitly referring to the audit of
its whole expanded function. They do not establish package confinement or registry
provenance. Original and expanded hashes remain separate; map spans are byte
provenance anchors, not promises that mapped lexemes have identical text.

Public finite limits guard source/output bytes, metadata, maps, identifiers and
aggregate staged work. Typed parsing, repeated copies, lowerings, registry clones,
map storage and authority path/witness envelopes are charged before their
corresponding allocation or traversal. Conservative stage reservations can reject
otherwise valid larger programs. These are logical guards, without an RSS,
latency or user-program termination claim.

The independent structured WASI foundation uses a new versioned quotation world,
real committed component and raw-only cache. It is separate from source checking;
it cannot construct `ExpandedChecked`. See macro41-structured.md for exact runtime
and cache evidence. Source Tier2 linking is not implemented by this batch.

## Coordination and verification

Claims #103–109 and the published macro41-contracts.md ownership/API agreement
preceded implementation. The live design metadata was rechecked and remains
2026-11-12T09:18:30.204Z, with 41 sections and Appendix A; §16.4 was reread. Fresh
open-board and actual PR filename checks leave external Warp #45/#101 reserved.
Main remains 47a093ea9464af47d8cb4cebb43d5b15e001b11a at this checkpoint. Work used
separate worktrees and private one-job caches.

Actual source acceptance against integration d67244e passed all eight CLI tests,
115 Python harness commands and 11 explicit executions of normal expanded source.
Cases include results of 42, both capture directions, global pre-resolution,
initializer scope, repeated spans, real E0303 linear duplication rejection,
trailing-unit E0200, current helper-body authority changes and implicit failures,
malformed/unsupported input and bounded CLI reads. Strict root/pure-host Clippy
passes. An independent read-only review found no actionable correctness or
authority bypass.

The integrated local workspace passed 2,305 tests in 295 suites, no ignores, on
Rust1.99.1 with actual Lean4.24.0. Strict workspace all-target Clippy and formatting
passed; the five standalone Lean files also checked. The existing source-tooling
acceptance passed its 192 measured callbacks, editor acceptance passed19sessions,
causal diagnostics passed17CLI/16stdio checks, and recovery passed15stdio/3batch
checks against the integrated binary. The final platform cleanup adjustment was
then covered by the actual seven cache tests and strict runner Clippy.

Native CI uses newly released Rust1.100. Its Linux/macOS workspace jobs exposed
the pre-existing Warp TCP malformed.rs129 needless_range_loop, already fixed by
the external owner's reserved PR101. Coordination was posted there; no Warp
files were edited or merged by this batch. Windows additionally caught a new
structured-cache cleanup lint; the helper now restores owner access with Unix
PermissionsExt and uses narrowly documented Windows readonly-attribute resets.
Native checks are rerun on that final fix. Final PR validation records the
observed status; local1.99 results are not represented as passing1.100 CI.

## Remaining design scope

This is the fixed, nonrecursive Expr quotation profile for nongeneric no-argument
macro-bearing functions and one explicitly associated source package plus the
builtin prelude. Ordinary supported helper functions can have parameters.
Other grammar categories, repetitions, nested/recursive invocations, derives,
generated items, source Tier2 linking, cross-package linking, incremental macro
queries, semantic completion and proof lenses remain unfinished. Unsupported
contexts fail explicitly. The overall design implementation goal stays active.
