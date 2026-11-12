# Unit and tuple values in pure Expr macros

Claims #166–170 precede writers. Base is published PR164
head2e282051176f46a1612ce8b17864b65076af7c0c, including main6877cfc and the
already-landed Warp TLS/planning work. Fresh open issues and actual external PR
files, including new eBPF follow-up165, show no Forge/syntax overlap. External158
owns every workflow, Bazel/build file and scripts/test-*.sh; we do not edit them.
Claude121–136/domain/native/main hooks remain reserved. Live design metadata is
unchanged41sections/AppendixA, modified2026-11-12T09:18:30.204Z; §16.4 was reread.

## Ownership

| Claim | Branch | Exclusive writes |
| --- | --- | --- |
| #166 | tuple41/integration | this contract, integration report, interface coordination, review and protected stack integration |
| #167 | tuple41/syntax | macro_source.rs expression grammar, new source_macro_tuples tests, syntax report |
| #168 | tuple41/host | typed_quote.rs, new typed_tuple tests, host report; obsolete typed-profile tuple-negative fixtures only |
| #169 | tuple41/lower | forge_frontend/lower.rs, new forge_tuples tests, lower report |
| #170 | tuple41/acceptance | new forge_tuples_cli.rs, source_macro_tuples fixtures, check-source-macro-tuples.py, acceptance report |

No core AST/IR/native ABI, header/authority-policy, guest WIT/raw record, root CLI
registration, workflow/build or test-wrapper changes. No new language macro
category: this completes ordinary value forms inside the existing Expr profile.
Existing parameter records, alpha header checks and original semantic-provider
boundaries remain unchanged. All Lean emission uses the existing structured AST.

Separate worktrees/private existing caches, one Cargo job/debug0/incremental0.
Root target/parallel-next is coordinator-only. Initially2.3GiB free: reuse light
caches, no new Wasmtime/toolchain install. Claims remain reserved through reviewed
merge. Freeze amendments before dependent writers change these interfaces.

## Surface and typed shape

Support standalone unit `()`, grouping `(x)`, singleton tuple `(x,)`, multiple
ordered values `(x,y)` and an optional trailing comma `(x,y,)`. Nested tuples,
tuple expression arguments and quoted expression slots are supported. Commas in
strings/comments are not syntax. Wrong commas/delimiters refuse; tuple punctuation
does not relax existing macro-invocation or ordinary-call trailing-comma rules.
Nested/recursive macros and other previously unsupported expression forms remain
unsupported. Ordinary support-item parsing is unchanged.

Tuple arity is not macro parameter count. Reuse existing finite source/token/
node/output/depth/metadata/work bounds; do not impose max_parameters on tuple
elements or silently raise an existing cap. Preflight element vector storage,
growth/copy envelopes, repeated slot copies and recursive traversals before
allocation. The ordinary checker/IR can conservatively refuse larger values.

Frozen public addition in typed_quote:

`ExprShape::Tuple { open:usize, close:usize, values:Vec<ExprShape> }`.

Unit uses an empty values vector. A grouped expression retains its existing
shape; a singleton tuple always has one value. No new public constructor for
ExprInput, ExpandedExpr, ResolvedBody or other sealed artifacts is introduced.
Actual parser/source/host identities and slot ownership remain required.

The private Node parser mirrors the tuple shape. Update every recursive visitor:
shape conversion, canonical shape and template substitution, global
pre-resolution, lexical term conversion, term height, binder candidates and
resolution/resource work. A wildcard arm must not silently omit tuple children.
Use distinct bounded structural tuple tags and per-element boundaries, disjoint
from existing call/block/ref/literal tags. This keeps unit, block, grouped,
singleton and multi-element structures distinct during slot-splicing validation.

## Hygiene, authority and lexical compatibility

