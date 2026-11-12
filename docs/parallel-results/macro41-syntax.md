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
