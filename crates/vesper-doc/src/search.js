"use strict";
// Port of vesper_doc::hoogle::{parse_text_query, search}. Keep in step with hoogle.rs.
const MODES = { copy: "Copy", affine: "Affine", linear: "Linear" };
const RANK = { Copy: 0, Affine: 1, Linear: 2 };
const PASSES = { in: "In", inout: "Inout", sink: "Sink" };
function tokens(text) {
  const out = [];
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (/\s/.test(c)) { i++; continue; }
    if (c === "-" && text[i + 1] === ">") { out.push({ arrow: true }); i += 2; }
    else if ("[](),.{}".includes(c)) { out.push({ sym: c }); i++; }
    else if (/[A-Za-z0-9_]/.test(c)) {
      let j = i;
      while (j < text.length && /[A-Za-z0-9_]/.test(text[j])) j++;
      if (j - i > 128) throw new Error("query identifier exceeds 128 bytes");
      out.push({ word: text.slice(i, j) });
      i = j;
    } else throw new Error("unexpected character '" + c + "' in query");
    if (out.length > 512) throw new Error("query token limit exceeded");
  }
  return out;
}
function parseQuery(text) {
  if (new TextEncoder().encode(text).length > 4096) throw new Error("query exceeds 4 KiB");
  const toks = tokens(text);
  if (toks.length === 0) throw new Error("empty query");
  if (!toks.some((t) => t.arrow)) {
    if (toks.length === 1 && toks[0].word !== undefined) return { name: toks[0].word };
    throw new Error("a signature query needs '->'; a name query is one word");
  }
  let at = 0;
  const peek = () => toks[at];
  const isSym = (t, c) => t !== undefined && t.sym === c;
  const eatSym = (c) => (isSym(peek(), c) ? (at++, true) : false);
  const expectSym = (c) => { if (!eatSym(c)) throw new Error("expected '" + c + "' in query"); };
  const word = () => (peek() && peek().word !== undefined ? toks[at++].word : null);
  const peekWord = (ws) => (peek() && ws.includes(peek().word) ? peek().word : null);
  const mode = () => {
    const w = peekWord(["copy", "affine", "linear"]);
    if (w === null) return null;
    const n = toks[at + 1];
    if (n !== undefined && (n.word !== undefined || n.sym === "(")) { at++; return MODES[w]; }
    return null;
  };
  const ty = (depth) => {
    if (depth > 16) throw new Error("query type nesting limit");
    if (eatSym("(")) {
      const items = [];
      if (!eatSym(")")) {
        for (;;) { items.push(ty(depth + 1)); if (eatSym(")")) break; expectSym(","); }
      }
      return { tuple: { items } };
    }
    const name = word();
    if (name === null) throw new Error("expected a type in query");
    if (name === "_") return "any";
    if (name === "Unit") return { tuple: { items: [] } };
    if (!/^[A-Za-z]/.test(name)) throw new Error("invalid type name '" + name + "' in query");
    const args = [];
    if (eatSym("[")) {
      for (;;) { args.push(ty(depth + 1)); if (eatSym("]")) break; expectSym(","); }
    }
    return { named: { name, args } };
  };
  const selector = () => {
    if (!(isSym(peek(), ".") && isSym(toks[at + 1], "{"))) return null;
    at += 2;
    const paths = [];
    for (;;) {
      const first = word();
      if (first === null) throw new Error("expected a field name in projection");
      const path = [first];
      while (eatSym(".")) {
        const w = word();
        if (w === null) throw new Error("expected a field name after '.'");
        path.push(w);
      }
      paths.push(path);
      if (paths.length > 64) throw new Error("query projection limit");
      if (eatSym("}")) break;
      expectSym(",");
    }
    return paths;
  };
  const param = () => {
    const p = peekWord(["in", "inout", "sink"]);
    let pass = null;
    if (p !== null) { at++; pass = PASSES[p]; }
    const m = mode();
    const t = ty(0);
    return { pass, mode: m, ty: t, selector: selector() };
  };
  const params = [];
  if (isSym(toks[0], "(") && isSym(toks[1], ")") && toks[2] && toks[2].arrow) at = 2;
  else if (!(peek() && peek().arrow)) {
    for (;;) {
      params.push(param());
      if (params.length > 64) throw new Error("query parameter limit");
      if (!eatSym(",")) break;
    }
  }
  if (!(peek() && peek().arrow)) throw new Error("expected '->' after the query parameters");
  at++;
  const resultMode = mode();
  const result = ty(0);
  let ceiling = null;
  if (peekWord(["eff"]) !== null) {
    at++;
    expectSym("[");
    const effects = [];
    if (!eatSym("]")) {
      for (;;) {
        const t = ty(0);
        if (t === "any" || t.named === undefined) throw new Error("an effect must be named");
        effects.push(t);
        if (eatSym("]")) break;
        expectSym(",");
      }
    }
    const texts = effects.map((e) => {
      const s = patternText(e);
      if (s === null) throw new Error("effect arguments cannot be wildcards");
      return s;
    });
    ceiling = [...new Set(texts)].sort(cmp);
  }
  if (at !== toks.length) throw new Error("unexpected trailing query text");
  return { signature: { params, result, resultMode, ceiling } };
}
function cmp(a, b) { return a < b ? -1 : a > b ? 1 : 0; }
function patternText(p) {
  if (p === "any") return null;
  if (p.named) {
    if (p.named.args.length === 0) return p.named.name;
    const a = p.named.args.map(patternText);
    return a.includes(null) ? null : p.named.name + "[" + a.join(", ") + "]";
  }
  const a = p.tuple.items.map(patternText);
  return a.includes(null) ? null : "(" + a.join(", ") + ")";
}
function patternDisplay(p) {
  if (p === "any") return "_";
  if (p.named) {
    if (p.named.args.length === 0) return p.named.name;
    return p.named.name + "[" + p.named.args.map(patternDisplay).join(", ") + "]";
  }
  return "(" + p.tuple.items.map(patternDisplay).join(", ") + ")";
}
// null: no match; true: matched without a wildcard or type parameter.
// binds: [[name, pattern]] in first-binding order.
function typeMatch(p, t, binds) {
  if (p === "any") return false;
  if (t.var !== undefined) {
    const b = binds.find((x) => x[0] === t.var.name);
    if (b === undefined) binds.push([t.var.name, p]);
    else if (JSON.stringify(b[1]) !== JSON.stringify(p)) return null;
    return false;
  }
  let ps, ts;
  if (p.named && t.named && p.named.name === t.named.name && p.named.args.length === t.named.args.length) {
    ps = p.named.args; ts = t.named.args;
  } else if (p.tuple && t.tuple && p.tuple.items.length === t.tuple.items.length) {
    ps = p.tuple.items; ts = t.tuple.items;
  } else return null;
  let exact = true;
  for (let i = 0; i < ps.length; i++) {
    const m = typeMatch(ps[i], ts[i], binds);
    if (m === null) return null;
    exact = exact && m;
  }
  return exact;
}
function startsWith(p, q) { return q.length <= p.length && q.every((x, i) => p[i] === x); }
function within(inner, outer) {
  if (outer.length === 0) return true;
  return inner.length > 0 && inner.every((p) => outer.some((q) => startsWith(p, q)));
}
function sameSelector(a, b) { return JSON.stringify(a) === JSON.stringify(b); }
function search(query, index) {
  const hits = [];
  for (const e of index.entries) {
    let exact;
    const binds = [];
    if (query.name !== undefined) {
      if (!e.path.toLowerCase().includes(query.name.toLowerCase())) continue;
      exact = e.name === query.name;
    } else {
      const q = query.signature;
      if (q.params.length !== e.params.length) continue;
      let ok = true;
      exact = true;
      for (let i = 0; i < q.params.length; i++) {
        const p = q.params[i], f = e.params[i];
        const m = typeMatch(p.ty, f.ty, binds);
        if (m === null || (p.pass !== null && p.pass !== f.pass) ||
            (p.mode !== null && RANK[p.mode] > RANK[f.mode]) ||
            (p.selector !== null && !within(f.selector, p.selector))) { ok = false; break; }
        exact = exact && m && p.pass !== null && p.mode === f.mode &&
          sameSelector(p.selector === null ? [] : p.selector, f.selector);
      }
      if (!ok) continue;
      const r = typeMatch(q.result, e.result, binds);
      if (r === null) continue;
      exact = exact && r;
      if (q.resultMode !== null) {
        if (RANK[e.result_mode] > RANK[q.resultMode]) continue;
        exact = exact && e.result_mode === q.resultMode;
      } else exact = false;
      if (q.ceiling !== null) {
        if (!e.effects.every((x) => q.ceiling.includes(x))) continue;
        exact = exact && sameSelector(e.effects, q.ceiling);
      } else exact = false;
      exact = exact && e.generics.length === 0;
    }
    const score = (exact ? 10 : 0) + (e.badge === "proved" ? 5 : e.badge === "audited" ? 2 : 0);
    const instantiation = [];
    for (const g of e.generics) {
      const b = binds.find((x) => x[0] === g.name);
      if (b !== undefined) instantiation.push([b[0], patternDisplay(b[1])]);
    }
    hits.push({ name: e.name, path: e.path, kind: e.kind, href: e.href, signature: e.signature,
      score, exact, badge: e.badge, instantiation });
  }
  hits.sort((a, b) => b.score - a.score || cmp(a.path, b.path));
  return hits;
}
if (typeof module !== "undefined") module.exports = { parseQuery, search };
if (typeof document !== "undefined") {
  const index = JSON.parse(document.getElementById("codex-index").textContent);
  const input = document.getElementById("q");
  const list = document.getElementById("hits");
  const err = document.getElementById("err");
  const run = () => {
    list.replaceChildren();
    err.textContent = "";
    if (input.value.trim() === "") return;
    let hits;
    try { hits = search(parseQuery(input.value), index); } catch (e) { err.textContent = e.message; return; }
    if (hits.length === 0) err.textContent = "No documented function matches.";
    for (const h of hits) {
      const li = document.createElement("li");
      const a = document.createElement("a");
      a.href = h.href !== null ? h.href : CODEX_HREF + "#item-" + encodeURIComponent(h.name);
      const code = document.createElement("code");
      code.textContent = h.signature;
      a.append(code);
      li.append(a);
      if (h.badge !== "unverified") {
        const b = document.createElement("span");
        b.className = "b-" + h.badge;
        b.textContent = " " + h.badge;
        li.append(b);
      }
      if (h.exact) li.append(" (exact)");
      if (h.instantiation.length > 0) {
        li.append(" with " + h.instantiation.map((x) => x[0] + " = " + x[1]).join(", "));
      }
      list.append(li);
    }
  };
  input.addEventListener("input", run);
  const initial = new URLSearchParams(location.search).get("q");
  if (initial) { input.value = initial; }
  run();
}
