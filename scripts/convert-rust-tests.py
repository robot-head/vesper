#!/usr/bin/env python3
"""Rust tests to Vesper tests (Rust-exit wave SH6 lane 19, #1414).

Two commands:

  classify [--markdown]
      Sorts every Rust test file (tests/*.rs and crates/*/tests/**/*.rs) by
      shape and counts what is left to convert:
        program  - embeds Vesper sources and checks them through the compiler
                   (parse, check, evaluator, JIT, objects): each case becomes
                   a .ves file run by `vesper test --engines all`;
        cli      - runs the `vesper` binary and compares its output or exit
                   status: each case becomes a .ves file plus the command;
        rust-api - calls a Rust API (a crate's functions or the compiler's
                   internal data structures): converted by the flip lane of
                   that crate against its Vesper port, or kept as a
                   differential against the last Rust commit.

  convert TESTS_RS --out DIR [--vesper BIN] [--write]
      Converts the cases of one Rust test file whose helpers have a known
      shape (see HELPER SHAPES below) into Vesper tests under DIR, one .ves
      file per case. Every generated file is run with
      `vesper test --engines all` and kept only if it passes; the rest are
      listed with the reason. Without --write it only reports.

HELPER SHAPES. A converted case is a call, with literal arguments, of a
helper defined in the same file whose signature and body match one of:

  value    fn H(name: &str, source: &str, expected: <int>)
           `assert_eq!(H(name, source), <int>)` for fn H(name, source) -> <int>
           The body may only run `main` on the evaluator and the JIT
           (optimized or not), check JIT teardown and box counts, compile
           objects and run an executable: exactly what `--engines all`
           checks. The case becomes the source plus
               test { @test fn <name>() -> bool { main() == <expected> } }
  error    fn H(source: &str, code: &str, <message>: &str)
           fn H(source: &str, code: &str)
           The body checks that a diagnostic has the code (and that its
           message contains the text). The function holding the diagnostic
           moves into the test block as
               @test(expect_error = <code>, message = "<text>")

The converter reads helper bodies for the engines they call and refuses a
helper that checks something `--engines all` does not (another backend, the
IR, a Rust data structure).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------------------
# A small Rust tokenizer: enough to find calls and decode literal arguments.


@dataclass
class Tok:
    kind: str  # ident, str, int, punct, char, lifetime
    text: str
    start: int
    end: int
    value: object = None


def _decode_escapes(body: str) -> str:
    out = []
    i = 0
    while i < len(body):
        c = body[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        n = body[i + 1]
        if n == "\n":
            i += 2
            while i < len(body) and body[i] in " \t\r\n":
                i += 1
            continue
        simple = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
        if n in simple:
            out.append(simple[n])
            i += 2
        elif n == "x":
            out.append(chr(int(body[i + 2 : i + 4], 16)))
            i += 4
        elif n == "u":
            close = body.index("}", i)
            out.append(chr(int(body[i + 3 : close], 16)))
            i = close + 1
        else:
            # Not a Rust escape (the literal would not compile); keep it.
            out.append(n)
            i += 2
    return "".join(out)


def tokenize(src: str) -> list[Tok]:
    toks: list[Tok] = []
    i = 0
    n = len(src)
    while i < n:
        c = src[i]
        if c.isspace():
            i += 1
        elif src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
        elif src.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif src.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
        elif c in "rbc" and re.match(r'(b|c)?r#*"', src[i:]):
            m = re.match(r'(?:b|c)?r(#*)"', src[i:])
            hashes = m.group(1)
            kind = "str" if c == "r" else "bytes"
            close = '"' + hashes
            j = src.index(close, i + len(m.group(0)))
            toks.append(Tok(kind, src[i : j + len(close)], i, j + len(close), src[i + len(m.group(0)) : j]))
            i = j + len(close)
        elif c == "b" and i + 1 < n and src[i + 1] in "\"'":
            # Byte strings and byte chars: kept opaque.
            q = src[i + 1]
            j = i + 2
            while src[j] != q:
                j += 2 if src[j] == "\\" else 1
            toks.append(Tok("bytes", src[i : j + 1], i, j + 1))
            i = j + 1
        elif c == '"':
            j = i + 1
            while src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            toks.append(Tok("str", src[i : j + 1], i, j + 1, _decode_escapes(src[i + 1 : j])))
            i = j + 1
        elif c == "'":
            m = re.match(r"'(\\.[^']*|[^\\'])'", src[i:])
            if m:
                toks.append(Tok("char", m.group(0), i, i + len(m.group(0))))
                i += len(m.group(0))
            else:
                m = re.match(r"'[A-Za-z_][A-Za-z0-9_]*", src[i:]) or re.match(r"'(\\u\{[0-9A-Fa-f]+\}|.)'?", src[i:], re.S)
                toks.append(Tok("lifetime", m.group(0), i, i + len(m.group(0))))
                i += len(m.group(0))
        elif c.isalpha() or c == "_":
            m = re.match(r"[A-Za-z_][A-Za-z0-9_]*", src[i:])
            toks.append(Tok("ident", m.group(0), i, i + len(m.group(0))))
            i += len(m.group(0))
        elif c.isdigit():
            m = re.match(r"0x[0-9A-Fa-f_]+|0b[01_]+|0o[0-7_]+|[0-9][0-9_]*(\.[0-9][0-9_]*)?([eE][+-]?[0-9]+)?", src[i:])
            text = m.group(0)
            j = i + len(text)
            s = re.match(r"(u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64)", src[j:])
            suffix = s.group(0) if s else ""
            value = None
            if "." not in text and "e" not in text.lower().replace("0x", ""):
                digits = text.replace("_", "")
                value = int(digits, 0) if digits[:2] in ("0x", "0b", "0o") else int(digits)
            toks.append(Tok("int" if value is not None else "float", text + suffix, i, j + len(suffix), value))
            i = j + len(suffix)
        else:
            two = src[i : i + 2]
            if two in ("::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", ".."):
                toks.append(Tok("punct", two, i, i + 2))
                i += 2
            else:
                toks.append(Tok("punct", c, i, i + 1))
                i += 1
    return toks


def matching(toks: list[Tok], i: int) -> int:
    """Index of the bracket closing toks[i]."""
    pairs = {"(": ")", "[": "]", "{": "}"}
    want = pairs[toks[i].text]
    depth = 0
    for j in range(i, len(toks)):
        t = toks[j].text
        if toks[j].kind != "punct":
            continue
        if t in pairs:
            depth += 1
        elif t in pairs.values():
            depth -= 1
            if depth == 0:
                assert t == want, (toks[i], toks[j])
                return j
    raise ValueError("unbalanced")


def split_args(toks: list[Tok], open_i: int, close_i: int) -> list[list[Tok]]:
    args, cur, depth = [], [], 0
    for t in toks[open_i + 1 : close_i]:
        if t.kind == "punct" and t.text in "([{":
            depth += 1
        elif t.kind == "punct" and t.text in ")]}":
            depth -= 1
        if depth == 0 and t.kind == "punct" and t.text == ",":
            args.append(cur)
            cur = []
        else:
            cur.append(t)
    if cur:
        args.append(cur)
    return args


def literal(arg: list[Tok], bindings: dict[str, object] | None = None):
    """The value of a literal argument (or of a `let` bound to one in the
    same test): a str, an int, or None."""
    if len(arg) == 1 and arg[0].kind == "ident" and bindings and arg[0].text in bindings:
        return bindings[arg[0].text]
    if len(arg) == 1 and arg[0].kind == "str":
        return arg[0].value
    if len(arg) == 1 and arg[0].kind == "int":
        return arg[0].value
    if len(arg) == 2 and arg[0].text == "-" and arg[1].kind == "int":
        return -arg[1].value
    if len(arg) == 2 and arg[0].text == "&" and arg[1].kind == "str":
        return arg[1].value
    return None


# ---------------------------------------------------------------------------
# Items of a Rust test file.


@dataclass
class RustFn:
    name: str
    params: list[tuple[str, str]]
    ret: str
    body: tuple[int, int]  # token indices of `{` and `}`
    is_test: bool
    start: int  # token index of the first attribute or `fn`


def rust_fns(toks: list[Tok]) -> list[RustFn]:
    fns = []
    i = 0
    while i < len(toks):
        t = toks[i]
        if t.kind == "ident" and t.text == "fn" and i + 2 < len(toks) and toks[i + 1].kind == "ident":
            name = toks[i + 1].text
            j = i + 2
            if toks[j].text == "<":
                depth = 0
                while True:
                    if toks[j].text == "<":
                        depth += 1
                    elif toks[j].text == ">":
                        depth -= 1
                        if depth == 0:
                            break
                    j += 1
                j += 1
            if toks[j].text != "(":
                i += 1
                continue
            close = matching(toks, j)
            params = []
            for a in split_args(toks, j, close):
                text = " ".join(x.text for x in a)
                if ":" in [x.text for x in a]:
                    k = [x.text for x in a].index(":")
                    params.append((" ".join(x.text for x in a[:k]), " ".join(x.text for x in a[k + 1 :])))
                else:
                    params.append((text, ""))
            k = close + 1
            ret = []
            while k < len(toks) and toks[k].text not in ("{", ";"):
                ret.append(toks[k].text)
                k += 1
            if k < len(toks) and toks[k].text == "{":
                end = matching(toks, k)
                # Attributes before `fn`.
                s = i
                is_test = False
                while s >= 2 and toks[s - 1].text == "]":
                    o = s - 1
                    depth = 0
                    while True:
                        if toks[o].text == "]":
                            depth += 1
                        elif toks[o].text == "[":
                            depth -= 1
                            if depth == 0:
                                break
                        o -= 1
                    if toks[o - 1].text != "#":
                        break
                    attr = " ".join(x.text for x in toks[o + 1 : s - 1])
                    if attr == "test":
                        is_test = True
                    s = o - 1
                while s >= 1 and toks[s - 1].text in ("pub", "async", "unsafe", "const"):
                    s -= 1
                fns.append(RustFn(name, params, " ".join(ret[1:]) if ret[:1] == ["->"] else "", (k, end), is_test, s))
                i = k + 1
                continue
        i += 1
    return fns


INT_TYPES = {"u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "usize", "isize", "i128", "u128"}

# A value helper may reach only these compiler modules: the engines
# `vesper test --engines all` runs.
PROGRAM_PATHS = {"check", "parser", "runtime", "native", "native_runtime", "seq", "fip", "Runtime"}


def vesper_modules(src: str) -> set[str]:
    """The compiler modules a Rust test file names (`vesper::m`, `use vesper::{m, ..}`)."""
    mods = set(re.findall(r"\bvesper::([a-z_]+)", src))
    for block in re.findall(r"use vesper::\{([^;]*)\};", src, flags=re.S):
        mods |= set(re.findall(r"(?:^|[\s,{])([a-z_]+)(?=::|[\s,}]|$)", block))
    mods.discard("self")
    return mods


def body_paths(toks: list[Tok], fn: RustFn) -> set[str]:
    """`a` of every `a::b` in the body."""
    lo, hi = fn.body
    out = set()
    for k in range(lo, hi):
        if toks[k].text == "::" and toks[k - 1].kind == "ident":
            out.add(toks[k - 1].text)
    return out


def body_calls(toks: list[Tok], fn: RustFn) -> set[str]:
    lo, hi = fn.body
    return {toks[k].text for k in range(lo, hi) if toks[k].kind == "ident" and toks[k + 1].text == "("}


def body_text(src: str, toks: list[Tok], fn: RustFn) -> str:
    lo, hi = fn.body
    return src[toks[lo].start : toks[hi].end]


@dataclass
class Helper:
    kind: str  # value, value-ret, error
    name: str
    message: bool = False
    reason: str = ""


def helper_shapes(src: str, toks: list[Tok], fns: list[RustFn]) -> dict[str, Helper]:
    by_name = {f.name: f for f in fns if not f.is_test}
    shapes: dict[str, Helper] = {}
    compiler_modules = vesper_modules(src)

    def reach(fn: RustFn, seen: set[str]) -> tuple[set[str], set[str]]:
        paths, calls = body_paths(toks, fn), body_calls(toks, fn)
        for c in list(calls):
            if c in by_name and c not in seen:
                seen.add(c)
                p, cc = reach(by_name[c], seen)
                paths |= p
                calls |= cc
        return paths, calls

    for f in by_name.values():
        types = [t for _, t in f.params]
        names = [n for n, _ in f.params]
        if types[:2] == ["& str", "& str"] and names[:2] == ["name", "source"]:
            paths, calls = reach(f, {f.name})
            extra = {p for p in paths if p in compiler_modules or p.startswith("vesper_")} - PROGRAM_PATHS
            if not {"jit_with_options"} <= calls:
                continue
            if len(f.params) == 3 and types[2] in INT_TYPES and not f.ret:
                kind = "value"
            elif len(f.params) == 2 and f.ret in INT_TYPES:
                kind = "value-ret"
            else:
                continue
            reason = f"also reaches {', '.join(sorted(extra))}" if extra else ""
            shapes[f.name] = Helper(kind, f.name, reason=reason)
        elif types[:2] == ["& str", "& str"] and names[:2] == ["source", "code"] and len(f.params) in (2, 3):
            text = body_text(src, toks, f)
            if "== code" not in text and "code ==" not in text:
                continue
            if len(f.params) == 3:
                if types[2] != "& str" or f".contains({names[2]})" not in text.replace(" ", ""):
                    continue
            shapes[f.name] = Helper("error", f.name, message=len(f.params) == 3)
    return shapes


# ---------------------------------------------------------------------------
# Cases.


@dataclass
class Case:
    test: str  # the #[test] fn
    kind: str  # value or error
    name: str
    source: str
    expected: int | None = None
    code: str | None = None
    message: str | None = None
    tokens: tuple[int, int] = (0, 0)  # token span of the call statement


@dataclass
class Skipped:
    test: str
    reason: str


def cases_of(src: str, toks: list[Tok], fns: list[RustFn], shapes: dict[str, Helper]):
    cases: list[Case] = []
    skipped: list[Skipped] = []
    covered: dict[str, bool] = {}
    for f in fns:
        if not f.is_test:
            continue
        lo, hi = f.body
        k = lo + 1
        found_any = False
        uncovered = False
        error_index = 0
        bindings: dict[str, object] = {}
        while k < hi:
            t = toks[k]
            if (
                t.text == "let"
                and toks[k + 1].kind == "ident"
                and toks[k + 2].text == "="
                and toks[k + 3].kind == "str"
                and toks[k + 4].text == ";"
            ):
                bindings[toks[k + 1].text] = toks[k + 3].value
                k += 5
                continue
            call = None
            if t.kind == "ident" and t.text in shapes and toks[k + 1].text == "(" and toks[k - 1].text not in ("fn", "."):
                call = (k, shapes[t.text])
            if (
                t.kind == "ident"
                and t.text == "assert_eq"
                and toks[k + 1].text == "!"
                and toks[k + 2].text == "("
                and toks[k + 3].text in shapes
                and shapes[toks[k + 3].text].kind == "value-ret"
                and toks[k + 4].text == "("
            ):
                outer_close = matching(toks, k + 2)
                outer = split_args(toks, k + 2, outer_close)
                inner_close = matching(toks, k + 4)
                inner = split_args(toks, k + 4, inner_close)
                helper = shapes[toks[k + 3].text]
                ok = len(outer) >= 2 and len(inner) == 2
                name = literal(inner[0], bindings) if ok else None
                source = literal(inner[1], bindings) if ok else None
                expected = literal(outer[1], bindings) if ok else None
                end = outer_close + 1
                if ok and isinstance(name, str) and isinstance(source, str) and isinstance(expected, int) and not helper.reason:
                    cases.append(Case(f.name, "value", name, source, expected=expected, tokens=(k, end)))
                    found_any = True
                else:
                    uncovered = True
                    why = helper.reason or "arguments are not literals"
                    skipped.append(Skipped(f.name, f"`assert_eq!({helper.name}(..), ..)`: {why}"))
                k = end
                continue
            if call:
                k0, helper = call
                close = matching(toks, k0 + 1)
                args = split_args(toks, k0 + 1, close)
                vals = [literal(a, bindings) for a in args]
                end = close + 1
                if helper.kind == "value" and len(vals) == 3 and isinstance(vals[0], str) and isinstance(vals[1], str) and isinstance(vals[2], int) and not helper.reason:
                    cases.append(Case(f.name, "value", vals[0], vals[1], expected=vals[2], tokens=(k0, end)))
                    found_any = True
                elif helper.kind == "error" and all(isinstance(v, str) for v in vals) and len(vals) == (3 if helper.message else 2):
                    error_index += 1
                    cases.append(
                        Case(
                            f.name,
                            "error",
                            f"{f.name}_{error_index}",
                            vals[0],
                            code=vals[1],
                            message=vals[2] if helper.message and vals[2] else None,
                            tokens=(k0, end),
                        )
                    )
                    found_any = True
                elif helper.kind != "value-ret":
                    uncovered = True
                    why = helper.reason or "arguments are not literals"
                    skipped.append(Skipped(f.name, f"`{helper.name}(..)`: {why}"))
                k = end
                continue
            if t.kind == "punct" and t.text == ";":
                k += 1
                continue
            # Any other statement: the test is not covered by cases alone.
            uncovered = True
            # Skip to the end of the statement.
            depth = 0
            while k < hi:
                x = toks[k]
                if x.kind == "punct" and x.text in "([{":
                    depth += 1
                elif x.kind == "punct" and x.text in ")]}":
                    depth -= 1
                    if depth < 0:
                        break
                    if depth == 0 and x.text == "}" and toks[k + 1].text not in (".", ")", ",", ";", "else"):
                        k += 1
                        break
                elif depth == 0 and x.text == ";":
                    k += 1
                    break
                k += 1
        covered[f.name] = found_any and not uncovered
        if not found_any:
            skipped.append(Skipped(f.name, "no case of a known helper shape"))
        elif uncovered:
            skipped.append(Skipped(f.name, "has statements besides converted cases (kept in Rust)"))
    return cases, skipped, covered


# ---------------------------------------------------------------------------
# Vesper output.


def vesper_string(text: str) -> str:
    out = ['"']
    for ch in text:
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\t":
            out.append("\\t")
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def dedent(source: str) -> str:
    """Strip the indentation a Rust string literal adds to its lines."""
    lines = source.split("\n")
    rest = [l for l in lines[1:] if l.strip()]
    if not rest:
        return source.strip() + "\n"
    indent = min(len(l) - len(l.lstrip(" ")) for l in rest)
    first = lines[0].strip()
    body = [l[indent:] if len(l) >= indent else l.strip() for l in lines[1:]]
    # The closing brace of a top-level item is often indented one level less.
    text = "\n".join([first] + body).strip("\n")
    out = []
    for l in text.split("\n"):
        out.append(l.rstrip())
    return "\n".join(out).strip() + "\n"


def top_level_items(source: str) -> list[tuple[int, int, str | None]]:
    """(start, end, fn name or None) of each top-level item of a Vesper source,
    by brace matching. Attributes and comments before an item belong to it."""
    items = []
    i, n = 0, len(source)
    depth = 0
    start = None
    name = None
    j = 0
    while j < n:
        c = source[j]
        if source.startswith("//", j):
            e = source.find("\n", j)
            if depth == 0 and start is None:
                start = j
            j = n if e < 0 else e
            continue
        if c == '"':
            j += 1
            while j < n and source[j] != '"':
                j += 2 if source[j] == "\\" else 1
            j += 1
            continue
        if depth == 0 and start is None and not c.isspace():
            start = j
        if depth == 0 and name is None and start is not None:
            m = re.match(r"(pub\s+)?(fip\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)", source[j:])
            if m and (j == 0 or not (source[j - 1].isalnum() or source[j - 1] == "_")):
                name = m.group(3)
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                items.append((start, j + 1, name))
                start, name = None, None
        elif c == ";" and depth == 0:
            items.append((start, j + 1, name))
            start, name = None, None
        j += 1
    return items


def header(rel: str, case: Case) -> str:
    return (
        f"// Converted from {rel} (`{case.test}`) by scripts/convert-rust-tests.py.\n"
        "// Run with `vesper test --engines all` (tests/vesper/README.md).\n"
    )


def value_file(rel: str, case: Case) -> str:
    return (
        header(rel, case)
        + "\n"
        + dedent(case.source)
        + "\ntest {\n    @test\n"
        + f"    fn {case.name}() -> bool {{\n        main() == {case.expected}\n    }}\n}}\n"
    )


def error_file(rel: str, case: Case, fn_name: str) -> str | None:
    source = dedent(case.source)
    items = top_level_items(source)
    picked = [it for it in items if it[2] == fn_name]
    if len(picked) != 1:
        return None
    s, e, _ = picked[0]
    rest = (source[:s] + source[e:]).strip()
    attr = f"expect_error = {case.code}"
    if case.message:
        attr += f", message = {vesper_string(case.message)}"
    item = "\n".join("    " + l if l else l for l in source[s:e].strip().split("\n"))
    out = header(rel, case) + "\n"
    if rest:
        out += rest + "\n\n"
    out += "test {\n    @test(" + attr + ")\n" + item + "\n}\n"
    return out


def diagnostics(vesper: str, source: str) -> list[dict]:
    with tempfile.TemporaryDirectory() as d:
        p = Path(d) / "case.ves"
        p.write_text(source)
        out = subprocess.run([vesper, "check", str(p), "--json"], capture_output=True, text=True)
    try:
        data = json.loads(out.stdout)
    except json.JSONDecodeError:
        return []
    if isinstance(data, dict):
        data = data.get("diagnostics", [])
    return data


def fn_holding(source: str, span: tuple[int, int]) -> str | None:
    for s, e, name in top_level_items(source):
        if s <= span[0] < e:
            return name
    return None


def run_case(vesper: str, path: Path, fuel: int) -> tuple[bool, str]:
    out = subprocess.run(
        [vesper, "test", str(path), "--engines", "all", "--no-cache", "--fuel", str(fuel)],
        capture_output=True,
        text=True,
    )
    return out.returncode == 0, (out.stdout + out.stderr).strip()


def safe_name(name: str) -> str:
    return re.sub(r"[^A-Za-z0-9_]", "_", name)


def convert(args) -> int:
    path = Path(args.tests_rs).resolve()
    rel = path.relative_to(ROOT).as_posix()
    src = path.read_text()
    toks = tokenize(src)
    fns = rust_fns(toks)
    shapes = helper_shapes(src, toks, fns)
    cases, skipped, covered = cases_of(src, toks, fns, shapes)
    out_dir = Path(args.out)
    kept, refused = [], []
    used = set()
    with tempfile.TemporaryDirectory() as tmp:
        for case in cases:
            stem = safe_name(case.name)
            while stem in used:
                stem += "_"
            used.add(stem)
            if case.kind == "value":
                text = value_file(rel, case)
            else:
                source = dedent(case.source)
                diags = [
                    d
                    for d in diagnostics(args.vesper, source)
                    if d.get("code") == case.code and (not case.message or case.message in d.get("message", ""))
                ]
                if not diags:
                    refused.append((case, "the checker does not report the diagnostic on the dedented source"))
                    continue
                span = diags[0].get("span", {})
                holder = fn_holding(source, (span.get("start", -1), span.get("end", -1)))
                if holder is None:
                    refused.append((case, "the diagnostic is outside every function"))
                    continue
                text = error_file(rel, case, holder)
                if text is None:
                    refused.append((case, f"could not isolate `fn {holder}`"))
                    continue
            probe = Path(tmp) / f"{stem}.ves"
            probe.write_text(text)
            # The canonical layout (`vesper fmt` on that one file).
            subprocess.run([args.vesper, "fmt", str(probe)], capture_output=True)
            text = probe.read_text()
            ok, log = run_case(args.vesper, probe, args.fuel)
            if ok:
                kept.append((stem, case, text))
            else:
                refused.append((case, "vesper test failed: " + log.splitlines()[-1] if log else "vesper test failed"))
    tests = [f for f in fns if f.is_test]
    converted_tests = [f.name for f in tests if covered.get(f.name)]
    refused_tests = {c.test for c, _ in refused}
    fully = [t for t in converted_tests if t not in refused_tests]
    print(f"{rel}: {len(tests)} #[test] functions, {len(cases)} cases of known shapes")
    print(f"  converted: {len(kept)} cases; #[test] functions fully covered: {len(fully)}")
    for case, why in refused:
        print(f"  refused {case.test}/{case.name}: {why}")
    for s in skipped:
        print(f"  kept in Rust {s.test}: {s.reason}")
    if args.write:
        out_dir.mkdir(parents=True, exist_ok=True)
        for stem, _, text in kept:
            (out_dir / f"{stem}.ves").write_text(text)
        manifest = {
            "from": rel,
            "cases": len(kept),
            "covered_tests": fully,
            "kept_in_rust": sorted({s.test for s in skipped} | refused_tests),
        }
        (out_dir / "converted.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return 0


# ---------------------------------------------------------------------------
# Classification.

CLI_RE = re.compile(r'CARGO_BIN_EXE_vesper|Command::new\(\s*vesper|env!\("CARGO_BIN_EXE')


def classify_file(path: Path) -> dict:
    src = path.read_text(errors="replace")
    lines = src.count("\n")
    tests = len(re.findall(r"#\[test\]", src))
    uses_cli = bool(CLI_RE.search(src))
    vesper_mods = vesper_modules(src)
    other_crates = set(re.findall(r"^use (vesper_[a-z_]+)", src, flags=re.M))
    in_crate = path.parts[-3] if "crates" in path.parts else None
    embeds = len(re.findall(r'fn main\(', src))
    api = (vesper_mods - PROGRAM_PATHS - {"ast"}) | other_crates
    if in_crate is not None or api:
        shape = "rust-api"
    elif embeds:
        shape = "program"
    elif uses_cli:
        shape = "cli"
    else:
        shape = "rust-api"
    try:
        toks = tokenize(src)
        fns = rust_fns(toks)
        shapes = helper_shapes(src, toks, fns)
        cases, _, covered = cases_of(src, toks, fns, shapes)
    except (ValueError, IndexError, AssertionError, AttributeError) as e:
        print(f"warning: {path.relative_to(ROOT)}: not scanned ({e!r})", file=sys.stderr)
        cases, covered = [], {}
    return {
        "cases": len(cases),
        "covered_tests": sum(covered.values()),
        "file": path.relative_to(ROOT).as_posix(),
        "lines": lines,
        "tests": tests,
        "shape": shape,
        "cli": uses_cli,
        "embedded_programs": embeds,
        "api": sorted(api),
    }


def rust_test_files() -> list[Path]:
    files = sorted((ROOT / "tests").glob("*.rs"))
    for crate in sorted((ROOT / "crates").iterdir()):
        t = crate / "tests"
        if t.is_dir():
            files += sorted(t.rglob("*.rs"))
    return files


def converted_from() -> dict[str, dict]:
    out = {}
    base = ROOT / "tests" / "vesper"
    if base.is_dir():
        for m in sorted(base.rglob("converted.json")):
            data = json.loads(m.read_text())
            out[data["from"]] = data
    return out


def classify(args) -> int:
    rows = [classify_file(p) for p in rust_test_files()]
    done = converted_from()
    totals: dict[str, list[int]] = {}
    for r in rows:
        key = ("tests/" if r["file"].startswith("tests/") else "crates/*/tests/") + " " + r["shape"]
        t = totals.setdefault(key, [0, 0, 0, 0, 0])
        t[0] += 1
        t[1] += r["lines"]
        t[2] += r["tests"]
        t[3] += r["cases"]
        t[4] += r["covered_tests"]
    converted_cases = sum(d["cases"] for d in done.values())
    converted_tests = sum(len(d["covered_tests"]) for d in done.values())
    if args.json:
        print(json.dumps({"files": rows, "totals": totals, "converted": done}, indent=2))
        return 0
    print("| Where | Shape | Files | Lines | #[test] | Cases of known shapes | #[test] fully of known shapes |")
    print("|---|---|--:|--:|--:|--:|--:|")
    for key in sorted(totals):
        where, shape = key.split(" ")
        f, l, t, c, ct = totals[key]
        print(f"| `{where}` | {shape} | {f} | {l:,} | {t:,} | {c:,} | {ct:,} |")
    sums = [sum(v[i] for v in totals.values()) for i in range(5)]
    print("| **Total** | | **{:,}** | **{:,}** | **{:,}** | **{:,}** | **{:,}** |".format(*sums))
    print()
    print(f"Converted so far: {len(done)} file(s), {converted_cases} Vesper cases under tests/vesper/, "
          f"{converted_tests} #[test] functions fully covered.")
    if args.files:
        print()
        print("| File | Shape | Lines | #[test] | Cases | Embedded `main`s | API |")
        print("|---|---|--:|--:|--:|--:|---|")
        for r in rows:
            print(f"| `{r['file']}` | {r['shape']} | {r['lines']} | {r['tests']} | {r['cases']} | {r['embedded_programs']} | {', '.join(r['api'][:6])} |")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("classify")
    c.add_argument("--json", action="store_true")
    c.add_argument("--files", action="store_true", help="one row per file")
    v = sub.add_parser("convert")
    v.add_argument("tests_rs")
    v.add_argument("--out", required=True)
    v.add_argument("--vesper", default=os.environ.get("VESPER", "target/release/vesper"))
    v.add_argument("--fuel", type=int, default=200_000_000)
    v.add_argument("--write", action="store_true")
    args = ap.parse_args()
    return classify(args) if args.cmd == "classify" else convert(args)


if __name__ == "__main__":
    sys.exit(main())
