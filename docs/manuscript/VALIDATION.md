# Manuscript publication checks

Edition: 14 November 2026. Compiler: Typst 0.15.1, bundled fonts only.
Applied revision: `0001-manuscript-unify-end-state-specification.patch`.

| Check | Result |
| --- | --- |
| Architectural parts | I–VIII, each once and in order |
| Complete numbered chapter inventory | Chapters 1–41, each once and in numerical order |
| Appendices | A and B in Part VIII |
| Pages | 228 |
| PDF bookmarks | 280 |
| Hyperlinks and internal cross-references | 534; all internal PDF destinations are valid |
| Source-code preservation | All 157 complete blocks survive normalized PDF text extraction |
| Patch source-code fidelity | All 157 fenced listings are identical to the supplied patch |
| Fonts | All 7 used font subsets embedded |
| Missing text glyphs | None found |
| Text exceeding checked margins | None found |
| Nearly empty spill pages | None found |
| Typst compilation diagnostics | None |
| Website design actions | All three point to the PDF; no specification-text link remains |

All 228 pages were rendered into contact sheets and reviewed. Detailed renders covered the cover and contents, part openers, tables, formal rules, code listings, and both appendices. Two wide inference-rule premises were split across lines. Four code listings use slightly tighter line spacing to eliminate isolated short continuation pages while retaining their font size and exact source text. Unsupported symbol names and TeX-style math escapes in the supplied patch were corrected to native Typst notation.

The source-code comparison uses Typst's evaluated raw blocks and actual PDF text. A separate comparison against the applied patch confirms that no code listing changed during the publication repairs. All 273 document labels are unique and all 416 source-level internal cross-references resolve. Mathematical expressions were visually checked; this is a publication check, not an independent proof audit.

Two successive local compilations with the pinned compiler and bundled fonts produced identical PDF bytes. The website build checks verify that the published file is a PDF and that every design action opens it. Publication counts, the PDF digest, source digests, and the applied patch digest are recorded in `source-manifest.json`.
