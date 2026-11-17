# Stage 1 perf lane 7: near-linear `vesper fmt`, file cap lifted

Claim: #937. Baseline: `943d901e74430d8b4da759f62dec66a458fd1a20` (origin/main).
Scope: `crates/vesper-fmt/**` and this report only.

## Where the cap and the quadratic work came from

Measured on the baseline, not guessed:

1. **The 8,192-token cap** was a hard clamp in `guarded_lex`
   (`limits.max_tokens.min(8192)`), so larger requested limits were ignored.
   The ordinary parser (`parser::parse`) has its own 65,536-token default on
   top of that.
2. **The 4·N² charge** was a bookkeeping charge, not real work: every
   `guarded_lex` call charged `tokens²` work units "for existing parser
   calls", and one format runs it four times (first pass, both conservation
   lexes, idempotence pass). With the 128 Mi budget that refused files above
   about 5,800 tokens, below the nominal cap. The parser itself is linear.
3. **The real quadratic algorithm** was render lookahead.
   `Document::following_width` copied the whole pending render stack for
   every group it measured, and a block or module with K items keeps about K
   entries on that stack. Rendering was O(groups × items in the enclosing
   block): visible on files with many items in one block or one long list.
4. **The "nesting" failure the warp-wheel report hit** was the
   128-component expression-chain preflight, not delimiter depth. Its counter
   at one level was reset only by `,` and `;`, so the header tokens of each
   method in an `impl` block added up as one "chain" and about 18 methods
   (40 short ones) returned `DepthLimit`.

## What changed

- **Linear work charge.** The parser preflight charges 64 units per token
  instead of N². Lexing uses `lex_with_limits` at the lexer's hard ceilings,
  and the ordinary parse uses `parse_with_limits` with the same ceilings, so
  the formatter's own `max_tokens` is the only token limit that applies.
- **Lookahead without copying.** `following_width` walks the pending stack
  from its top in place and expands each `Concat` one child at a time. It
  visits only the nodes it needs before the 100-column ribbon overflows or a
  line breaks. The visit order and every decision are unchanged.
- **Chain guard fixed.** A closing `}` followed by a token that cannot
  continue the expression (anything but a binary or assignment operator,
  `.`, `?`, `else`, `with`, `(`, `[`, `..`, `..=`, `->`, `=>`, `:`, `::` or
  `as`) starts a new item or statement and resets that level's counter.
  `{0} + {0} + ...` and `if .. {} else {} + ...` chains still count, so the
  guard still refuses what the parser's height limit (128) would refuse.
  The delimiter-depth limit (24) is unchanged: it was not the problem.
- **Parser scope copies charged.** The ordinary parser in `vesper-syntax`
  clones its set of bound names at every function, block, `for`, match arm,
  closure and handler clause, so one function with thousands of distinct
  block-initialized locals is quadratic there (8,000 `let xN = { 0 };` took
  2.4 s). That parser is outside this lane, so the preflight charges an upper
  bound: copy sites times identifier tokens, per function (a top-level
  function or a method of a top-level `impl`/`trait`/`model`). Real files stay
  near-linear; that input shape is now `WorkLimit`. Making the parser's scope
  sets persistent would remove the charge and is left for a `vesper-syntax`
  lane.
- **Every lookahead step is charged**, including leaving an empty `Concat`,
  so shared empty nodes built through the public `Document` API cannot scan
  for free.
- **New defaults.** 1 MiB input, 4 MiB output, 131,072 tokens (twice a
  64k-token file, trivia included), 1,000,000 Doc nodes, 512 Mi work units.
  Requested limits above the lexer's hard ceilings (8 MiB, 262,144 tokens)
  are clamped, so `usize::MAX` limits are still typed errors, never panics.
- **Kept at 8,192 tokens:** files that use source macros (the macro-profile
  parser in `vesper-syntax` resolves names in quadratic time and has its own
  budget), `--recovery` formatting and the Lean proof profile. Those parsers
  live outside this lane's paths. A macro file over the cap now returns
  `TokenLimit` up front instead of a budget error.

## Output identity

Every tracked `.ves` file (608 at the baseline) was formatted with the
baseline crate and with this branch through the library API, hashing each
output or recording the error. The two lists are identical: 603 identical
outputs and the same 5 errors (3 `OpaqueMacro`, 2 `Syntax`; all 5 are in
`scripts/fmt-exclude.txt` or are fixtures that are invalid on purpose).
`scripts/test-fmt.sh` (`vesper fmt --check` over the repo) runs in CI on
BuildBuddy.

