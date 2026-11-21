# 18 Lens and the editor

Type half a method call and the editor already has an incomplete program to check. Lens aims to report the same ownership errors as a batch check while keeping that unfinished call from producing misleading errors elsewhere.

## 18.1 Reuse the checker during editing

The language server maps edits to compiler queries. Completion can use the type and outstanding obligations at the cursor. For a live transaction, a consuming method is a more relevant suggestion than another inspection. Opening a workspace also needs a policy for macros and build-time code.

## 18.2 Recover at a known syntax boundary

When you type `tx.` the member name is still missing. Recovery keeps an incomplete node and resumes parsing at a balanced delimiter or another stable boundary. The checker can suppress ownership conclusions that depend on the missing member while continuing to check neighboring functions.

## 18.3 Stop recomputation at unchanged results

Incremental queries track their inputs. Changing a private body may leave the file’s public signature fingerprint unchanged, so callers can reuse earlier results. Macro outputs and checked interfaces form further boundaries. Slower proof queries can run after editing settles without blocking syntax feedback.

## 18.4 Connect a diagnostic to the edits it needs

A move error involves the location where ownership transferred and the later use of the old owner. A multi-span diagnostic shows both. A quick fix edits the concrete syntax tree (CST), which retains comments and spacing. Check the edited function again because changing a permission can affect callers.

## 18.5 Translate source spans into editor locations

The span layer connects compiler byte ranges to the editor’s document positions and revisions. A fix computed for an older file version may select the wrong text after an edit. The language server needs to associate diagnostics and edits with the snapshot that produced them.

# 19 Stylus formatting and ownership-aware linting

Stylus formats code while preserving its tokens and comments. A lint can propose a change to the program itself, such as borrowing just one field of a record. Applying that change calls for a fresh type check.

## 19.1 Preserve tokens and inspect semantics

A formatter works on a lossless syntax tree so a comment stays with the code it explains. A lint can use type information to notice that a helper requests exclusive access to an entire record but only touches its index. If an autofix narrows that parameter, recheck the function and its callers.

## 19.2 Set layout and lint policy

Canonical formatting selects line breaks, indentation, and grouping consistently. A package lint policy can assign severities and enable domain-specific rules. For example, a project may treat a public capability increase more seriously than a naming suggestion. External lint packs need an explicit execution boundary.

## 19.3 Narrow a projection or shorten an allocation lifetime

If a helper only writes `broker.index`, a smaller projection can let the caller lend another field. If temporary data never leaves one request, an arena may suit its lifetime. Before applying either change, check aliasing, escape paths, allocation behavior, and the shape of the public signature.

## 19.4 State what an autofix preserves

A formatter property can compare the ordered comment tokens before and after formatting. For a proposed semantic fix, check that the result type-checks and preserves the behavior or compatibility promised by that rule. For an allocation rewrite, one such obligation is that every value remains live until its last use.

## 19.5 Select line breaks and recheck edits

A document formatter represents alternatives such as a flat group and an indented group with breaks. It chooses a layout that fits the line width. A lint engine instead proposes a source edit at a span. If the diff changes a parameter’s access mode, inspect the lint edit. Line breaking alone should preserve that mode.

# 20 Codex documentation and doctests

Codex proposes using the compiler’s ownership, layout, and effect information to document APIs. A caller can look up which fields a function changes or search for a method that consumes a transaction.

## 20.1 Search for the operation you can call

A signature search can consider input and output types, parameter modes, and effects. If you own a transaction and need to finish it, a method accepting `sink self` is relevant. Looking only for the word “transaction” would also return methods that merely inspect it.

## 20.2 Exercise a documentation example

A doctest turns a documented code block into a check or execution in a controlled environment. A simulated clock can give a stable result to a timing example. The example needs an expected result, a defined set of capabilities, and a source location for reporting a failure.

## 20.3 Read a layout or obligation diagram

A wire map gives each field a byte range. A typestate graph shows which operation changes a value’s state or consumes it. A mutation footprint shows which fields an exclusive call may touch. Choose the view that answers the caller’s question, such as “May I reuse this buffer after the call?”

## 20.4 Define a correct search result

A search result must satisfy the signature constraints the query requested. A complete wire map must account for every byte of the modeled record. These are checkable properties of a representation. To trust the generated diagram, also check that the compiler supplied the field offsets for this version of the record.

## 20.5 Generate documentation from a checked interface

The proposed pipeline extracts states and transitions, indexes signatures, and plans a shared compilation for runnable examples. A cached page needs a key that includes the interface and relevant examples. A changed method signature should invalidate both its visual contract and any doctest using it.

# 21 Chroma highlighting and accessible code

Source coloring can show nesting and keywords even in a fragment. With compiler metadata it can also distinguish a consuming argument, a mutable place, and an effect capability.

## 21.1 Choose syntax or semantic tokens

Syntax highlighting can operate on partial text. Semantic highlighting needs resolved information about a binding or call. A static code card should keep the source readable when color is unavailable. Names, labels, and ordinary code syntax must still convey the important distinction.

## 21.2 Attach meaning to a source range

A semantic token carries a range and a classification. A website example can associate the source with an expected result or a diagnostic. Keep the underlying text intact so readers can copy a compiling example and assistive tools can read it in order.

## 21.3 Render a static example or an editable snippet

A published page can render token spans once during the build. An interactive playground must recompute highlighting when the reader edits the source. Theme variables map the same semantic role to suitable colors on light and dark surfaces. Static examples can remain readable without scripts.

