# Macro provenance (COMP-03)

Claim: #840. Scope: the Tier1 source macro expander (`src/forge_frontend/`),
`vesper forge`, and the `vesper proof-audit` admission gate. Macro syntax,
hygiene and lowering (§16.4, owned by the Codex agents) are unchanged.

REVIEW-STATUS COMP-03 asks to substantiate macro provenance: every expanded
item should record its macro origin through to diagnostics, the IR and Lean
extraction, and the admission gate should reject proofs about expanded code
whose provenance is missing.

## What every expanded artifact now carries

`ExpandedChecked::provenance()` is a `vesper.forge.provenance/1` table
(`src/forge_frontend/provenance.rs`):

- one `ExpansionRecord` per macro invocation: macro name, invocation span in
  the original source, host function, and every expanded-source range whose
  tokens that expansion generated (taken from the lowering's own
  `SegmentOrigin::Generated`, not reconstructed from text);
- one `FunctionProvenance` per IR function (`ir::Program::functions`), with
  origin `source` or `expanded`, the ordinals of its expansions, and a BLAKE3
  digest over its record, its expansion records and both source hashes;
- a table digest over the whole canonical table.

Construction fails closed: an IR function without a top-level source
declaration, or generated code from an unknown expansion, refuses the
expansion (`Stage::Lower`). `ProvenanceTable::verify` recomputes every digest,
checks that the per-function expansion lists are exactly the records naming
that host, and, given the sources, checks both hashes and every range.

`vesper forge check --json` reports the table under `provenance`.

## Diagnostics

When checking or IR validation of the expanded program fails,
`CompileFailure::origins()` is aligned with `diagnostics()`: each entry gives
the expanded span, the original span, whether the code was `source` or
`generated`, and the macro name. A type error inside `wrapping_add($x, true)`
from `bad!(n)` is attributed to the invocation `bad!(n)` and macro `bad`; an
error in ordinary code keeps its original source span. `forge check --json`
reports these as `origins` on rejection.

## Lean extraction

`vesper forge lean SOURCE [--provenance FILE]` runs the ordinary extractor
(`lean::extract_report`, unchanged) on the checked expanded program and adds,
inside `namespace Vesper`:

- `«d$macro_provenance» : String := "<function digest>"` for every extracted
  declaration `d` owned by an expanded function (the function itself, its
  `f$…` companions and `$block_f_N` helpers);
- `«vesper$macro_provenance» : String := "<table digest>"`.

These are ordinary kernel-checked definitions, so the gate reads them from
the replayed environment, not from source text. `--provenance` writes the
manifest (never replacing an existing file) before any Lean is emitted.
Extraction refuses if an expanded function has no Lean declaration.

## Admission gate

The auditor (`tools/proof-audit/Audit.lean`) now also reports, per theorem,
its transitive package dependencies, and every package constant whose name
ends in `$macro_provenance` with its string value (anything other than a
string-literal definition is refused). `vesper proof-audit` then applies, for
every audit (CLI, `publish`, generated audits):

- function markers require exactly one module marker in the same namespace;
- a theorem that depends on a marked constant is refused unless
  `--macro-provenance FILE` names a manifest; the manifest must verify, its
  digest must equal the module marker, every marker must match an expanded
  function record, and each dependency is reported with its function and
  macros under `macro_provenance` in the JSON report;
- with a manifest, a dependency named after one of its expanded functions but
  lacking its marker (provenance stripped) is refused.

Artifacts without markers and without a manifest are audited as before.

## Evidence

- `tests/forge_provenance.rs`: table completeness against the IR, generated
  ranges equal to the source map's generated segments, determinism and JSON
  round trip, tamper/incompleteness refusals, diagnostic origins (generated
  and source), Lean markers, the owner-name rule.
- `tests/forge_provenance_cli.rs` (real Lean 4.35.0, skipped without it;
  Linux and Windows in CI): `forge lean` manifest equals `forge check`'s
  table and is never overwritten; a theorem about `Vesper.main` is refused
  without the manifest and admitted with it, reporting `keep` and the
  transitively reached `twice` via `Vesper.doubled`; a theorem about the
  unmarked `plain` needs no manifest; a forged manifest, stripped function
  markers and a fully unmarked rebuild are refused.
- `src/proof_commands.rs` unit tests for malformed markers and the
  no-manifest rule.

## Remaining gaps

- The gate can only see expanded code that carries markers. Running the
  ordinary `vesper emit --emit lean` on the expanded text yields an unmarked
  module; theorems about it are proofs about that ordinary source and are not
  claims about the macro-bearing original. Binding a proof to the original
  source therefore requires the `forge lean` path and its manifest.
- The owner rule follows the extractor's current naming (`f`, `f$…`,
  `$block_f_N`); a new extractor naming scheme must be added to
  `provenance::lean_owner`.
- Provenance is per function. Statement-level attribution inside Lean terms
  would need span information in the extractor (`src/lean.rs`, outside this
  lane).
