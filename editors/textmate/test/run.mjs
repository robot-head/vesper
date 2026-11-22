// Checks editors/textmate/vesper.tmLanguage.json against the Vesper lexer.
//
// Every tracked `.ves` file is tokenized twice: once by a port of the token
// rules in crates/vesper-syntax/src/lexer.rs (logos: longest match, keywords
// win ties), and once by the TextMate grammar through vscode-textmate. For
// every byte the lexer puts in a comment, string, number or hard keyword
// token, the grammar must give a matching scope, and the grammar must not
// call any other byte a comment, string or number. A grammar rule that
// leaks (an unterminated string or comment carried across lines) shows up as
// mismatches on the lines after it.
//
//   cd editors/textmate/test && npm ci && npm test
//
// `npm test -- path.ves ...` checks only the given files and prints each
// mismatch.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const vsctm = require("vscode-textmate");
const oniguruma = require("vscode-oniguruma");

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../../..");
const grammarPath = join(here, "..", "vesper.tmLanguage.json");

// ---- reference lexer (crates/vesper-syntax/src/lexer.rs) ----

const KEYWORDS = new Map(
  Object.entries({
    struct: "decl", enum: "decl", effect: "decl", impl: "decl", fn: "decl",
    let: "decl", eff: "contextual", mod: "modal", copy: "modal", affine: "modal",
    linear: "modal", in: "mode", inout: "mode", sink: "mode", self: "self",
    handle: "control", state: "contextual", with: "control", return: "control",
    match: "control", if: "control", else: "control", true: "bool", false: "bool",
  }),
);
const RULES = [
  ["ws", /[ \t\r\n\x0C]+/y],
  ["comment", /\/\/[^\n]*/y],
  ["upper", /[A-Z][A-Za-z0-9_]*/y],
  ["lower", /[a-z_][A-Za-z0-9_]*/y],
  ["number", /[0-9]+\.[0-9]+(?:[eE][+-]?[0-9]+)?|[0-9]+[eE][+-]?[0-9]+/y],
  ["number", /0[xX][0-9a-fA-F_]+|[0-9][0-9_]*/y],
  ["string", /"(?:[^"\\]|\\[\s\S])*"/y],
];

function lex(src) {
  const toks = [];
  let i = 0;
  while (i < src.length) {
    if (src.startsWith("/*", i)) {
      let depth = 1;
      let j = i + 2;
      while (j + 1 < src.length && depth > 0) {
        if (src.startsWith("/*", j)) { depth++; j += 2; }
        else if (src.startsWith("*/", j)) { depth--; j += 2; }
        else j++;
      }
      if (depth > 0) j = src.length;
      toks.push({ kind: "comment", start: i, end: j });
      i = j;
      continue;
    }
    let best = null;
    for (const [kind, re] of RULES) {
      re.lastIndex = i;
      const m = re.exec(src);
      if (m && (!best || m[0].length > best.len)) best = { kind, len: m[0].length };
    }
    if (!best) {
      toks.push({ kind: "punct", start: i, end: i + 1 });
      i++;
      continue;
    }
    let kind = best.kind;
    const text = src.slice(i, i + best.len);
    if (kind === "lower" && KEYWORDS.has(text)) kind = "kw:" + KEYWORDS.get(text);
    toks.push({ kind, start: i, end: i + best.len });
    i += best.len;
  }
  return toks;
}

// ---- what each lexer class must map to in the grammar ----

const want = {
  comment: (s) => s.some((x) => x.startsWith("comment.")),
  string: (s) => s.some((x) => x.startsWith("string.")),
  number: (s) => s.some((x) => x.startsWith("constant.numeric.")),
  "kw:decl": (s) => s.some((x) => x.startsWith("storage.type")),
  "kw:modal": (s) => s.some((x) => x.startsWith("storage.modifier")),
  "kw:mode": (s) => s.some((x) => x.startsWith("storage.modifier")),
  "kw:self": (s) => s.some((x) => x.startsWith("variable.language.self")),
  "kw:control": (s) => s.some((x) => x.startsWith("keyword.control")),
  "kw:bool": (s) => s.some((x) => x.startsWith("constant.language.boolean")),
};
const literalScope = (s) =>
  s.find((x) => /^(comment|string|constant\.numeric)\./.test(x));

// ---- grammar ----

const wasm = readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm")).buffer;
await oniguruma.loadWASM(wasm);
const registry = new vsctm.Registry({
  onigLib: Promise.resolve({
    createOnigScanner: (p) => new oniguruma.OnigScanner(p),
    createOnigString: (s) => new oniguruma.OnigString(s),
  }),
  loadGrammar: async () =>
    vsctm.parseRawGrammar(readFileSync(grammarPath, "utf8"), grammarPath),
});
const grammar = await registry.loadGrammar("source.vesper");

// Scopes for every UTF-16 offset of the file, tokenized line by line.
function scopes(src) {
  const out = new Array(src.length);
  let state = vsctm.INITIAL;
  let base = 0;
  for (const line of src.split("\n")) {
    const r = grammar.tokenizeLine(line, state);
    for (const t of r.tokens) {
      for (let k = t.startIndex; k < Math.min(t.endIndex, line.length); k++) out[base + k] = t.scopes;
    }
    if (base + line.length < src.length) out[base + line.length] = ["source.vesper"];
    state = r.ruleStack;
    base += line.length + 1;
  }
  return out;
}

// ---- run ----

const args = process.argv.slice(2);
const files = args.length
  ? args
  : execFileSync("git", ["ls-files", "*.ves"], { cwd: root, encoding: "utf8" })
      .split("\n")
      .filter(Boolean);
const verbose = args.length > 0;

let checked = 0;
let bad = 0;
const badFiles = [];
const t0 = Date.now();
for (const rel of files) {
  const src = readFileSync(resolve(root, rel), "utf8");
  const sc = scopes(src);
  const problems = [];
  const lineOf = (i) => src.slice(0, i).split("\n").length;
  for (const tok of lex(src)) {
    checked++;
    const s = sc[tok.start] ?? [];
    const text = src.slice(tok.start, tok.end);
    const check = want[tok.kind];
    if (check) {
      // Every byte of the token (a comment or string can span lines; newlines carry no scope).
      for (let p = tok.start; p < tok.end; p++) {
        if (src[p] === "\n" || src[p] === "\r") continue;
        if (!check(sc[p] ?? [])) {
          problems.push(`${rel}:${lineOf(p)}: ${tok.kind} ${JSON.stringify(text.slice(0, 40))} got ${(sc[p] ?? []).slice(1).join(" ") || "(none)"}`);
          break;
        }
      }
    } else if (tok.kind !== "ws") {
      const lit = literalScope(s);
      if (lit) problems.push(`${rel}:${lineOf(tok.start)}: ${tok.kind} ${JSON.stringify(text.slice(0, 40))} got ${lit}`);
    }
  }
  if (problems.length) {
    bad += problems.length;
    badFiles.push([rel, problems.length]);
    if (verbose) for (const p of problems) console.log(p);
    else console.log(problems[0] + (problems.length > 1 ? ` (+${problems.length - 1} more)` : ""));
  }
}
console.log(
  `${files.length} files, ${checked} tokens, ${bad} mismatches in ${badFiles.length} files (${Date.now() - t0} ms)`,
);
process.exit(bad ? 1 : 0);
