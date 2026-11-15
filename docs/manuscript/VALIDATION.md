# Manuscript publication checks

Edition: 14 November 2026. Compiler: Typst 0.15.1, bundled fonts only.

| Check | Result |
| --- | --- |
| Complete numbered chapter inventory | Sections 1–44, each once and in numerical order |
| Appendices | A and B, following Section 44 |
| Pages | 233 |
| PDF bookmarks | 281 |
| Hyperlinks and internal cross-references | 414 |
| Source-code preservation | All 165 complete blocks survive normalized PDF text extraction |
| Prose, list, and table preservation | All 1,649 math-free source blocks survive normalized PDF text extraction |
| Fonts | All 9 used font subsets embedded |
| Missing text glyphs | None found |
| Text exceeding checked margins | None found |
| Nearly empty spill pages | None found |
| Typst compilation diagnostics | None |
| Website design actions | All three point to the PDF; no specification-text link remains |

All pages were rendered into layout contact sheets and reviewed. Detailed page renders covered the title and contents, tables, formal rules, source listings, Section 44's equations, the mechanized appendix, and the identity appendix. A wide inference rule was reformatted as a multiline fraction, table rows were kept together, list nesting was restored, and a short code spill page was eliminated before the final review.

The source-code comparison uses Typst's evaluated raw blocks and the actual PDF text, rather than matching a second copy of the importer. Prose preservation was checked separately against the complete intermediate source structure. Mathematical expressions were typeset from the supplied source and visually checked; this is a publication check, not an independent proof audit.

Two successive local compilations with the pinned compiler and bundled fonts produced identical PDF bytes. The website build checks verify that the published file is a PDF and that every design action opens it. The original specification snapshot's Git bytes still match its recorded revision inventory; its Windows worktree uses CRLF line endings.