## Formatting time against file size

Release build, one cloud core, one `format_source` call (which formats twice
and checks idempotence). "Before" is the baseline algorithm with its token
clamp and N² charge removed so that it can run at all past 8k tokens; the
unmodified baseline refuses every row above 8,192 tokens with `TokenLimit`.

| Input | Tokens | Before (ms) | After (ms) |
|---|---:|---:|---:|
| `check_expr.ves` ×1 | 5,761 | 7.6 | 6.0 |
| `check_expr.ves` ×4 | 23,041 | 31.9 | 22.3 |
| `check_expr.ves` ×11 | 63,361 | 97.7 | 69.7 |
| `check_expr.ves` ×22 | 126,721 | n/a | 171.5 |
| `warp_ack_full.ves` ×11 | 55,628 | 72.1 | 47.1 |
| 1,000 small functions | 16,001 | 26.5 | 14.9 |
| 2,000 small functions | 32,001 | 74.7 | 19.9 |
| 4,000 small functions | 64,001 | 222.2 | 38.9 |
| 8,000 small functions | 128,001 | 1,319.8 | 87.3 |
| one block of 6,000 `let` statements | 84,011 | 303.6 | 95.6 |
| one 20,000-element tuple | 60,019 | 1,129.8 | 67.4 |
| all of `packages/vesper-compiler/src` in one file | 75,504 | `Syntax` (parser cap) | 108.1 |
| the warp-bbr port's 8 parts in one file | 15,692 | `TokenLimit` | 24.6 |

After the change, time and work are linear: doubling the file doubles the
time, and `formatting_work_grows_linearly_with_the_file` checks that the work
budget that admits a file admits half the file with half the budget.

## Tests

New in `crates/vesper-fmt/tests/format.rs`:
- `a_64k_token_file_formats_inside_the_default_budget` (66k+ tokens, canonical
  output, a scrambled copy reaches the same text).
- `formatting_work_grows_linearly_with_the_file`.
- `many_methods_in_one_block_are_not_a_depth_error` (64 methods; the baseline
  fails at about 18).
- `many_items_in_one_block_render_in_linear_time` (120k+ tokens in one block).
- `adversarial_inputs_stay_typed_errors`: 5,000 nested parentheses and braces
  are `DepthLimit`, a 200-term chain of `if` blocks is `DepthLimit`, an
  over-cap file is `TokenLimit`, an over-size file is `InputLimit`, and
  `usize::MAX` limits are clamped to typed errors.

The existing tests, including the 200-term `f(1,2)+...` and `{0}+...` chain
tests and the macro allowance tests, pass unchanged. `cargo +1.100.0 clippy
-p vesper-fmt --all-targets -D warnings` is clean.

## Stage 1 guard

This lane edits nothing under `bootstrap/**`, `src/verified_codegen/**`,
`src/ir.rs`, `lean/**`, `packages/vesper-compiler/**` or
`crates/vesper-codegen-verified/**`. `vesper-fmt` is not a dependency of
`vesper-codegen-verified` or the `vesper-bootstrap` binary, so the Stage 1
artifacts cannot change. The `bootstrap-stage1` workflow was dispatched on this
branch; its result is linked from the PR.

## Port ratios

Unchanged by construction. The formatter is not on any port's build or run
path, so the Vesper/Rust ratios in the P1 reports (warp-wheel ~8x, BBR
11-16x, Prism 4.5x, warp-tcp 10-14x, Quarry 4.3-4.9x) are the same before and
after. What this lane changes for ports is the source layout: a port no
longer has to be split into 8,192-token parts to pass `vesper fmt --check`.
Merging the parts back (warp-bbr `vesper/src/*.ves`, the
`packages/vesper-compiler` concatenation) is left to the port owners, since
the owner rule keeps this lane off port branches.

## What is left

- The `vesper-syntax` parser's scope-set copies are quadratic in the number of
  locals of one function; the formatter refuses that shape with `WorkLimit`.

- The macro-profile, recovery and proof-profile token caps (8,192) need
  linear parsers in `vesper-syntax` first.
- The parser's 65,536-token default in `parser::parse` still applies to other
  callers (LSP, `vesper build`); `vesper build` on a file over that is a
  separate question for the compiler driver.
