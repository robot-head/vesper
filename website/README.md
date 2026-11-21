# Vesper website

The public language website at **https://vesper-lang.org/**. It compares Vesper with Rust using programs and rejection cases from the repository. The published specification describes the full language design.

## Build

Requires Node.js 22 or newer. No package installation is needed.

```sh
cd website
npm run build
npm run check
```

Serve `website/dist` using any static HTTP server. The build includes the three compiler examples from the repository and checks that the displayed/downloaded source preserves their tokens. Code highlighting is generated as static HTML. The source explorer and comparison tabs use the shared keyboard navigation in `assets/system.js`. The full examples and comparisons are readable with scripts disabled.

`index.html` holds the comparison, compiler overview and build guide. `site.css` defines the responsive layout and themes. `site.js` handles the theme switch and clipboard. The field diagram in `brand/broker-fields.webp` illustrates the permissions in `examples/projections.ves`. The Vesper mark and favicon use Appendix B’s original geometry.

The [language guide](language.html) is a standalone HTML page. Its table of contents links directly to worked examples. Edit the guide in place and run the website checks to validate its links and section IDs.

The documentation homepage is published at `/docs/`. Edit the lessons in [the eight documentation parts](docs/). Use a level-one heading for a chapter or appendix and a level-two heading for each numbered section. Keep the manuscript number before each plain-language heading. Blank lines separate paragraphs, backticks mark inline code, and four spaces indent a code or trace block.

The build produces static chapter pages, an in-page search index, and sitemap entries. It reads the printed numbers and PDF destination labels from `docs/manuscript/content.typ` and checks for missing lessons. The contents, chapters, and PDF links work without JavaScript.

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

GitHub Pages CI rebuilds the PDF using pinned Typst 0.15.1 and its bundled fonts before building the site. The checked-in PDF supports the existing local Node-only website build. See `docs/manuscript/README.md` for the typesetting command and verification details.
