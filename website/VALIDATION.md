GATE: PASS — no open failures.

# Vesper website validation — 14 November 2026

The redesign follows Appendix B and the intended language design. The user explicitly chose to incorporate copy and selected components from `vesper_official_website_website_index_html.html` into this design, rather than replace the site wholesale.

## Final checks

- `npm run build` / `npm run check`: pass. Local assets, anchors, unique IDs, tab references, included source tokens and the custom domain are checked.
- Final layout checker: **no findings** at 1440×900, 1024×768, 820×1180, 768×1024 and 390×844, including headline stress tests. Desktop and phone screenshots were visually reviewed. The phone Learn section grows to fit its comparisons.
- Copy lint: **0 failures, 0 warnings**. Voice: the engineer; direct and specific, using language-design terms where useful. The software category and zero-GC positioning are explicit.
- The initial full consistency review exercised five widths, reduced motion and script-failure cases. Findings were corrected through subsequent browser reviews. The final finish gate checked desktop/phone and walked 320px for overflow, collisions and tap targets: **no open failures**, two reviewed waivers and 24 non-blocking warnings.
- Theme switching logic was exercised in both directions: palette, browser theme color and header/hero SVG switch correctly; dark code surfaces and the footer retain Dusk marks. A Dawn-state browser rendering was visually reviewed as well as the default Dusk page.
- Source file tabs, learning comparison tabs, clipboard/download controls and navigation use the existing runtime. Native checkbox semantics and a visible focus ring make the Dusk/Dawn switch keyboard accessible.
- Final performance review: **0 failures, 4 warnings**, across desktop and phone, three runs per size. Transfer was **251.8 KiB** desktop and **245 KiB** phone. Layout shift was **0.003 / 0.000**. No broken requests. These are local CPU-browser measurements, not field data or compiler benchmarks.

## Reviewed waivers

| Finding | Reason |
| --- | --- |
| `card-layout`, ecosystem group | One coherent 2×2 group contains four different source fragments and real repository destinations. The skill permits a consistent feature-card family; its heuristic counts sibling cards as repeated section layouts. |
| `copy-headings`, language heading group | Ownership, field projections, async traits, effect evidence and source are concrete programming-language concepts from the supplied material. The general-business noun dictionary does not recognize them. |

The former footer-logo alignment waiver was removed. The footer now uses the canonical SVG lockup.

## Retained non-blocking warnings

The gate's 24 consolidated warnings were visually reviewed:

- `nav`, `nav-labels`, `nav-current`: the canonical logo includes its required padding; concise navigation labels map to descriptive section headings; there is no scroll-based active-section marker. Destinations and the mobile menu work.
- `label-centre`, `action-repeat`, `button-contrast`: the primary action includes the kit's chevron and appears in both header and hero; the secondary action uses a quiet dark fill with readable text. These are consistent controls.
- `scroll-reveal`, `pinned`, `fade-cut`, `container-edges`: the compiler's border transitions, held frame, crossfade and inset steps belong to the unchanged pinned-chapters runtime. Static phone and reduced-motion views remain readable. Text antialiasing at comparison-panel edges also triggers the fade heuristic; the complete programs remain visible in the final views.
- `cta`, `copy-headings`: the explanatory compiler section does not add a redundant action; ecosystem headings name software domains.
- `cut-text`, `stagger`, `section-gaps`, `no-visual`: the compound footer mark, padded mode badges, differing screen compositions and the uncluttered phone prism trigger heuristics. Final layout checks found no clipping, overlap or off-screen content. The hero is deliberately centered on the supplied brand mark.

Performance warnings are two per size: three font families are required by Appendix B, and the same sub-kilobyte SVG appears below the fold without lazy loading. That SVG is already loaded in the header and reused from cache. Three font files transferred about 100 KiB in total.

## Brand and material

Software; official Dusk/Dawn; Space Grotesk / Inter / JetBrains Mono; sharp component radii and pill actions; collage hero adapted to the client mark; subtle painterly shader within the source-contract panel; pinned-chapters compiler; canonical vector-wordmark close. No scene. Kit assets are unchanged.

The canonical master and favicon coordinates come directly from Appendix B. The Dawn master changes only approved colors. Uniform scaling and negative channels are preserved. The wordmark uses Space Grotesk Bold. Semantic source colors follow the three prism blades, with the supplied rose declaration color and emerald proof color.

Actual text pairings meet WCAG AA: Dusk token text is at least 8.84:1; Dawn amber badges use the official card surface at 4.67:1. The reference's blanket AAA claim is not repeated.

No assumed facts, invented name, stock photographs, generated logo, stand-in art, client media slots, forms or `.example` contacts. No release hashes, simulated execution presented as real, installer endpoint or benchmark telemetry from the reference was published. The real source-build guide and repository examples remain available. The canonical domain is https://vesper-lang.org/.

The final gate is the conclusive desktop/phone consistency result; the earlier five-width full report describes an earlier iteration, not a claim of a fresh exhaustive five-width consistency run after every edit. The final independent five-width layout review did run after the last layout changes.


## Published language manuscript

The design actions open the 228-page Typst PDF. The manuscript is organized into eight parts, 41 chapters, and appendices A/B; the advanced types supplement is integrated into the core theory. Site build checks cover all three PDF links and the PDF file header. GitHub Pages compiles the committed manuscript with pinned Typst 0.15.1 and bundled fonts. Full publication checks are recorded in `docs/manuscript/VALIDATION.md`.
