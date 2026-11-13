# Syntax-only complete-declaration formatting — issue #112

`vesper_fmt::recovery::format_recovery_source` now formats independently complete
functions inside actual original-source recovery snapshots. Result/edit fields
are private; getters expose the original hash, disjoint original-byte ranges,
replacement bytes, output/changed state, and count of eligible declarations.
Neither caller spans nor copied descriptors can construct a proposal.

The implementation lexes the entire real buffer with finite bounds and refuses
global lexical uncertainty. It uses the sealed `parse_recovery` inventory, then
reparses all selected complete functions together with `parse_complete_items`.
Selection also requires actual lexical delimiter depth zero at the declaration
start. Recovery can resynchronize a Complete-looking `fn` inside a damaged body;
that status alone never authorizes formatting. Nested apparent functions stay
untouched, and a missing outer delimiter conservatively makes following apparent
siblings ineligible until a true balanced top-level boundary. Mismatched closing
delimiters refuse the entire proposal. Comments/string token payloads do not
contribute delimiters.
Each complete function uses the same production formatter and shared Work.
Unsupported contexts remain untouched. No recovered function body, opaque macro,
comment outside a declaration, or damaged gap is formatted. Removing only the
formatter-added terminal LF from each replacement preserves original outside
trivia exactly and prevents newline growth across repeated formatting.

The assembled candidate must preserve the entire ordered non-whitespace token
stream, including exact comment/string bytes. Its actual recovery declaration
kind/name/status/body-presence inventory and recovery-site kind/owner inventory
must also match. The whole recovery pass is repeated with the same budget; a
successful candidate is byte-idempotent and yields no edits on that second pass.
Ordinary syntax/semantic errors are never repaired or suppressed, and no checked
source, World, semantic diagnostic, macro/native execution or proof is created.

All calls share one cumulative parent Work. Real lexer token staging, selected
index/edit storage, byte comparisons, output storage and copies are charged before
allocation/traversal. Recovery/complete-item parser calls each reserve eight Mi
work units before entering their own bounded parser, up to four calls across the
two passes. Parser source is capped at one MiB, tokens at8192, declarations256,
sites256, metadataoneMiB, identifiers256bytes and depth24, further tightened by
caller formatter limits. Maxima need not fit together. These conservative logical
budgets make no RSS, performance or termination claim.

Actual combined leaf verification used macro checkpoint9121999 after the shared
helper2d46541, with temporary module registration only (not committed). Private
cache `/tmp/stylus41-recovery-target`, one Cargo job/debug0/incremental0. The full
formatter suite passes: 11 unchanged ordinary tests, seven actual macro tests,
12 new recovery tests and one private-construction compile-fail doctest, zero
ignores. Strict all-target formatter Clippy, rustfmt and diff checks pass.

New tests exercise damaged functions with complete siblings; CRLF/Unicode and
outside gaps; opaque macro fake declarations; comments/string payloads; current
original-byte edits/hash; canonical eligible counts versus zero eligibility;
lexical uncertainty; input/output/token/node/depth/work rejection; aggregate
second-pass exhaustion over20functions with64Mi LSP success; preserved linear
binding tokens; and every UTF8 prefix of an actual damaged/macro-bearing buffer. Further actual
regressions cover recovery-resynchronized fake functions inside damaged bodies,
missing-outer-brace ambiguity and mismatched-delimiter refusal.
Root acceptance separately checks the normal compiler still rejects leaks and
that actual stdio proposals remain current-version UTF16 edits.