Public Term and ResolvedFragment wire structures remain unchanged. A tuple may
use the existing Term::Block only as ordered lexical traversal of all its values;
the actual semantic tuple remains in sealed ExprShape and expected core AST.
Empty unit has no lexical operations, but retains actual token provenance and
expansion records. Keep legacy flat lexical parsing unchanged; typed tuple support
does not claim a new flat fragment version or source Tier2 integration.

Every tuple element is visited in order with the correct enclosing context.
Block-local bindings in one element must not leak into another. Current source
parameter scopes, caller/template hygiene and initializer rules remain intact.
Template-local free globals retain actual definition-site claims. Tuple syntax
cannot grant a foreign host/source/slot/function identity or cached approval.

Generated calls inside any tuple element must still be recorded with their true
origin and gated against actual compiler-derived authority. A generated helper
whose unchanged signature hides a newly effectful body must fail empty grants.
Caller-origin helper expressions supplied in slots keep their source origin;
they must not be relabeled as generated or grant the macro extra capabilities.
Tuple/block traversal and call aliases cannot hide operations from those checks.

## Lowering and provenance

Lower ExprShape::Tuple to the existing core ExprKind::Tuple, preserving every
value and order. Emit `()` for zero values and the mandatory trailing comma for
one value; never turn a singleton into grouping. Multiple values may use canonical
comma/space formatting. Existing parenthesized-expression normalization remains.

Validate actual delimiter/token indices and retained provenance through the
sealed host data. Preserve source/generated byte anchors for tuple structure and
all child expressions under the existing mapping schema. Structural punctuation
uses the node's actual delimiter provenance/anchor policy; maps do not assert
lexeme equality. Repeated argument occurrences retain distinct expanded ranges.
No fabricated original spans, source placeholder, deep World clone or constructor
for a checked artifact is accepted.

Preflight tuple AST/value vectors, recursive node/depth costs, render output and
map storage/copies/work before allocation. Traverse tuple children for all
reference/binder inventories. Preserve active parameter IDs and exact expected
header association from params41. The complete candidate must pass the actual
ordinary checker, body AST comparison, current authority gate and IR validation.
Failures retain the existing no-accepted-artifact/maps/audits behavior.

Do not alter integer inference, contextual tuple field typing, pass semantics,
product-mode joining or linear exhaustion. The real checker must reject repeated
or leaked linear tuple components. Unit/group/singleton shape differences must
also survive the body AST comparison, even if a mismatched candidate could type
check under another context.

## Actual acceptance and integration

Use actual source check/expand and separate normal expanded-program runs to42
for unit, grouped, singleton, multiple and nested tuples, including tuple-valued
input slots and current plain function parameters. Ordinary support functions may
destructure values; this batch does not add pattern grammar to macro bodies.

Cover both hygiene directions, parameter/global collisions, initializer scopes,
element block isolation, order, UTF8/comment/string punctuation, exact anchors and
repeated spans. Real linear tuple construction/consumption, duplication/leaks and
field-type mismatches must reach the actual checker. Include generated-helper
effect drift inside nested tuple elements and a distinct caller-origin effect
case, without inventing runtime grants. All malformed/depth/output/metadata/work
refusals fail without partial acceptance.

Explicit unsaved editor previews use current bytes/version/hash; original hover
and diagnostics remain unexpanded/unavailable. Actual formatting conserves all
ordered nontrivia/comment tokens, reparses and is idempotent. No preview/compiler
component, Lean or generated-program execution occurs; runs above are separate
explicit normal CLI invocations. No fake executables or performance claims.

Root retains old macro/parameter/query/provider/formatter/editor guards and strict
workspace checks. #158 owns native CI migration; supply exact new Rust targets
and `check-source-macro-tuples.py --binary REALPATH` for Linux/macOS/Windows.
New Rust tests are discoverable by ordinary workspace metadata. Pending native
jobs remain pending evidence; preserve merge ancestry while the own stack lands.
The full design goal remains active beyond this Expr slice.
