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
An unclosed inline span or fence cannot suppress a reference after a blank line,
ordinary comment or source declaration breaks its doc-comment group. Script-like
text remains escaped, with no script/iframe tags or external hrefs.

Unknown references, a genuine type/effect namespace ambiguity, explicit Self,
package paths and generic paths must fail with their actual diagnostic code and
original byte span. Renaming a real declaration invalidates the old reference;
repairing it restores successful documentation. Real parse, type and linear leak
errors precede link analysis. Reference/path/source limits refuse without a
successful document or newly created HTML file. Existing output bytes and input
source bytes remain unchanged; the Rust Unix case also checks final input
symlink refusal. The proof status stays unverified.

Validation so far: standalone compilation of all five executable Rust tests,
strict standalone Clippy with `-D warnings`, rustfmt, Python syntax compilation
and whitespace checks pass. Actual ordinary `vesper check` passes for the local
inventory, exclusions, ambiguous namespace and generic declaration fixtures.
Those are fixture baselines, not evidence that the new link feature runs.

**New-feature executable results are pending the integrated implementation.**
The coordinator will supply the actual tuple-plus-links executable before this
lane runs its Rust/Python harnesses. No source execution, guest/component, Lean,
certificate, other-platform, timing or RSS result is claimed here.
