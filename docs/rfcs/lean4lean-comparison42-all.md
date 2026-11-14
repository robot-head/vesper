# Actual map-all traversal and lookup bridge (#445)

The fixed `MapAll.lean` module compiled successfully on pinned Lean 4.24 with
empty stdout/stderr. It contains the three frozen public APIs and one public
list-loop proof helper. This is compilation evidence; complete projection,
transitive axiom policy and comparison-packet acceptance remain coordinator
gates. No comparison, primitive Level equality or whole-checker result is
claimed by this module.

The public claim and pushed comparison contract preceded source. Only
`lean/Lean4LeanLevel42/MapAll.lean` and this report were changed. The source
imports only `Lean4LeanLevel42.MapValue`; no parent, vendor, runtime, Cargo,
auditor/projector, profile or CI source changed. No Cargo ran in this lane.

## Actual proof boundary

`norm_all_eq_toList_all s f` proves equality between the actual `s.all f`
and the predicate's `List.all` over the actual `s.toList`. Its `change` step is
definitionally checked against the actual TreeMap/DTreeMap `all`: constant
product iteration and the dependent iteration elaborate to the same curried
tree traversal. The proof then uses pinned
`Std.TreeMap.forIn_eq_forIn_toList` and a direct induction over the real list
short-circuit `forIn` loop. A false predicate stops immediately; a true head
continues with the exact tail. No map or traversal implementation is replaced.

`norm_all_eq_true_iff s f` derives the exact lookup characterization using
`List.all_eq_true` and the actual comparator-specific
`Std.TreeMap.mem_toList_iff_getElem?_eq_some` theorem with explicit
`List.compareLex Name.cmp`. It has no MapWF, sorting, semantic-result or
assumed-traversal premise. `norm_all_lookup` is its forward corollary from
actual `all = true` and actual successful lookup.

All four roots are ordinary public ASCII `theorem` commands. The module uses
no admitted proof, project axiom, unsafe/partial definition or native_decide.

## Executed commands and dependencies

Actual compiler: `/home/agent/.local/share/vesper-lean/bin/lean`, pinned commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`. The invocation used one thread,
`-j1 -M4096`, the fresh private root followed by the pinned toolchain's
`lib/lean` on LEAN_PATH, and:

```text
-o Lean4LeanLevel42/MapAll.olean Lean4LeanLevel42/MapAll.lean
```

The source/object dependencies came from immutable
`/tmp/lean4lean-normalization42-subsumption-v2`. All 36 manifest entries were
checked for exact length/SHA256 before copying; every copied source was also
byte-compared against this checkout's frozen source. They were checked again
after the owned compilation. The original shared root was never modified.

A fixed read-only Lean probe first printed the genuine `Std.DTreeMap.all`
elaboration and iteration definitions. The first owned compilation then failed
on the list-loop proof's true-head branch: simplification normalized the goal
but not the induction hypothesis. Changing that proof to normalize the actual
hypothesis and goal together closed the obligation. No incomplete source was
published. Final compilation exited zero, with no diagnostics; the tool
reported 0.776 seconds wall time for that invocation.

Source checkpoint: `9eba2e3`. Published immutable dependency root:
`/tmp/lean4lean-comparison42-all-v1`, with all 38 source/object entries and
their lengths, SHA256 and BLAKE3 values in `identities.json`. Its original
36-entry association is retained as `input-identities.json`.

| File | Bytes | SHA256 | BLAKE3 |
| --- | ---: | --- | --- |
| MapAll.lean | 1,739 | `b71484e639dac14b230f007b23b0fafcba15985ea6396f80d3bff47c93b2ff4a` | `24995a57e6723f48fc71d3847fe8a05e6c352e26ac6c30cc87686156f246bee2` |
| MapAll.olean | 102,576 | `45d7f37a7fb3168135a4bb6958cf0593978b8b0fd2147ec33453ffa0186c0d93` | `9e0feffe7889e1b8a12629d05ba81300ef13495e3440b10de50ffe595b0b28ca` |

These associate observed bytes, not machine-code derivation. The final root
was supplied to the dependent map-comparison writer without subsequent edits.
The compiler lease was released after publication; no process remains active
from this lane.
