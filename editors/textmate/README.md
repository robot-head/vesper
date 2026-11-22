# Vesper TextMate grammar

`vesper.tmLanguage.json` (scope `source.vesper`, extension `.ves`) highlights
Vesper source in any editor or tool that reads TextMate grammars: VS Code,
Sublime Text, Zed and Helix through their TextMate importers, Shiki, and
GitHub Linguist.

The rules follow the lexer in `crates/vesper-syntax/src/lexer.rs` and the
Tier 0 classes of `crates/vesper-highlight/src/tier0.rs`:

- hard keywords, and the contextual words (`for`, `loop`, `break`,
  `continue`, `let mut`, `pub`, `use`, `trait`, `type`, `where`, `region`,
  `state`, ...) only where the parser reads them as keywords;
- modal specifiers (`copy`, `affine`, `linear`, `mod`) and pass modes
  (`in`, `inout`, `sink`), including `@copy` and `@@linear` annotations;
- effect rows (`eff [Fail[E]]`), `handle ... with` handlers;
- primitive scalars up to `u128`/`i128`, wire scalars (`u16be`, `i64le`);
- integer, hex and float literals, strings with escapes, nested block
  comments, `// @case` / `// @export` directives;
- `@test`, `@test_case(...)`, `@fip`, `@proof` attributes, `@scope/package`
  paths, macro calls (`m!(...)`) and `$x` metavariables.

## Test

```sh
cd editors/textmate/test
npm ci
npm test              # every tracked .ves file
npm test -- a.ves     # one file, printing every mismatch
```

The test tokenizes every tracked `.ves` file with the grammar (through
vscode-textmate) and with a port of the lexer's token rules, and requires
every comment, string, number and hard keyword token to get the matching
scope, and nothing else to be scoped as a comment, string or number.

## GitHub

GitHub highlights `.ves` with Rust's grammar for now, through
`*.ves linguist-language=Rust` in the root `.gitattributes`. Linguist only
adds a new language once its extension is in wide use on GitHub (about 2,000
`.ves` files indexed in the last year, spread over many users, with the
language's own author's repositories filtered out), and it vendors grammars
from a repository under an approved open source license. Until then this
grammar is for editors.
