# Self-hosting SH5 lane 15, part 1: the doc renderers in Vesper

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1338 (part 1 of it). Base: `main` at `bbda89e`. Lane prompt:
[`docs/agent-prompts/self-host-sh5/15-vesper-doc.md`](../agent-prompts/self-host-sh5/15-vesper-doc.md).
Rust stays the default: the default compiler installs nothing, so every
`vesper doc` output there comes from the Rust code, which is unchanged
and not deleted. Only the non-default hybrid build (`crates/vesper-hybrid`)
serves the ported renderers from Vesper.

## What changed

| Area | Files |
| --- | --- |
| Package `@vesper/doc` | `packages/vesper-doc/{Loom.toml,src/lib.ves,src/json.ves,src/page.ves}` (636 lines) |
| Swap point | `crates/vesper-doc/src/swap.rs` (new), 3 hunks of `crates/vesper-doc/src/lib.rs` |
| Hybrid module `doc` | `crates/vesper-hybrid/modules/doc/**`, `port/doc.rs` (generated binding), the `doc` rows of `build.rs`, `src/hybrid.rs`, `Cargo.toml` (feature `divergent-doc`) and `BUILD.bazel` |
| Tests | `tests/self_host_doc.rs`, `crates/vesper-hybrid/tests/doc.rs` |
| CI | `BUILD.bazel` (`doc_package`, test data and a `long` timeout), `crates/vesper-serde/BUILD.bazel` (its Loom package is visible), `.github/workflows/hybrid.yml` (paths, the divergent-doc step) |

Every `vesper doc` output goes through three functions of `vesper-doc`,
and part 1 ports those:

- `to_json` (every `--json` report and every `search-index.json`):
  serde_json's pretty writer into a writer bounded by the byte budget, then
  one newline. `json.ves` rewrites the compact serialization of the same
  value (`serde_json::to_vec`) into the exact `PrettyFormatter` bytes:
  strings and numbers are the same bytes in both forms, so only the
  whitespace between tokens is written. It has no depth limit, like
  serde_json's writer. The budget errors are the two Rust texts:
  "bounded JSON output: output byte budget exceeded" when the text does not
  fit, and "output byte budget exceeded" when only the final newline does
  not.
- `render_html` (the static page every file page starts from): the fixed
  header, the item overview and the escaped typed metadata. The overview is
  read from the pretty JSON through the `@vesper/serde` tape (`doc_parse`,
  `doc_field`, `next_decoded`), exactly as Rust reads it with
  `serde_json::from_slice::<Value>`: `document` if the top level has it,
  then `functions`, `types`, `effects` and `traits` when they are arrays,
  each item's `name` (a non-string `name` is "item", a missing one falls
  back to `declaration.name`) and `docs`, decoded and HTML-escaped.
  serde_json refuses a document with 128 nested containers and the page is
  written without the overview; the tape gets `max_depth` 127, which
  refuses the same documents.
- `render_document_html` (the linked page under every codex page): Rust
  still runs the link checks (`document_targets`, `validate_report`,
  `render_preflight`, the retained-text checks), then the port writes the
  whole page: the static page, the local declarations and the comments with
  their links.

`vesper_doc::swap` is the swap point: a `RenderTable` of three functions,
installed once (`install_render`) like the parser's `ParseTable`. With no
table installed, which is the default compiler, nothing changes. With one
installed, the three functions above send a request (the byte budget, the
compact JSON and, for a linked page, the validated targets, comments and
references, as little-endian words and length-prefixed bytes) and decode a
result (a status byte, then the bytes or the error text). The same encoder
serves the hybrid glue and the tests. `format()` in the package and
`swap::FORMAT` must agree, or the hybrid aborts at start-up.

## Differential

Both differentials run with the table installed after the Rust outputs have
been recorded, through the same public functions, and compare bytes, error
texts included. Numbers are from local release runs on x86-64 Linux.

