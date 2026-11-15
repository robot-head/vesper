# Wave nine lane 1: Vesper Chroma core (§43, claim #578)

Base: `c9f967763a0ab3752ba254d13b531b043831a3f8` (origin/main after plan PR #612,
a descendant of the required baseline `37f7b8b`). Branch:
`claude/project-thread-rv14nw`.

## What was built

- **`crates/vesper-highlight`** (new workspace member, §43.5).
  - `VesperTokenClass`: the 18 classes with the frozen `repr(u16)` values of
    §43.5, `ALL`, `from_u16`, `name`.
  - `HighlightSpan { start, end, class, is_live_linear, flags, hover_sym_id }`,
    `ChromaError` (the three §43.5 variants plus typed errors for oversized
    input, CST mismatch, wire decoding, invalid facts, hover overflow and
    OKLCH/fallback mismatch), `verify_lossless_spans` (as in §43.5).
  - **Tier 0** `highlight_tier0`: walks the leaf tokens of the
    `vesper-syntax` rowan CST (batch parser with error recovery) and
    classifies each token from its kind, its enclosing TYPE / ERROR / INCOMPLETE
    nodes and its significant neighbours. No regex, no grammar file, linear in
    tokens. Every byte belongs to exactly one span; the function re-checks the
    partition and returns `CstMismatch` rather than output if the CST ever
    disagreed with the source. Syntactic facts it uses: `linear effect X`
    declarations, `eff [...]` rows and `handle X` (effect names; `Fail` and
    `Cancel` are always abortable), `inout p.{a, b}` projection leaves,
    `@proof`, wire endian scalars (`u32be`, `i64le`, ...), contextual
    keywords (`pub use trait model wire type where region dim as for`),
    ligature operators (`-> => :: == != <= >= && ||`, flag bit 2), and a
    `RECOVERED` flag (bit 3, an additive extension) for lexer errors and
    recovery nodes.
  - **Tier 0 token mode** `highlight_tier0_tokens`: the same classifier over
    the lexer's lossless token stream without building the CST. Token
    boundaries are identical to CST mode (tested on every repository source)
    and on the test samples so are the classes; it only loses TYPE-node
    context for non-primitive lower-case type names and parser recovery
    flags. The wasm module uses it (see measurements).
  - **Tier 1** `highlight_tier1(src, &SemanticFacts)`: decorates Tier 0 spans
    with a facts DTO (`LinearBindingFact`, `FieldLeafFact`, `EffectFact`,
    `HoverEntry`). Live linear bindings get `LinearBinding` + `is_live_linear`
    (wire bit 0) from minting site through the consuming use; written inout
    leaves get `FieldLeafMut` + `MUTATED_LEAF` (bit 1), other field reads
    `FieldLeafRead`; effects become `EffectLinear` / `EffectAbortable` from the
    checked World. Facts outside the source or off a UTF-8 boundary are
    `InvalidFact`. `tier0_fallback(src, FallbackReason)` returns Tier 0 output
    with a typed reason (`Syntax`, `Check`, `Budget`, `NotRequested`). The CLI
    runs the checker through the guarded `tooling::frontend::check_source` at
    its hard maxima, so inputs whose inferred types grow exponentially (e.g.
    repeated `let x = (x, x);`) fall back as `Budget` in milliseconds. An
    argument with no mode keyword takes the callee parameter's mode.
  - **Wire record** `wire::ChromaTokenSpanWire` (16 bytes, little-endian,
    offsets 0/4/8/10/12, `NO_HOVER = u32::MAX`), `encode`, `encode_into`
    (leaves the buffer unchanged on error), `decode` (rejects truncated
    buffers, unknown classes, unknown flag bits, overflowing ranges).
  - **Palettes** `ChromaPalette::vesper_dusk()`, `vesper_dawn()`, `by_name`,
    `swatch_for`, `role_for`, `verify_contrast_gates` (the §43.5 integer gate:
    7:1 primary, 4.5:1 comments, against `bg_card`), `verify_oklch_fallbacks`,
    `audit()` (float WCAG ratios, APCA Lc, fallback errors).
    `ThemeSwatch::new` computes `rel_lum_x1000` from the sRGB fallback in code.
  - **Colour science** `color`: OKLCH <-> sRGB (Ottosson's matrices), WCAG 2.x
    relative luminance and contrast ratio, APCA-W3 0.0.98G-4g Lc (constants of
    `apca-w3` 0.1.9; checked against its published values).
  - **Renderers**: `html::render` (semantic `<pre class="vc">`, classes only,
    `aria-hidden` `vc-gutter` line numbers, hover rows as `title`), `ansi`
    (24-bit SGR, `strip`), `json` (schema `vesper.chroma.spans/1`: records,
    hover table, fallback reason, the records' bytes in hex), `css::stylesheet`
    (`vesper-chroma.css`, both themes, generated from the palettes).
  - **wasm32 C ABI** (`src/wasm_abi.rs`, compiled only for `wasm32`; the
    crate is `#![deny(unsafe_code)]` and this module is the one `allow`):
    `chroma_alloc(len) -> ptr`, `highlight(ptr, len) -> record count` (or
    `u32::MAX` with `chroma_error()`), `chroma_output_ptr()`. No imports.
- **`vesper highlight`** (`src/highlight_commands.rs`, routing hunk in
  `src/main.rs`): `[--html|--ansi|--json] [--theme dusk|dawn] [--tier 0|1]
  [--no-line-numbers] [-o PATH] FILE` and `--css`. The Tier 1 adapter reads
  the parser AST and the checker's existing outputs (`World::types`,
  `World::mode`, `World::effects`, `World::functions`) read-only, and runs
  only when the snippet has no syntax and no check diagnostics; otherwise it
  falls back to Tier 0 and reports the first diagnostic (stderr and `--json`).
- **`lean/ChromaHighlightSafety.lean`** (§43.4).
- Tests: `crates/vesper-highlight/tests/chroma.rs` (17), unit tests (5),
  `tests/highlight_cli.rs` (6), `tests/highlight_lean.rs` (3),
  `tests/highlight_wasm.rs` (1); fixtures `tests/fixtures/highlight/`.

## Public API for lane 2 (`vesper shot`) and lane 17 (Lens)

Stable from this PR, additive changes only during the wave:
`VesperTokenClass` (frozen numbering), `HighlightSpan`, `flags::*`,
`ChromaError`, `verify_lossless_spans`, `highlight_tier0`,
`highlight_tier0_tokens`, `highlight_tier1`, `tier0_fallback`, `Highlight`,
`Tier`, `FallbackReason`, `SemanticFacts` and its fact types,
`ChromaPalette`, `ThemeSwatch`, `SwatchRole`, `ContrastAudit`, `color::*`,
`wire::*`, `css::class_name`, `css::stylesheet`, `html::render`,
`html::HtmlOptions`, `ansi::render`, `json::render`.
`CodeCardScene`, `PositionedGlyphRun` and `build_code_card_scene` (§43.5) are
not here: they belong to lane 2's `src/shot/**`. `HighlightSpan` has two
fields beyond §43.5 (`flags`, `hover_sym_id`), so the wire record round-trips.

## Measurements (cloud host: Intel Xeon @ 2.10 GHz, 4 vCPUs, Linux 6.18, Rust 1.100.0)

| Target (design) | Measured | Status |
| --- | --- | --- |
| Tier 0 under 0.2 ms on a 200-line file | CST mode 0.67-1.03 ms; token mode 0.10-0.11 ms (200 lines, 6400 bytes, release, mean of 200 runs) | CST mode misses; token mode meets |
| `vesper-chroma.wasm` 26 KiB | 54,700 bytes (53.4 KiB), stripped, opt-level z, fat LTO, 1 CGU, panic=abort, token mode | Missed |
| Re-highlight under 0.3 ms per keystroke | 62 µs for a 4096-byte buffer after a 1-byte edit (copy in, highlight, read records) under Wasmtime 50 / Cranelift | Meets on this host |
| `vesper-chroma.css` 1.2 KiB | 1,080 bytes (both themes plus the `prefers-color-scheme: light` default) | Meets |

Where the time and bytes go: the batch parser alone takes about 0.56 ms on
the 200-line file (it builds the AST as well as the CST); the lexer takes
16 µs. A wasm module that links the parser is 170,147 bytes; one that calls
only the `vesper-syntax` lexer (no classifier) is 38,832 bytes, so the
26 KiB target cannot be reached while reusing the `vesper-syntax` lexer (its
logos tables and diagnostics). Reaching it needs a smaller dedicated lexer;
not attempted here. Sizes were measured without `wasm-opt`, which is not
installed in the cloud image.

`tests/highlight_wasm.rs` builds the module with the Cargo that runs it when
the `wasm32-unknown-unknown` standard library is installed (or loads
`VESPER_CHROMA_WASM`), asserts it has no imports, compares its records with
the native token mode on five inputs, checks the UTF-8 error code, and
prints the size and timing. Under Bazel (no Cargo) and on toolchains without
the target it prints a skip, so CI does not exercise it; it ran in the cloud
session with the numbers above.

## Contrast audit (WCAG 2.x relative luminance; APCA-W3 Lc; both against `bg_card`)

Vesper Dusk (`bg_card` #111625):

| Role | Colour | WCAG | Gate | APCA Lc |
| --- | --- | --- | --- | --- |
| fg_default | #E2E8F5 | 14.67:1 | 7.0 | -91.76 |
| linear_amber | #FFB454 | 10.22:1 | 7.0 | -69.59 |
| inout_cyan | #5CCFE6 | 9.89:1 | 7.0 | -67.83 |
| effect_iris | #D2A6FF | 9.13:1 | 7.0 | -63.42 |
| wire_emerald | #6EE7B7 | 11.82:1 | 7.0 | -78.07 |
| keyword_rose | #FF8F9C | 8.29:1 | 7.0 | -58.81 |
| comment_slate | #8B97B3 | 6.16:1 | 4.5 | -45.11 |

Vesper Dawn (`bg_card` #FAF8F5):

| Role | Colour | WCAG | Gate | APCA Lc |
| --- | --- | --- | --- | --- |
| fg_default | #1B2030 | 15.29:1 | 7.0 | 99.09 |
| linear_amber | #8B3C00 | 7.22:1 | 7.0 | 81.84 |
| inout_cyan | #015C6F | 7.17:1 | 7.0 | 81.85 |
| effect_iris | #6627D0 | 7.16:1 | 7.0 | 81.05 |
| wire_emerald | #016137 | 7.16:1 | 7.0 | 81.77 |
| keyword_rose | #A71132 | 7.15:1 | 7.0 | 80.31 |
| comment_slate | #55607A | 5.93:1 | 4.5 | 77.24 |

The test gate for APCA is |Lc| >= 45 on every token colour. Dusk's accents
reach Lc 59-78, below APCA's body-text level of 75 (Dusk keyword rose is
below 60), although they clear WCAG AAA comfortably; this is recorded, not
fixed, because the hex values are the design's.

### Design mismatches (reported, as the assignment asked)

1. **§43.5's Dusk OKLCH numbers do not reproduce its hex values.** All nine
   differ by more than one sRGB step, and two are out of gamut, e.g.
   `oklch(0.835 0.158 72)` is about rgb(263, 182, 63), not #FFB454. The palette
   keeps the design's hex values (the website CSS uses them) and stores the
   OKLCH coordinates of those colours, each of which round-trips within one
   step (`verify_oklch_fallbacks`). The test
   `design_dusk_oklch_values_are_reported_against_their_hex` prints each
   design value, its computed colour and the step error.
2. **§43.5's hand-copied luminance values differ from the computed ones**
   (fg 790 listed vs 805 computed, linear 552 vs 545, inout 548 vs 526, effect
   472 vs 482, wire 660 vs 639, keyword 435 vs 433, comment 305 vs 309,
   bg_card 16 vs 8, bg_canvas 9 vs 4). The code computes them.
3. **§43.3's Vesper Dawn accents fail the 7:1 gate the design asks for**:
   #B85200 4.67:1, #006B80 5.80:1, #6B2FD6 6.60:1, #087443 5.52:1, #B4233C
   6.10:1 on #FAF8F5. Each was darkened in OKLCH, keeping its hue family and
   reducing chroma only to stay in gamut, until it clears 7:1:
   #8B3C00, #015C6F, #6627D0, #016137, #A71132. Dawn's background,
   foreground and comment colours are the design's. The design gives no Dawn
   canvas colour; #F2EFEA was chosen.

## Lean (§43.4)

`lean/ChromaHighlightSafety.lean` compiles with Lean 4.24.0 (release zip),
has no `sorry`, `admit`, axioms or `native_decide`, and is kernel-checked by
`tests/highlight_lean.rs` when `VESPER_LEAN` is set (on a regular copy, since
Lean rejects symlinked runfiles). It proves, about its own models:

- the two theorems of §43.4 as written;
- for `verify` (the model of `verify_lossless_spans` over a list of
  `(start, end)` pairs): `verify_sound` / `verify_complete` against the
  `Contiguous` relation, `chroma_spans_lossless` (accepted spans' lengths sum
  to the source length) and `chroma_verify_reconstructs` (for any source
  list, concatenating the accepted slices gives the source back);
- for `gate` (the model of the Rust integer test
  `den * (max + 50) >= num * (min + 50)`): symmetry, soundness, the AAA and
  4.5:1 forms, `verifyGates_sound` for the model of
  `verify_contrast_gates`, and by `decide` that the shipped palettes' computed
  luminance thousandths pass and that the design's Dawn accents fail.
  `tests/highlight_lean.rs` checks those literals against the Rust palettes.

These are theorems about the Lean models, not about the Rust code, the
browser's copy behaviour or perceived contrast.

## Checks run

- `cargo +1.100.0 fmt --all -- --check`: clean.
- `cargo +1.100.0 clippy --locked -p vesper-highlight -p vesper --all-targets -- -D warnings`: clean.
- `cargo +1.100.0 clippy --locked -p vesper-highlight --target wasm32-unknown-unknown -- -D warnings`: clean.
- `cargo +1.100.0 test -p vesper-highlight` (debug and release): 22 passed.
- `cargo +1.100.0 test --test highlight_cli --test highlight_lean --test highlight_wasm` with
  `VESPER_LEAN` set: 10 passed. `highlight_cli` round-trips all 348 `.ves`
  files in the repository and 13,000+ truncated prefixes in CST mode, and
  checks token mode's boundaries on every file.
- `lean lean/ChromaHighlightSafety.lean` (4.24.0): no output, exit 0.
- `vesper fmt --check tests/fixtures/highlight/`: canonical.
- `python3 tools/bazel/sync_build_files.py`: generated
  `crates/vesper-highlight/BUILD.bazel` (default `vesper_crate()`; the crate
  tests read no files outside the crate).
- The full workspace suite runs in CI (Bazel on Linux and macOS, Cargo on
  Windows).

## Root patch (own commit)

`Cargo.toml`: `crates/vesper-highlight` in `members` and `default-members`
(before `experiments/native-pulse`), and `vesper-highlight` as a root
dependency for `vesper highlight`. `Cargo.lock`, regenerated by Cargo: `smol_str 0.3.7` (pinned
`=0.3.7`, default features; the only new external crate used) and the lock
entry for its optional `borsh` dependency, which is not built
(`cargo tree -i borsh` is empty). `crates/vesper-highlight/BUILD.bazel` from
the sync script.

## Decisions

- Plain value identifiers have no class of their own in §43.5; they are
  `PassModeIn` ("neutral slate for copy/in bindings", §43.1), drawn with the
  default foreground.
- At Tier 0 an effect named in a row but not declared in the snippet is
  `EffectAbortable` (the conservative reading of "uncancellable (linear
  effect) versus abortable (effect)"); Tier 1 replaces it with the checked
  World's answer.
- Tier 1 runs only when the snippet has zero syntax and check diagnostics.
  Its facts come from the AST and existing checker outputs. Bindings follow
  lexical scope (blocks, match arms, closures, handler clauses), patterns
  bind their components with the types the checker's `bind_mode` gives them
  (tuples by position, constructor fields with ADT generics substituted),
  and uses resolve to the innermost visible binding. Handler clause
  parameters have no recorded type, so they only shadow. A linear binding's
  discharge is its last use in the function.
- Mutated leaves: a field path is mutated when it is an assignment target,
  an explicit `inout`/`sink` argument, or the receiver of a method whose
  first parameter is not `in`, resolved as `Type::method` from the
  receiver's recorded type (an unresolved method counts as a read).
- Tier 0 is linear-time on incomplete input: effect rows are tracked with a
  bracket stack in one pass, and at most 64 distinct effect names are
  tracked per snippet (later ones stay `TypeNominal`). Other field reads are `FieldLeafRead`.
- Rust ownership in this crate is not evidence that the Vesper source typing
  rules are implemented; the linear facts are what the checker reported.

## Remaining gaps

- wasm module size (53.4 KiB vs 26 KiB) and CST-mode latency (about 0.7 ms vs
  0.2 ms) miss the design targets; token mode meets the latency target.
- Hover cards for FieldTree disjointness and `eff [...]` rows in the wasm
  module (§43.1 item 4) are not built: the module exposes Tier 0 records only;
  hover tables come from the CLI's Tier 1.
- Twoslash `//^?` / `//^!` callouts and `@shot` are lane 2's.
- The dark-mode CSS uses the sRGB fallbacks only; no `oklch()` values are
  emitted (they would not change the rendered colour and cost bytes).
