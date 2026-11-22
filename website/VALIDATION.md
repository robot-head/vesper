# Vesper website validation

The site build uses the repository compiler and Chroma for examples, the Wasm checker and evaluator for interactive lessons, and Typst 0.15.1 for the HTML reference. Run the commands in [README.md](README.md) from the same checkout.

## What the site check verifies

- Local links and fragment targets, unique element IDs, tab controls, and PDF destinations.
- Canonical URLs and active tour navigation. Retired lesson pages link to their replacements and stay out of the sitemap.
- Byte-for-byte repository downloads, Chroma output on Vesper listings, and real diagnostic codes for annotated failures.
- Tour starter formatting, expected compiler failures, test-case selection, and evaluation with a Wasm module that imports no host functions.
- All 233 numbered manuscript sections in search, together with entries for the language guide and active tour.
- The HTML manuscript’s code listing and inference rule counts against the Typst source.

The landing page has a five-stage pipeline, linked platform groups, Bazel and Cargo instructions, and a projected-loan comparison. All site sections share navigation and preserve the Dawn or Dusk theme. The published build no longer packages the legacy template kit.

## Manuscript checks

GitHub Pages compiles the PDF from `docs/manuscript/main.typ`. The publication verifier checks document structure, named destinations, linked contents, code text, and layout. The HTML reference uses the same body source with browser rendering for equations, tables, code, and inference rules. Page counts can change when the manuscript or template changes.

Publication checks are distinct from implementation and theorem checks. See [manuscript review status](../docs/manuscript/REVIEW-STATUS.md) for the known technical boundaries.