| Test | Inputs | Result |
| --- | --- | --- |
| `tests/self_host_doc.rs` `renderers_match_rust` (JIT) | every repository `.ves` file and doc fixture `document_source` accepts (213 documents, 4 of them with intra-doc links; each `to_json`, `render_html`, `render_document_html`) and the full `vesper doc --json` report of each (213, replayed in key order), 3,000 fuzzed values (strings with escapes, markup, controls and non-ASCII text, numbers, the overview's keys in every shape, nesting of 120-135 containers), 14 values at the exact reader limit, and budget sweeps around every output length | 4,186 cases: **0 differ**. 1,649 pages have an overview, 1,036 renders fail on their budget |
| `crates/vesper-hybrid/tests/doc.rs` (hybrid against the default compiler) | `vesper doc --json`, `-o` and `--bundle` on all 981 `.ves` files of the repository, and `--json` and `--bundle` on its 99 Loom packages: 3,141 runs per compiler, 663 that document (19.0 MB of reports, pages, overviews, search pages and search indexes) | **0 byte differences** in exit status, stdout, stderr or any written file |
| `doc_module_runs_from_vesper` | `VESPER_HYBRID_STATS=1 vesper-hybrid doc --json examples/counter.ves` | the doc module answered from Vesper |

The harnesses can fail:

- `--features divergent-doc` adds one space to every linked page
  (`build.rs` and `BUILD.bazel` edit `modules/doc`): 438 of the 3,141 runs
  differ, the first being `doc --bundle bootstrap/corpus/ackermann.ves`
  (the codex page no longer ends in `</html>\n`). The Hybrid compiler
  workflow asserts this.
- With `&gt;` changed to `&gt` in `page.ves`, the JIT differential reports
  the first difference in every page (checked by hand, not in CI).

## Numbers

ABBA timing of `to_json` plus `render_html` over the CLI reports of every
4th corpus file (53 reports, 475,568 compact JSON bytes), release,
`VESPER_DOC_SPEED=1 cargo test --release --test self_host_doc speed --
--ignored`. The Vesper side is all of what the swap does: the compact
serialization in Rust, the request, and the Vesper call.

| Run | Rust (A, A) | Vesper (B, B) | Vesper / Rust |
| --- | --- | --- | --: |
| 1 | 23.3 ms, 24.1 ms | 22.2 ms, 18.9 ms | 0.87x |
| 2 | 21.4 ms, 22.5 ms | 19.1 ms, 18.6 ms | 0.86x |
| 3 | 22.8 ms, 31.2 ms | 19.7 ms, 21.3 ms | 0.76x |

The port is faster mainly because the Rust page parses its own pretty JSON
into a `serde_json::Value` tree to find the overview's items, while the
port indexes it once into a flat tape and looks the fields up in place.
End to end, the hybrid's 3,141 `vesper doc` runs took 15.80 s against
16.18 s for the default compiler (0.98x), most of which is the checker.

## Stage 1 guard

The lane touches no file under `src/verified_codegen/`, `src/ir.rs`,
`ir_opt`, `lean/`, `bootstrap/` or `packages/vesper-compiler/`, and
nothing in the root package's sources: the only compiler-side change is the
swap point in `crates/vesper-doc`, which is a no-op until a table is
installed, and only `vesper-hybrid` installs one. The verified backend's
pipeline never reaches a doc renderer. The `bootstrap-stage1` workflow on
the branch checks every digest of `bootstrap/run.sh stage1 --reproduce
--outcome` against `bootstrap/stage1.lock`.

## Compiler gaps

None blocked this part: the package checked, formatted and passed the
differential on the first build. Two things a later part will meet:

- **The `@vesper/serde` tape caps nesting at 128** (`hard_depth()` in
  `index.ves`, from `HARD_DEPTH` in `src/index.rs`), whatever `max_depth`
  asks for. serde_json's writer has no limit, so the pretty writer cannot
  be built on the tape and scans the compact text itself. A part that reads
  deep DTO JSON (part 3's search-index reader, `hoogle::parse_index`) has
  the same limit as serde_json's reader, so it is not a gap there.
  Repro, read from `value` in `index.ves`: `doc_parse` of 129 nested `[`
  raises `DepthLimit` even with `max_depth: 1000`.
- **Requests are bytes, not text.** An exported `in Str` binds to `&str`,
  so a request holding binary words must be `in Slice[u8]`; the JIT
  harness then builds the slice view and its borrowed header by hand
  (`call_bytes` in `tests/self_host_doc.rs`). Not a gap, but the check
  harness's `Entry` only covers `in Str`.

## What is left (later parts of #1338)

1. The page renderers that still format in Rust: `visual::render_codex_page`
   (badges table, layout tables and SVG diagrams, typestate SVG),
   `heatmap::render`, `footprints::render`, `modes::render`,
   `capabilities::render{,_package}`, `package::render_impls`,
   `package::render_overview` and `hoogle::search_page`. Each takes a
   DTO that serializes to JSON, so each can cross as compact JSON read
   through the tape, like `render_html` here.
2. The analyses: `hoogle` (index build, the strict index reader, text
   queries and search), `links` (targets, analysis, validation),
   `badges`, `capabilities`, `heatmap`, `modes`, `visual` (wire and native
   diagrams), `sim` (the doctest device journal) and the structural search.
3. The doc model: `Document` construction from the checked module
   (`src/tooling/doc.rs` `build_document`, `analysis`, the source
   preflight and `src/tooling/doc/{certs,links,sources}.rs`), which needs
   the checked AST across the boundary (export ABI v4 trees, or the
   `@vesper/ast` parse once the hybrid swaps it in).
4. `src/doc_commands.rs` (options, bundle layout, file discovery), with
   the driver lane (SH5 lane 12).
