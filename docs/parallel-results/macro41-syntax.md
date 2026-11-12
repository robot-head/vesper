# #103 actual source macro syntax

First compiled checkpoint on fresh `macro41/syntax` from `fd09dd7`.
Read issue #103, macro41 contracts, specification §16.4 and existing parser,
recovery, lexer and Forge category APIs. APIs were coordinated with typed host,
source binding, lowering and root owners before implementation.

The private MacroSource implementation retains actual source/hash/lexer tokens,
source-bound private IDs and immutable definition/function/header/body/invocation/
argument inventories. FunctionHeader is independently parsed, with actual types,
effects and original spans; no Function with a dummy body exists. Ordinary
functions also have actual complete original ASTs in support_items(), along with
supported ordinary ADT/effect items. Macro-bearing functions have no support AST.
Quote spans exclude wrapper braces and contain exactly one Expr; actual inner
blocks carry lets/trailing semicolons, never an invented wrapper block.

Dollar is appended AFTER the existing Eof token ordinal. Existing batch syntax
still rejects macros; no implicit execution is enabled. The shared header parser
retains old function context save/restore and editor HeaderNote behavior. Root
owns `pub mod macro_source;` registration; temporary registration and standalone
manifest wiring used for private validation were restored before this checkpoint.
No manifests, root lock, compiler host or core AST schemas were changed.

Frozen limits are flat usize fields max_source_bytes/max_tokens/max_macros/
max_functions/max_invocations/max_parameters/max_depth/max_metadata_bytes/max_work;
defaults128KiB/16384/32/128/256/8/48/1MiB/16MiB and HARD
1MiB/65536/128/1024/4096/32/128/16MiB/128MiB. validate()/preflight() are public.
Error has public kind:ErrorKind and span:Option<Span>, bounded static Display.
The profile supports names/literals/calls/let/blocks/grouping/qualified paths,
fixed Expr parameters, one Expr quotation per definition, independent invocation
sites, and no nested/recursive invocations. Unknown macros and arity mismatch fail.

First checkpoint validation: actual `cargo check --offline` PASS in reused
`/tmp/vesper-target-editor41-span`, one job/debug0/incremental0. This checkpoint
claims no test/Clippy completion yet; meaningful syntax/compatibility/budget tests
and final validation follow before delivery is complete.

## Final delivered syntax profile and validation

Final implementation adds strict bounded templates/caller-body/argument category
validation and real ordinary item/header parsing over exact full-source token
ranges. MacroId/FunctionId/InvocationId contain private source hash/index fields,
Copy/Eq/Hash with index()/source_hash(); snapshot definition()/function()/
invocation() lookups reject IDs from different actual source bytes. Identical
source bytes intentionally share value identities, not a host session or approval.
The typed host separately owns session/slot/expansion identities.

Definitions contain id/name_span/parameters/quote_span/fullspan. Parameters retain
actual `$name:expr` and name ranges, with Category::Expr. Function inventory
includes all supported top-level functions; SourceFunction has id/actual header/
body_span/fullspan/invocation IDs. Macro-bearing functions have at least one
invocation, no parameters/generics, and no core Function in support_items().
Ordinary functions appear both as a real header inventory and their full actual
support AST; dependent registry code deduplicates the same actual item span.
Invocations retain definition/function IDs, actual name/full range and ordered
actual expression argument ranges. Forward definitions resolve after full parse.
Unknown macros, duplicate macro/parameter names and arity mismatches fail; public
record clones cannot replace sealed snapshot records.

