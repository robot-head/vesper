# Checked local intra-doc link executable acceptance

Claim [#180](https://github.com/robot-head/vesper/issues/180) owns only the new
executable Rust tests, fixtures, Python harness and this report. The published
contract is `docs/rfcs/links41-contracts.md`; it preceded these writes. External
#158/#171 retains every workflow, build/Bazel file and `scripts/test-*.sh`.

Five independent Rust tests and `scripts/check-doc-links.py` exercise the actual
`vesper doc` executable. The Rust harness accepts `VESPER_DOC_LINKS_BINARY`, with
the Cargo-produced executable as its fallback; Python takes `--binary`. Neither
harness substitutes a parser, checker, checked artifact or proof report.

The corpus contains actual local function, linear type, inherent method, effect,
effect-operation and trait-name targets. References precede declarations, repeat
and retain exact original exclusive-end UTF8 byte spans after a multibyte prefix
and after conversion to CRLF. The accepted inventory must exclude builtin-only
targets and include the expected declaration kinds. Every rendered fragment href
must have exactly one actual HTML target; repeated links must preserve the same
target identity. Both JSON and HTML must be byte deterministic.

Lexical cases cover ordinary comment/string lookalikes, `////`, escaped brackets,
matching inline-code delimiter runs, backtick and tilde fences, images, explicit
Markdown URLs, reference-style links/definitions and bracketed non-path prose.
A fence or inline-code matching search cannot suppress a reference after a blank
line, ordinary comment or declaration breaks its doc-comment group. Unmatched
backticks remain literal rather than granting an unclosed code span. Script-like
text remains escaped, with no script/iframe tags or external hrefs.

Unknown references, a genuine type/effect namespace ambiguity, explicit Self,
package paths and generic paths must fail with their actual diagnostic code and
original byte span. Renaming a real declaration invalidates the old reference;
repairing it restores successful documentation. Real parse, type and linear leak
errors precede link analysis. Reference/path/source limits refuse without a
successful document or newly created HTML file. Existing output bytes and input
source bytes remain unchanged; the Rust Unix case also checks final input
symlink refusal. The proof status stays unverified.

Actual validation against the coordinator's integrated `db99085` executable at
`/workspace/vesper/target/parallel-next/debug/vesper` passed:

- All five independent executable Rust tests, zero failures and zero ignored.
- The Python harness's 49 actual commands, with exact local inventories,
  UTF8/CRLF reference ranges, complete unique HTML fragment targets, deterministic
  escaped output, actual failure codes/spans and preserved source/output bytes.
- Standalone compilation and strict standalone Clippy with `-D warnings`,
  rustfmt, Python syntax compilation and whitespace checks.

The first executable pass identified two incorrect corpus assumptions. An
unmatched backtick is literal, so its bracketed content remains eligible for
resolution; the group-boundary case now uses a known reference before the
boundary and still requires the exact unknown span afterward. Adjacent
`[present] [present]` pairs are excluded reference-style Markdown, so the strict
1025-reference refusal now uses semicolon-separated occurrences. Both tests keep
their intended actual diagnostic/refusal requirements; production limits and
behavior were not changed for these corrections.

Before feature integration, actual ordinary `vesper check` also passed the local,
exclusion, ambiguous namespace and generic declaration fixtures. Those checks
were fixture baselines, distinct from the feature runs above. No source
execution, guest/component, Lean, certificate, other-platform, timing or RSS
result is claimed here. Root runs integrated Cargo and workspace checks
separately; hosted native evidence remains pending.

Final hardening rerun: the same actual executable path rebuilt from the
coordinator's final source `b884a8c` (integration head `9c12e67`, including library
commit `2f94869`) again passes all five Rust executable tests with zero failures
or ignores, and all 49 Python commands. This follow-up changes only this report;
no test expectations, production code, limits, caches or build files changed.
These remain local Linux results, without a BuildBuddy or other-platform claim.
