# Wave nine lane 2: Vesper Chroma `vesper shot` code cards (§43)

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

Claim: #579. Branch: `claude/project-thread-248b49`. Base: lane 1's PR #613 head `3194955` (on main `c9f9677`, descendant of the required baseline `37f7b8b`); this PR merges after #613.

## What was built

`vesper shot FILE -o OUT.{svg,png} [--theme dusk|dawn] [--dpi 1-4] [--title T] [--verify] [--embed-font] [--store DIR | --no-cache] [--json]`
renders a Vesper example as a vector SVG or a 1x-4x PNG code card with no
browser, no system fonts and no network.

- `crates/vesper-highlight/src/shot/` (behind the crate feature `shot`, so the
  wasm build of lane 1's highlighter does not pull in the renderer):
  - `directives.rs`: the `@shot(title = .., theme = .., dpi = ..)` header and
    Twoslash-style callouts. A `//  ^?` line under a token asks for that
    token's inferred type and mode; a `//^!` comment attaches a note, either
    trailing a code line or on its own line under a token. These are read from
    the text and removed from the displayed (and verified) source; they are
    not attribute syntax and are never compiled (§16.4 attribute syntax belongs
    to Codex). Malformed directives, unknown keys, duplicates, a misplaced
    header, carets past the end of the line or onto blank lines, trailing
    `//^?`, empty notes, control characters and over-limit input are typed
    `ShotError`s with the source line.
  - `font.rs`: `ShotFont` parses the font with `ttf-parser` and shapes each
    line with `rustybuzz` using the font's default features. Advances come from
    the font (`hmtx` through HarfBuzz positioning), not a fixed 8.4 px.
  - `scene.rs`: `CodeCardScene` / `build_code_card_scene` (§43.5). Each line
    is shaped once as a whole, so contextual ligatures form across the line;
    glyphs are then grouped into `PositionedGlyphRun`s by the highlight span
    their cluster falls in. A run records its exact source text, colour, the
    live-linear underline flag and whether the font substituted any glyph
    (`ligature`, the wire `semantic_flags` bit 2). The scene is a display list
    in CSS pixels: canvas, an eight-layer ambient elevation shadow, the card,
    window chrome (three dots, centred title, divider), code runs, wavy
    obligation underlines, callout pins (a pointer and a box under the token
    for `^?` and caret notes, a pill at the line end for trailing notes) and
    mode-legend pills (linear, inout, eff, wire/proof, only for classes the
    snippet uses). `resolve_callouts` maps carets to spans and asks the
    caller's resolver; the author's text after `^?` is the fallback, and a
    query with neither is an error.
  - `svg.rs`: deterministic SVG. Text stays `<text>` (selectable, searchable),
    one element per run, positioned at the shaped pen position with
    `textLength` set to the shaped width; numbers have at most two decimals.
    The font is referenced by family name, or embedded as a base64
    `@font-face` with `--embed-font`.
  - `raster.rs`: tiny-skia rasterisation at 1x-4x, filling glyphs from the
    font's own outlines. tiny-skia is built without its `simd` feature so all
    targets run the same scalar pipeline. Opaque cards are encoded with `png`
    directly (premultiplied equals straight alpha), avoiding tiny-skia's
    full-size demultiplied copy.
  - `cache.rs`: `shot_cache_key = BLAKE3(source digest, theme id, config)`;
    the config covers the title, the resolved pins, format, scale or font
    embedding and the font's digest. `SHOT_CACHE_SCHEMA` versions it.
- `src/shot_commands.rs` (root CLI):
  - `--verify` wraps the displayed example in one declaration doc fence
    (entry `main`) and runs it through `vesper::doctest::run`, the Codex
    doctest sandbox of §40.1 (checker, one shared JIT module, evaluator
    fallback). A failure in checking or at run time prints the diagnostics
    and writes nothing.
  - `//^?` queries are answered by the checker: the CLI checks the displayed
    source with `tooling::frontend::check_source` and asks Lens hover
    (`lsp::navigation::hover`) at the token, and accepts the answer only when
    hover's range is exactly the token (hover otherwise falls back to an
    enclosing expression, which would describe something else).
  - Outputs are cached in the Loom content-addressed store (`--store`, else
    `$VESPER_STORE`, else `~/.vesper/store`) as a derivation
    (`vesper-shot`, `SHOT_CACHE_SCHEMA`, option `key` = the BLAKE3 key, input
    `source` = the displayed source object). A hit returns the stored bytes,
    which the store rehashes on read.
- `assets/fonts/jetbrains-mono/`: JetBrains Mono 2.304 Regular (SIL Open
  Font License 1.1, `OFL.txt`), from the upstream GitHub release
  `JetBrainsMono-2.304.zip`; SHA-256 of the TTF
  `a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f`.
  JetBrains Mono forms `->`, `=>` and `::` through `calt`: shaping gives a
  spacer glyph `SPC` plus `hyphen_greater.liga`, `equal_greater.liga` or
  `colon_colon.liga`.

## Acceptance

- Golden SVG bytes for three fixture cards (the §43.2 `wal_txn.vsp` example,
  a checkable linear transaction with three callouts, and the linear-effect
  counter) in Vesper Dusk and Vesper Dawn, and BLAKE3 hashes of the RGBA
  pixels at 1x and 4x (`tests/fixtures/shot/golden/`). Every render is done
  twice and compared byte for byte (scene, SVG, RGBA, PNG).
- Ligatures: `->`, `=>` and `::` shape to the font's `.liga` glyphs, none of
  them the character's nominal glyph; in a card the `->` run holds
  `hyphen_greater.liga` and is marked as a ligature in the SVG.