## 21.4 Check both source coverage and contrast

Concatenating token ranges in order should reproduce the original text exactly, including whitespace. Contrast checks compare the actual foreground and background pairing. A bright color that works on one theme can disappear against a different surface.

## 21.5 Generate a reusable code card

The renderer lays out tokens and annotations into a scene for an SVG or raster image. OKLCH separates lightness, chroma, and hue for palette design. Check the exported size, chosen font, and displayed background so the image preserves the distinctions visible in source.

# 22 Crucible testing and migration

Crucible records the inputs and scheduling choices that lead to a failure. The test runner can replay that sequence, then shorten it until the remaining choices explain the bug.

## 22.1 Explore program behavior with properties

A property states what should hold across inputs, such as decoding an encoded record giving the original value. Coverage guides exploration toward unseen branches. Concolic testing pairs a concrete run with symbolic branch conditions to look for another input. One counterexample can refute a universal claim.

## 22.2 Shrink a failing choice tape

Record generated values and simulated scheduling choices. After a failure, try shorter tapes or smaller choices and retain a candidate only if it reproduces the failure with valid resource lifetimes. A two-event trace can explain a scheduling bug more clearly than the original thousand events.

## 22.3 Compare implementations during a migration

When replacing a C or Rust function, run both versions against equivalent inputs and compare their observable results. Account for aliasing, state updates, and external effects. Comparing a returned integer alone could miss a changed buffer. A proposed lifted signature also needs review against the foreign caller’s actual contract.

## 22.4 Explain why reduction stops

Ordering tapes by length and then lexicographically gives a shrinking algorithm a measure of progress. A successful reduction must move downward. Agreement of two implementations on a test corpus gives evidence for those cases. A general equivalence theorem also needs a model of all relevant observations.

## 22.5 Preserve the smallest useful replay

The shrinker proposes edits to a recorded choice sequence. A projection lifter infers candidate access paths from source operations. A test author needs a reproducible failure, while a migration author needs to know which reads and writes the new signature permits.

# 23 Chrono recording and reverse debugging

When a service stops with a missing ticket, its final memory snapshot shows which resources remain. Chrono’s transfer history can show where the missing ticket last had an owner.

## 23.1 Record what makes a run nondeterministic

An effect-boundary recorder captures observable inputs such as clock readings, incoming completions, and scheduling outcomes. Checkpoints give replay a starting state. Hardware branch traces can help explain execution between events. Sensitive inputs need a redaction policy before recording.

## 23.2 Decode a flight-log entry

A compact wire record identifies an event and the data required to replay it. Preserve event order and identities when stitching records together. A linear resource’s lineage connects creation, transfers, and final consumption across functions or fibers.

## 23.3 Work backward from a bad outcome

Find the failure event, restore a preceding checkpoint, and replay the recorded decisions. A reverse watchpoint seeks the previous write to a particular location. A lineage query asks who owned a particular resource at the time. These questions can produce different answers if a slot was reused.

## 23.4 Identify the replay assumptions

Deterministic replay requires an initial state and every nondeterministic observation used by the model. A lineage completeness property requires every modeled transfer to emit an event. Unrecorded external input or an incomplete log limits what a replay can establish.

## 23.5 Index a trace for repeated questions

An index from a location to ordered writes speeds up reverse watchpoints. An index from resource identity to transfers supports ownership queries. Keep the allocation generation with the identity so two resources that occupied the same address can have separate histories.

Consider a trace in which two transactions use the same storage slot.

    Event 10   mint T1 in slot 4
    Event 12   transfer T1 to the log writer
    Event 15   consume T1 at commit
    Event 21   mint T2 in slot 4

A watchpoint on slot 4 can return events from both transactions. A query for T1 ends at event 15. Following the slot into event 21 would incorrectly attribute a second lifetime to T1.

# 24 Metronome performance measurements

To compare two versions of a hot loop, decide what work you want to measure, which hardware events represent it, and how much the measurement itself can vary.

## 24.1 Pick counters that answer the question

A performance monitoring unit (PMU) counts events such as cycles, instructions, or cache misses. Compare equivalent workloads on a stated machine profile. A lower cycle count on one machine is evidence about that measurement setup. It is a different claim from an allocation-free source contract.

## 24.2 Measure a batch that consumes its inputs

A benchmark context can separate preparing a batch of linear tickets from measuring the operation that consumes them. Recreate equivalent inputs for the next batch. Snapshot the counters before and after measured work. The counter width matters when interpreting their difference.

## 24.3 Express a gate for a real workload

For a SIMD kernel, measure a range of lengths including a tail. For log transactions, decide whether preparation and persistence belong in the measured interval. A benchmark annotation can give the harness a budget to compare with those observations. Keep compiler optimizations from deleting unused results.

## 24.4 Account for wraparound and shared drift

A hardware counter can wrap between samples. Modular subtraction recovers the delta only under a bound on the elapsed count. Interleaved A/B pairs can cancel some common drift. If the environment changes differently for the two cases, that assumption needs another check.

Take a toy eight-bit counter. A start of 250 and an end of 4 represent ten events if fewer than 256 events occurred. Another complete trip around the counter gives the same two readings. Choose a sampling interval short enough to rule out that extra wrap.

## 24.5 Compare paired measurements

Pair baseline and candidate runs, vary layout where appropriate, and inspect the distribution of differences. A gate should report the affected measure and uncertainty. Compare the generated assembly or use causal profiling to investigate which change slowed the measured work.