Initial templates and macro-bearing bodies accept integer/string/bool literals,
ordinary/qualified names, parentheses, calls and actual brace blocks with ordinary
`let name=expr;` bindings, discarded expressions and real tails. Empty actual
blocks are allowed; empty quote contents are not an Expr. Antiquotes occur only
as Expr atoms in templates, including callee positions; the separate typed host
must group/structurally compare substituted syntax, not trust this parser as a
proof of expansion shape. Let binders cannot be antiquotes. Nested/recursive
macro invocations, other macro categories, handlers/closures/operators/field/
index/type-argument syntax in macro-bearing category fragments, destructuring/
typed lets, and unknown top-level contexts explicitly reject. Qualified global
names have syntax identity only; no DefId resolution/authority is minted here.
Ordinary complete support function bodies remain the real existing parser's
language (including parameterized functions); only top-level fn plus ADT/effect
support forms are admitted by this source-macro entry point. Mode-prefixed
functions are explicitly unsupported here, rather than bypassing the function
inventory. Existing batch parsing of those forms is unchanged.

The shared parser FunctionHeader production parses actual name/generics/params/
result/effects/where syntax before body parsing. Normal function() preserves its
actual row_names/bound_values save/restore and editor HeaderNote logic; the header
span ends at the actual opening brace. No prefix padding, wrapper source, dummy
Function or guessed body is used. Header-only macro parsing rejects trait-bound
contexts unsupported by this profile. Body-span ranges include actual braces;
quote-span ranges exclude quote wrapper braces and include original inner trivia.
Root can remove exact macro definition spans and replace only actual macro-bearing
body spans while copying original headers/support/trivia unchanged.

Resource accounting is logical, not RSS/allocator-capacity/OOM/latency evidence.
Source/config checks precede lexing or source copies; source bytes plus potential
bounded token/index records are preflighted before lexer staging. Macro/parameter/
function/invocation/vector/string staging checks occur before growth. Actual
ordinary parser stages conservatively reserve `64*depth*token_count +
2*depth*source_range_bytes` metadata units and `64*depth*token_count +
8*source_range_bytes` work before token/header/AST clones, allowing for guarded
parser context/AST staging. These reservations accumulate across stages; no
successful/failed stage resets the allowance. Individual maxima are not jointly
guaranteed to fit. Source/name lookup byte comparisons, parser/cursor operations,
original hash/source copy allowance, delimiter stacks and inventory records are
charged. Existing parser/standard-library allocation machinery is not claimed to
be fallible or an allocator/RSS limit; ErrorKind::Allocation covers explicitly
fallible adapter reservations. Hard source/token/depth/count ceilings additionally
bound implementation/container overhead. Recursive/category depth and delimiter
nesting are bounded; later host/AST phases enforce their own expression-shape caps.

Actual final validation in reused private `/tmp/vesper-target-editor41-span`,
one job/dev+test debug0/incremental0:

- **12 new source_macro_syntax integration tests PASS**: actual source/hash/tokens,
  exact header/body/template/argument ranges, true original support AST spans,
  explicit block/semicolon preservation, real slot callee syntax, Unicode/CRLF/
  strings/comments, unknown/arity/recursive/nested refusal, malformed/incomplete
  source/contexts, duplicate names, foreign IDs, every direct resource cap and
  every actual UTF-8 prefix of a contextual macro corpus.
- **49 existing tests PASS**: 16 parser/model units, 9 actual incremental tests,
  10 guarded parser limits and 14 recovery parser cases. Ordinary generic modal
  and row/value shadow header cases also match existing complete support ASTs in
  the new tests. Legacy batch still rejects macro source; Dollar ordinal is
  exactly old Eof+1, preserving every prior token discriminant.
- **2 compile-fail doctests PASS**, including external MacroSource construction.
- **Strict syntax all-target Clippy PASS** (`-D warnings`).
- Owned Rust rustfmt check and git diff check PASS.

Temporary syntax lib registration and standalone manifest were restored again;
no leaf lock, manifests, root registration/lock/cache or sibling production files
are committed. Deliveries are the first compiled checkpoint plus final tests/
resource-lookup refinement commit. Root owns full expanded-source checking,
expected-shape validation, compiler authority and runtime acceptance; none of
those positive authority or execution claims follows from this syntax snapshot.
