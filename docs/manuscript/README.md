# Vesper language manuscript

This directory is the sole source of truth for the Vesper language and platform
specification. Edit `content.typ` and `main.typ` here; the published PDF is a
generated artifact. External documents in `source-manifest.json` record provenance,
not an alternative specification to maintain.

The published manuscript is [vesper-lang.org/vesper-specification.pdf](https://vesper-lang.org/vesper-specification.pdf). It presents the complete end-state Vesper specification organized into Parts I–VIII (Chapters 1–41 and Appendices A and B). The website's three **Read the design** actions open this PDF.

## Editable sources

- `main.typ`: title page, specification overview note, contents, typography, page layout, and running matter.
- `content.typ`: the complete native Typst specification monograph across Parts I–VIII, including inference-rule layouts, tables, code listings, mathematics, and internal anchors.
- `source-manifest.json`: source provenance, taxonomy metadata, transformation summary, and SHA-256 content digests.
- `verify.py`: checks the compiled document's part/chapter/appendix inventory, bookmarks, code preservation, embedded fonts, and text geometry.

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

## Verify

Optional publication QA requires Python and PyMuPDF:

```sh
python docs/manuscript/verify.py --typst /path/to/typst
```

The verification covers document structure (Parts I–VIII, Chapters 1–41, and Appendices A–B), source-code preservation, font embedding, and page geometry. Implementation workflows are documented in [the documentation index](../README.md).
