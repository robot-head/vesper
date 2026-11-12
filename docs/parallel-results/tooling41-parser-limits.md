# Bounded lossless source lexing/parsing — issue #70

Issue: <https://github.com/robot-head/vesper/issues/70>. Worktree
`/workspace/vesper-worktrees/tooling41-parser`, branch `tooling41/parser`, base
`a1a8d938dd8103803219a05c8364cb7a198be8d9`. The coordinator checked open issue/PR
ownership, including external Warp PR #57, before assigning these paths.

Only `vesper-syntax/src/{lexer,parser}.rs`, new `vesper-syntax/tests/limits.rs`
and this report change. Core AST/checker/IR, root routing/manifests/lockfiles/CI,
other tooling crates and all Warp production paths remain unchanged. Earlier
Serde and Grammar branches are complete and were not edited for this task.

## API and limits

Existing `lexer::lex(&str) -> (Vec<Token>, Vec<Diagnostic>)` and
`parser::parse(&str) -> Parse` retain their signatures. `Parse` fields and AST/CST
schemas remain unchanged. Added explicit APIs:

```rust
lexer::lex_with_limits(source, LexLimits) -> Result<Lexed, LexLimit>
parser::parse_with_limits(source, ParseLimits) -> Result<Parse, LexLimit>
```

`LexLimits` fields are `max_source_bytes`, `max_tokens`, `max_diagnostics`;
`ParseLimits` adds `max_depth`. Defaults and hard ceilings:

| Budget | Default | Hard ceiling |
| --- | ---: | ---: |
| UTF-8 source bytes | 1 MiB | 8 MiB |
| Non-EOF tokens, including trivia/error/tail | 65,536 | 262,144 |
| Diagnostics | 128 | 1,024 |
| Guarded parser descent / expression AST height | 64 | 128 |

EOF is one extra sentinel slot. Token and diagnostic limits must be at least
one; depth at least one. Zero source-byte budget permits empty input. Values
above hard ceilings or below these minima return Configuration before lexing.
The lexer reserves capped token/diagnostic storage before iteration; pushes do
not grow those vectors beyond their configured logical budgets plus EOF.

`LexLimit { kind, limit, observed, retained_bytes }` and `LexLimitKind` report
SourceBytes/Tokens/Diagnostics/Depth/Configuration. A count observed on early
exhaustion is the first exceeding count, not an assertion that the entire
unlexed suffix was counted. All limit diagnostics use **E0003**.

## Failure and lossless recovery

Explicit source-byte/configuration preflight returns Err before lexer/CST
allocation, with zero retained bytes. Legacy lex/parse cannot return Err without
breaking consumers, so source oversize yields E0003 and a bounded UTF-8 prefix
(no codepoint split); parsing also returns an empty AST. The diagnostic reports
observed size and retained bytes. This is explicit failure, never silently
accepted truncation. A legacy oversized CST represents only that prefix.

Within the byte budget, lexical token exhaustion coalesces the last retained
token and remaining source into one opaque Error-tail span within the token cap.
Diagnostic exhaustion similarly retains an opaque tail. Every non-EOF span
still partitions the entire source with its original offsets and comment bytes;
`Lexed.limit` explicitly marks the unusable semantic stream. Parse returns an
empty AST and a lossless CST without treating that tail as accepted syntax.

Parser diagnostic/depth exhaustion records E0003 (replacing the final diagnostic
when necessary to keep the cap), marks the parser halted, and directly drains
remaining token bytes into the current green node without recursive recovery or
allocating token Strings. Active guarded calls unwind normally and close their
own nodes; subsequent recursive entries return shallow placeholders. The final
module AST is cleared, including model uses/trait bounds, so partial declarations
cannot be presented as accepted semantics. All source within the byte budget
remains in the balanced CST with byte-accurate spans.

## Recursive and iterative coverage

Guarded wrappers cover types, direct type-argument recursion, expressions, Pratt
unary/right-associative binary calls, else-if chains, blocks/primary expressions,
patterns and dotted/braced place trees. Early returns live in inner methods so
depth restoration cannot be skipped. This addresses recursion without matching
delimiters (function results, unary, assignments, else-if, dotted places).

Iterative left-associative and postfix construction also creates recursively
owned ASTs. These loops reserve depth before wrapping, and actual expression
heights are cached from direct child spans. Parentheses cannot reset a deep
subexpression's height and hide composition with another iterative chain.
Compound parents are checked before publication; at most one bounded parent
candidate is discarded during recovery. All accepted expression ASTs respect
max_depth, protecting later ordinary recursive visitors/Drop from those chains.
The helper/descent count is deliberately conservative and differs from delimiter
nesting or a language-level grammar depth. Existing grammar and ownership/type
semantics are unchanged on ordinary accepted input.

## Validation

All builds used private `/tmp/vesper-parser-target`, one job, zero dev/test debug
information and incremental off. Focused checks:

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-parser-target cargo test -p vesper-syntax --all-targets
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-parser-target cargo clippy -p vesper-syntax --all-targets -- -D warnings
cargo fmt -p vesper-syntax --check
```

Focused tests exercise 4,000-level unary, assignments, else-if, function-result
types, constructor/tuple patterns, generic types and dotted places; 8,000-operation
postfix/left-binary/try chains; grouped iterative-chain composition; diagnostic
storms; exact byte/token/depth boundaries; every hard configuration ceiling;
UTF-8/comment retention; default oversized explicit/legacy paths; maximum
supported depth with hostile real inputs; and 1,000 malformed generated cases.
Losslessness checks also assert every CST token text equals its original source
span. No delimiter-only precheck is used as the recursion defense.

Independent compatibility check: compiled the pre-change parser from base
`a1a8d93` as a temporary reference module under `/tmp/vesper-parser-reference`.
Using the same Logos token kinds, it agreed with the new default parser on
**AST, exact green CST and diagnostics** for all **19 repository .ves examples
plus 1,000 malformed generated sources**. This directly compares old/new behavior,
in addition to the committed default/explicit API and ordinary-source tests.

Final focused results: **nine integration tests pass**, strict all-target
Clippy and formatting pass. Root consumer validation also passes:

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/vesper-parser-target cargo test -p vesper --lib --test syntax_and_ir --test language --test source_traits --test source_models
```

**149 root consumer tests pass**: four library tests, 107 language tests,
24 source-trait tests, ten source-model tests and four syntax/IR tests. These
exercise actual checking/PCG, runtime/native session, model elaboration and the
existing elaborated Lean consumer path, rather than only descriptor checks.
This is the named consumer subset, not a claim that every workspace test ran.
The target was about 1 GiB after compiling root dependencies.

## Evidence boundaries

These are logical source/token/diagnostic/depth budgets, not allocator/RSS or
constant-time guarantees. Caller-owned input allocations happen before these APIs and are not retroactively
bounded. AST/CST/name/height-map allocations remain on the host
heap and are structurally bounded by retained tokens/descent. General allocation
failure is not modeled; no allocator-fault injection or hardware stack-size
portability proof is supplied. Callers may request lower limits and must honor
parse diagnostics/explicit preflight errors. A retained-prefix CST must not be
confused with the full oversized source. Parser bounds do not prove termination,
resource bounds, stack safety or memory limits for arbitrary downstream checker,
optimizer, runtime, Forge, benchmark, or JIT execution.

No root registration patch is needed; the syntax crate is already registered.
Tooling may continue using existing defaults or choose explicit bounded APIs.
