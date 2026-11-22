# Vesper website

The public language website at **https://vesper-lang.org/**. It compares Vesper with Rust using programs and rejection cases from the repository. The published specification describes the full language design.

## Build

Use Node.js 22 or newer, Typst 0.15.1, and the repository’s Rust toolchain. Build the Vesper CLI and Chroma before building the site. Set `VESPER_BIN` or `TYPST_BIN` if either executable is in a different location.

```sh
cargo build --locked -p vesper -p vesper-tools --bin vesper --bin vesper-highlight
rustup target add wasm32-unknown-unknown
cd website
npm run build:wasm
npm run build
npm run check
```

Serve `website/dist` using any static HTTP server. The build keeps repository downloads byte-for-byte and runs Chroma on Vesper listings. To annotate an intentional failure, add the expected code to its fence (`~~~vesper E0302`) or listing (`data-error="E0302"`). The build checks the example and uses the reported span. The tabs and theme switch use `site.js`. Readers can see both tab panels with scripts disabled.

`index.html` holds the comparison, compiler overview and build guide. `site.css` defines the responsive layout and themes. `site.js` handles the theme switch and clipboard. The field table uses the starting values and updates in `examples/projections.ves`. The Vesper mark and favicon use Appendix B’s original geometry.

The [language guide](language.html) is a standalone HTML page. Its table of contents links directly to worked examples. Edit the guide in place and run the website checks to validate its links and section IDs.

Edit the [guided tour](tour.md) to update the lessons at `/tour/`. Use a level-one heading for each subject and a level-two heading for each step. Fence listings with `~~~vesper` or `~~~sh`. The build generates a static page per step with a contents menu and previous and next links.

The documentation homepage is published at `/docs/`. Edit the lessons in [the eight documentation parts](docs/). Use a level-one heading for a chapter or appendix and a level-two heading for each numbered section. Keep the manuscript number before each plain-language heading. Blank lines separate paragraphs, backticks mark inline code, four spaces indent a trace, and `~~~vesper` fences enclose Vesper source.

The build produces static chapter guides, a search index for the guides and tour, and a sitemap without retired tour URLs. It uses printed numbers and Typst labels to link each chapter or section to both the HTML and PDF editions. Docs search requires JavaScript. Contents, listings, and reference links work without it.

## Browser compiler

Install the browser target with `rustup target add wasm32-unknown-unknown`, then run `npm run build:wasm` from `website` before `npm run build`. The site copies the Wasm module into `dist`. Serve the directory with a static HTTP server and open `/tour/`.

The worker uses Vesper’s parser, bounded checker, evaluator, Forge, Lean extractor, and formatter. Chroma adds semantic highlighting when the original source checks and falls back to syntax highlighting for incomplete source. Repair lessons show one starting program and disclose the solution separately. Unit tests and selected table rows run through Vesper’s test discovery and evaluator. The worker has a source size limit, evaluation fuel, and a time limit. It has no host clock, filesystem, or native JIT imports.

## Publish

`.github/workflows/website.yml` builds and checks the site on relevant pull requests. Pushes to `main` publish the built artifact to GitHub Pages, with the `github-pages` environment and OIDC. Changes to the included compiler examples also rebuild the site. The workflow can be run manually.

GitHub Pages uses the custom domain `vesper-lang.org`; `CNAME`, canonical URLs, the sitemap and share metadata all use that domain.

Cloudflare DNS (DNS only, proxy disabled):

| Type | Name | Target |
| --- | --- | --- |
| A | @ | 185.199.108.153 |
| A | @ | 185.199.109.153 |
| A | @ | 185.199.110.153 |
| A | @ | 185.199.111.153 |
| AAAA | @ | 2606:50c0:8000::153 |
| AAAA | @ | 2606:50c0:8001::153 |
| AAAA | @ | 2606:50c0:8002::153 |
| AAAA | @ | 2606:50c0:8003::153 |
| CNAME | www | robot-head.github.io |

GitHub Pages provisions the TLS certificate; HTTPS is enforced for the configured domain. The www hostname redirects to the canonical apex domain. See [GitHub's custom-domain documentation](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/managing-a-custom-domain-for-your-github-pages-site).

## Language manuscript

The Specification and Read the design links open `/vesper-specification.pdf`. This is the complete, searchable Typst manuscript spanning Parts I–VIII (Chapters 1–42 and Appendices A and B), with linked contents, PDF bookmarks, and internal cross-references. Editable sources and provenance are in `docs/manuscript/`.

GitHub Pages CI rebuilds the PDF with Typst 0.15.1 and the bundled fonts. Local builds use the checked-in PDF. The site build generates `/docs/reference.html` from the same source using `docs/manuscript/web.typ`. It validates the manuscript links, listings, and inference rules. See `docs/manuscript/README.md` for PDF verification.
