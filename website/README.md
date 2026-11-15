# Vesper website

The public language website at **https://vesper-lang.org/**. It presents the end-state language and ecosystem from the supplied Vesper Language Design & Formal Specification.

## Build

Requires Node.js 22 or newer. No package installation is needed.

```sh
cd website
npm run build
npm run check
```

Serve `website/dist` using any static HTTP server. The build includes the three compiler examples from the repository and checks that the displayed/downloaded source preserves their tokens. Code highlighting is generated as static HTML. JavaScript handles the file and learning tabs, Dusk/Dawn theme switch, clipboard, navigation and the design skill's motion; it does not simulate compiler execution.

`site.css` and `site.js` contain Vesper-specific presentation and behavior. `brand/` contains Appendix B's canonical Disjoint Field-Tree Prism SVG, in its approved Dusk and Dawn palettes. The favicon uses Appendix B's separate pixel-snapped 16px geometry and follows the browser's light/dark preference with the approved palettes. Wordmarks use Space Grotesk Bold; prose uses Inter and code uses JetBrains Mono. Amber, cyan and iris preserve the brand's ownership, mutation and effect roles.

`assets/` contains the unchanged design-taste-frontend runtime files used by the page. `DESIGN.md` records the design choices and copy sources. Browser QA and the skill's finish gate run on `dist/index.html` with `--root dist`.

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

The three “Read the design” actions open `/vesper-specification.pdf`. This is the complete, searchable Typst manuscript with sections 1–44 and appendices A/B, linked contents, PDF bookmarks, and internal cross-references. Editable sources and provenance are in `docs/manuscript/`.

GitHub Pages CI rebuilds the PDF using pinned Typst 0.15.1 and its bundled fonts before building the site. The checked-in PDF supports the existing local Node-only website build. See `docs/manuscript/README.md` for the typesetting command and verification details.
