# Vesper language manuscript

The published manuscript is [vesper-lang.org/vesper-specification.pdf](https://vesper-lang.org/vesper-specification.pdf). It contains all 44 numbered sections and appendices A and B. The website's three **Read the design** actions open this PDF.

## Editable sources

- `main.typ`: title page, publication note, contents, typography, page layout, and running matter.
- `content.typ`: the complete native Typst manuscript, including tables, code, mathematics, and internal anchors.
- `source-manifest.json`: the retrieved source revision, supplemental documents, import history, and content checksum.
- `verify.py`: checks the compiled document's chapter inventory, bookmarks, code preservation, embedded fonts, and text geometry.

The content was imported from the full rich Google document, Section 44's Markdown supplement, and the separate Appendix B. Pandoc 3.14 was used once during import to translate rich document structure and TeX mathematics into native Typst. Regular builds require only Typst; no package downloads, source-document credentials, Pandoc, or system fonts are needed.

## Rebuild

Use **Typst 0.15.1**. From the repository root:

```sh
typst compile --ignore-system-fonts --root . docs/manuscript/main.typ website/vesper-specification.pdf
cd website
npm run build
npm run check
```

GitHub Pages runs the same typesetting command with the same pinned compiler before building the website. Changes under `docs/manuscript/` trigger that workflow. The checked-in PDF also supports the existing local Node-only website build.

The publication uses PDF 1.7, embedded Libertinus Serif, DejaVu Sans Mono and New Computer Modern fonts, searchable text, a linked contents page, and PDF bookmarks. The bundled-font flag keeps layout independent of installed fonts.

## Verify

Optional publication QA requires Python and PyMuPDF:

```sh
python docs/manuscript/verify.py --typst /path/to/typst
```

The verification covers document structure, source-code preservation, font embedding, and layout. It does not execute the supplied Vesper, Rust, or Lean examples. The initial import was also compared against every prose, list, and table block in the intermediate source document; results are recorded in `VALIDATION.md`.

## Editorial treatment

Sections are ordered numerically. Duplicate source titles and private word-processor formatting markers are removed. Section 35's indentation was recovered from the repository snapshot after confirming that its text matched the current source exactly apart from whitespace. Nested lists, emphasis, links, tables, and code are retained.

Section 44's conversational opening is rendered as manuscript prose. One mixed-language phrase, `TSC锚点`, is translated as “TSC anchor points.” A wide inference rule is laid out with aligned premises and conclusion while preserving every symbol and condition. Appendix B's integration preface is represented by the updated specification map; the full canonical appendix follows Appendix A.

Code blocks use consistent, high-contrast monochrome typography. Section cross-references are linked to stable manuscript anchors. The original compiler-facing `docs/specification.txt` snapshot and its revision inventory remain unchanged.
