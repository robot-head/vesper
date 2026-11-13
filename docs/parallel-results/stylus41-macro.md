# Supported source macro formatting — issue #111

`format_source` now formats the fixed Expr macro source profile introduced by
macro41. It first enforces the existing lexer/delimiter/chain guards, then requires
the actual sealed `vesper-syntax::macro_source` parser to accept the complete
module. A module must contain an actual macro definition; a bare `quote`-spelled
ordinary identifier does not opt source into this profile. Unknown macros,
unsupported categories, repetition, nested invocations, bad arity and malformed
templates remain errors. No quotation expansion, type checking or guest/native
execution occurs during formatting.

For a validated macro module, the actual sealed recovery CST supplies lossless
token locations. Each CST range, kind and text is checked against the original
lexer/source evidence before lowering to the existing bounded Doc renderer.
Recovery ERROR nodes do not establish syntax or semantic acceptance: the separate
actual-source macro parser is the syntax gate. Original and rewritten ordered
non-whitespace tokens, including every comment/string byte, must agree. The
rewritten module is reparsed and formatted again for exact byte idempotence.
Dollar-slot and invocation punctuation remain adjacent in the canonical output.

Both macro parser and recovery-CST work envelopes are prepaid from the shared
formatter Work before their bounded source/token/tree allocations. Their finite
limits can conservatively reject large valid inputs. Existing source/output,
token, delimiter, path-chain and Doc guards remain in force. This is logical
budget accounting, without a measured latency, RSS or conservation theorem claim.

The first checkpoint, 2d46541, exposes the agreed crate-private
`format_with_work(source, limits, &mut Work)` so recovery formatting can share a
cumulative operation budget across complete declarations. Parent Work's existing
fields and methods are unchanged; the public API still creates one Work and calls
that helper. The recovery owner and root received this checkpoint before their
dependent writes. No recovery registration is committed by this lane.

Actual leaf validation: all 11 existing formatter tests and seven new macro tests
pass, including simple/parameterized/block quotations, helpers, multiple
invocations, nested comments, Unicode, CRLF, line-comment boundaries, string
payloads, malformed/unknown/unsupported macros, finite limits and every UTF8
prefix of a sample program. Successful prefix cases conserve exact token
evidence and are idempotent. Strict all-target leaf Clippy and rustfmt/diff checks
pass. Private light target `/tmp/stylus41-macro-target` was used with one job,
debug0 and incremental0; the coordinator target was not written.

Actual CLI expansion/check/run42 and stdio acceptance belong to #113/root after
integration. No source compiler, core AST/IR, checker or native ABI was modified.