- Advances: every code run's width equals the sum of its shaped advances, and
  runs start where the preceding text ends.
- `--verify` rejects `reject_check.vsp` (E0302, a linear value never
  consumed) and `reject_runtime.vsp` (checks, then `Fail::raise` escapes at
  run time), writes no output for either, and accepts `transaction.vsp`.
- Cache: a second render with the same store is a `hit` with identical bytes
  for SVG and PNG; a different theme or dpi is a different key and a miss.
- Bad arguments, an invalid `@shot` and unsupported output extensions fail.

## Measurements

`cargo test --release --test shot_measure -- --nocapture` on the cloud host
(4 vCPU Intel Xeon @ 2.10 GHz, one thread, other builds running), for the
§43.2 `wal_txn.vsp` card (19 lines, 796x622 CSS px, 152 primitives, Tier 0
spans). Peak heap is the counting global allocator's peak live bytes above
the warmed-up baseline (heap only, not RSS).

| Step | Mean time | Peak heap | Output |
| --- | --- | --- | --- |
| scene only (shaping + layout) | 1.27 ms | 45.7 KB | |
| scene + SVG | 1.45 ms | 80.5 KB | 19.2 KB |
| scene + PNG 1x | 8.99 ms | 2.58 MB | 156 KB |
| scene + PNG 2x | 20.7 ms | 9.60 MB | 380 KB |
| scene + PNG 4x | 49.9 ms | 37.0 MB | 862 KB |

The 4 ms target is met for SVG cards and missed for PNG cards at every scale.
An earlier quiet run of the same renderer measured about 5.5 ms (1x) and
38 ms (4x); the table above is the recorded run.

## Commands and results

- `cargo test --test shot_cards`: 9 passed (golden SVG bytes, RGBA hashes for
  3 cards x 2 themes x 1x/4x, ligatures, advances, pins and chrome, typed
  errors, `--verify` rejections, checker queries, cache hits, CLI arguments).
- `cargo test -p vesper-highlight --features shot`: lane 1's suites plus the
  shot unit tests (directives, block comments and multi-line strings), all
  passing.
- `cargo test --release --test shot_measure -- --nocapture`: the table above.
- `cargo +1.100.0 clippy -p vesper-highlight --features shot --all-targets -- -D warnings`: clean.
- `cargo +1.100.0 clippy --bin vesper --test shot_cards --test shot_measure -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.
- The full `cargo test --workspace --locked` was not run locally (it takes
  well over an hour on this host); CI runs it.

## Root patch and shared files

- `crates/vesper-highlight/Cargo.toml`: the `shot` feature and its pinned
  dependencies (own commit): `rustybuzz =0.20.1`, `ttf-parser =0.25.1`,
  `tiny-skia =0.11.4` (default features off, `std` + `png-format`),
  `png =0.17.16` (already in Cargo.lock), `base64 =0.22.1` (already in
  Cargo.lock), `blake3` as lane 1 pins it.
- `crates/vesper-highlight/src/lib.rs`: one line, `#[cfg(feature = "shot")]
  pub mod shot;` (lane 1's file; additive).
- Root `Cargo.toml`: the root package's `vesper-highlight` dependency enables
  `features = ["shot"]`; `Cargo.lock` regenerated by cargo.
- `src/main.rs`: `mod shot_commands;` and the `shot` route (own commit).

## Decisions

- Font: JetBrains Mono rather than Fira Code: same licence and ligature
  approach, 274 KB, and its `post` table keeps glyph names, which lets the
  test name the ligature glyph it expects.
- The card is drawn from the font's outlines in the PNG and referenced by name
  in the SVG. With `--embed-font` the SVG is self-contained (adds ~366 KB).
  Without it, a viewer lacking the font falls back to a monospace font;
  `textLength` keeps every run at its shaped width either way.
- The "CST digest" in the cache key is a digest of the displayed text and its
  lossless highlight partition (every span's range, class and live-linear
  flag): that partition is derived from the lossless CST and covers every
  byte, and lane 1 does not publish a separate CST hash.
- CLI flags override the `@shot` header; defaults are Vesper Dusk and 4x.
- Shot fixtures use the `.vsp` extension (as §43.2 does), so they are outside
  the `vesper fmt --check` set of tracked `.ves` files; the `@shot` line is
  not Vesper syntax.

## Remaining gaps

- The 4 ms per-card target is met for SVG output, not for PNG (see the
  measurements); 4x cards are dominated by filling 6-9 megapixels and PNG
  deflate on one core.
- The doctest sandbox does not run examples with handlers or closures
  ("closures/handlers unsupported before checking"), so `--verify` rejects
  those even when they are correct; `counter.vsp` renders but cannot be
  verified.
- `//^?` answers come from lane 1's Tier 1 hover table (which covers linear
  binding sites, e.g. `tx : WriteTxn (mod linear)`), then Lens hover (types
  of expressions and parameters, e.g. `WriteTxn : mod linear`), then the
  author's text; the two checker sources format their answers differently.
  A snippet that does not check gets only the author's text.
- Callout pins show text only; Ariadne-style multi-span labels, wire struct
  byte-span diagrams and E0301 resumption-frame annotations are not drawn.
- Right-to-left and complex-script text is shaped but not bidi-reordered;
  glyphs missing from the font render as `.notdef`.
- Rust ownership of the renderer's types says nothing about Vesper's source
  typing rules; `--verify` relies on the existing checker and sandbox.
