# Vesper language manuscript

This directory contains the Vesper language and platform manuscript. It is an
under-review design draft with unresolved correctness and security issues; see
[REVIEW-STATUS.md](REVIEW-STATUS.md). Edit `content.typ` and `main.typ` here; the published PDF is a
generated artifact. External documents in `source-manifest.json` record provenance,
not an alternative specification to maintain.

The published manuscript is [vesper-lang.org/vesper-specification.pdf](https://vesper-lang.org/vesper-specification.pdf). It presents the proposed Vesper language and platform design organized into Parts I–VIII (Chapters 1–42 and Appendices A and B). The website's three **Read the design** actions open this PDF.

## Editable sources

- `main.typ`: title page, specification overview note, contents, typography, page layout, and running matter.
- `content.typ`: the complete native Typst specification monograph across Parts I–VIII, including inference-rule layouts, tables, code listings, mathematics, and internal anchors.
- `source-manifest.json`: source provenance, taxonomy metadata, transformation summary, and SHA-256 content digests.
- `supplements/continuation-fission.typ`: the uploaded Chapter 6 source retained as provenance; edit the integrated chapter in `content.typ`.
- `verify.py`: checks the compiled document's part/chapter/appendix inventory, bookmarks, code preservation, cited Lean theorem names, embedded fonts, and body-page geometry.

Regular builds require only Typst 0.15.1; no package downloads, external credentials, Pandoc, or system fonts are needed.

## Rebuild

Use **Typst 0.15.1**. From the repository root:

```sh
typst compile --ignore-system-fonts --root . docs/manuscript/main.typ website/vesper-specification.pdf
cd website
npm run build
npm run check
```

GitHub Pages runs the same typesetting command with the same pinned compiler before building the website. Changes under `docs/manuscript/` trigger that workflow. The checked-in PDF also supports the existing local Node-only website build.

The publication uses PDF 1.7, embedded Libertinus Serif, DejaVu Sans Mono, and New Computer Modern Math fonts, searchable text, a linked contents page, and PDF bookmarks. The bundled-font flag keeps layout independent of installed fonts.

The current manuscript edition is 16 November 2026. Its editorial revision clarifies reading paths, summaries, and selected body explanations while preserving code listings and inference rules. [VALIDATION.md](VALIDATION.md) records the checks for this edition separately from earlier compiler and model validation.

## Verify

Optional publication QA requires Python and PyMuPDF:

```sh
python -X utf8 docs/manuscript/verify.py --typst /path/to/typst
```

The verification covers document structure (Parts I–VIII, Chapters 1–42, and Appendices A–B), source-code preservation, font embedding, and page geometry. Implementation workflows are documented in [the documentation index](../README.md).
