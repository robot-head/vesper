# Hygienic parameters in source macros — issues #157/#159–162

Explicit Forge source compilation now accepts macro-bearing nongeneric functions
with bounded plain named parameters. Actual in/inout/sink semantics, parameter
types and final linear checking are preserved. The default count is8, with the
existing syntax HARD limit32. Projected parameters, generics and unsupported
expression categories remain explicit refusals. Ordinary support functions retain
their existing parameter forms and count behavior.

The privately parsed source retains actual header identifier spans. The current
host validates exact source bytes and source-bound FunctionId, appends actual
parameter tokens after the parsed body and seeds fresh source-ranked binding
scopes. Parameter records are private-construction metadata; public header clones
cannot replace them. The old body-only API and public lexical Term/fragment wire
structures remain unchanged. Initial scopes, candidate scans and retained copies
have finite prepaid budgets.

Parameters and body references alpha-rename together. Rewriting only the body,
or preserving parameter spellings, would let a parameter hijack a definition-site
global after source serialization. Header edits replace only actual identifier
tokens, preserving comments, pass/type/effect text and every other header field.
Source maps retain actual byte provenance, without promising identical lexemes.
The complete final expanded source passes the actual ordinary checker, lowered
body AST comparison, compiler-derived authority gate and normal IR validation.

Public authority::derive retains strict original-name header equality. The source
compiler's private path compares the complete checked header against the exact
expected alpha-renamed header derived from sealed original/lowering records.
Name/order/span/count/source/function association is checked, and pass, types,
projections, generics, result and effects remain exact. No public fingerprint
override, arbitrary World, dummy function body or cached approval is introduced.
Duplicate original parameter names reject before renaming can repair them.

## Coordination and actual evidence

Umbrella #157 and exclusive claims #159–162 preceded source writers. Exact APIs
and ownership were published in params41-contracts.md before four worktrees
started. Fresh open issues and actual external PR files found no Forge/syntax
collision. The live design remains41sections/AppendixA, modified
2026-11-12T09:18:30.204Z; §16.4 was reread. Work preserved core AST/IR/native ABI,
linear exhaustion, default original-source semantic providers and AST-only Lean.

The fresh issue check detected new external #158 before writers. Bazel/build
files, all workflows and scripts/test-*.sh were excluded from this batch. Exact
new Rust targets and actual-binary Python invocation were posted on #158 for
equivalent native CI wiring. Claude wave-five #121–136 remains reserved. No
external integration, build or workflow files were changed.

Independent review found a new temporary duplicate-ID set whose node allocation
was outside the authority metadata preflight. Root replaced it with a checked,
prepaid slice scan, and the reviewer confirmed the fix. No semantic or authority
bypass was found. An acceptance fixture's Rust-style `let mut` was corrected to
actual Vesper `let`; actual inout checking remains covered. Three obsolete
parameter-unsupported negatives became genuinely unsupported projected/generic
forms, retaining their rejection assertions.

Local integrated verification on Rust1.99.1 passed101tests in9root suites, zero
ignored, including43library,8new compiler,5new actual CLI and45existing
authority/lowering/source CLI/query/provider/preview checks. Syntax independently
passed71tests/7suites; pure Forge passed49tests/7suites, both zero ignored.
Strict workspace all-target Clippy, formatting and whitespace checks passed.

The new actual executable harness passed65commands,9stdio sessions,47JSON-RPC
requests and9separate expanded normal-program runs to42. It checks global/parameter
capture, both hygiene directions, initializer scopes, SelfKw, all supported
passes, real linear duplication/leak rejection, exact header/argument anchors,
current helper-body authority, current unsaved previews and formatting
conservation/idempotence. A standalone Rust harness passed all5tests, including
8expanded runs. Legacy macro acceptance passed115commands/11runs. The final root
binary also passed existing preview20commands/8sessions/70requests and
Stylus36commands/3sessions acceptance.

Current original-source diagnostics and hover remain unexpanded/unavailable
after successful explicit previews. Preview does not read URI files or run
components, proofs or generated programs. Executions above were separate explicit
normal CLI commands over actual checked expanded output.

## Pending and remaining scope

This draft stacks on PR156/142/115/110; retarget after the parents land and observe
the final native gates. New Rust targets are discoverable by existing workspace
CI; external #158 owns the Bazel migration and additional actual Python wiring.
Local results do not claim hosted macOS/Windows execution for this new head.

This extends the existing fixed pure Expr profile. Parameter projections, other
macro categories, source Tier2, cross-package linking, minimal per-function
rechecking, implicit original-source macro semantics and latency/RSS guarantees
remain future work. The broader design implementation goal remains active.
