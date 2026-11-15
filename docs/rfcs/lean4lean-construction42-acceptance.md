# Construction packet acceptance (#411)

Source is authored; actual compilation, both full matrices and strict Clippy
remain with the coordinator. This lane has run only direct rustfmt, whitespace
checks and a static extraction comparison. No Cargo or Lean process was run.

The original foundations entry point and the new construction entry point call
one shared runner using a closed Packet enum. The private specs contain exact
module lists, required theorem names, evidence labels and output module names.
There is no runtime arbitrary-module choice, expected acceptance input,
alternate axiom policy or weakened checking mode.

Foundations retains its exact eight-module closure. Construction selects the
minimal seven-module closure: actual List/Level, frozen Semantics/ListProofs,
OrderedInsert/VarInsert, and Lean4LeanConstruction42. An explicit source list
replaces the old directory scan so the newly added construction files cannot
accidentally enter the frozen foundations matrix. Each actual import closure
must still equal its entire selected source set before compilation.

All 24 original required theorem names are preserved, in order. The parent
catalog remains derived from its actual fixed sources, without a golden count.
The new catalog likewise audits every authored theorem in its selected closure,
requiring the contracted helpers and all final compiled computational witness
names supplied by both proof authors. Every actual alternate-proof alias is
added to the audit roots.

Static comparison confirms that seven complete blocks are byte-identical to
the passing parent: provenance validation, source-command lexing, artifact
hashing, full projection/partition validation, root/alias union, positive strict
audit validation and source binding. The complete canonical-clean versus
forbidden-alternate-proof control is also byte-identical. Original/projection
missing and corrupt controls remain, with only the closed packet module names
substituted. Source mutation, provenance, tool identity and before/after hash
checks remain intact.

The same standard axiom policy, 128-root audit batches, 300-second whole-matrix
budget, bounded regular-file capture and artifact limits apply separately to
each packet. Actual bounded evidence is still printed before refusal controls.
No budget, scheduling, kernel, profile, vendor, proof, auditor or Cargo source
is changed by this lane.

The existing parent result was eight modules and 49 authored roots. That result
must be reproduced by the extracted runner, alongside a genuine complete new
construction run, before integration is declared verified. This source-only
report is not proof that either post-extraction matrix has passed.
