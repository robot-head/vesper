//! The Vesper TOML reader (@vesper-std/toml, through `read_toml`) against
//! the `toml` crate the Rust build parses manifests with: the same documents
//! are accepted with the same tree and spans (toml_edit's `ImDocument`, which
//! `toml::from_str` deserializes from), and a rejected document gets the
//! same error text, byte for byte.
#![cfg(vesper_port)]

use std::fmt::Write as _;
use toml_edit::{ImDocument, Item, Table, Value};

fn span(out: &mut String, span: Option<std::ops::Range<usize>>) {
    match span {
        Some(r) => write!(out, "@{},{}", r.start, r.end).unwrap(),
        None => out.push_str("@-"),
    }
}

fn bytes(out: &mut String, text: &str) {
    out.push('"');
    for &b in text.as_bytes() {
        if !(32..=126).contains(&b) || b == b'"' || b == b'\\' {
            write!(out, "\\x{b:02x}").unwrap();
        } else {
            out.push(b as char);
        }
    }
    out.push('"');
}

fn value(out: &mut String, src: &str, v: &Value) {
    span(out, v.span());
    let raw = || &src[v.span().expect("a parsed value has a span")];
    match v {
        Value::String(s) => {
            out.push('s');
            bytes(out, s.value());
        }
        Value::Integer(i) => write!(out, "i{}", i.value()).unwrap(),
        Value::Float(f) => {
            let x = *f.value();
            let special = match (x.is_infinite(), x.is_nan(), x.is_sign_negative()) {
                (true, _, false) => 1,
                (true, _, true) => 2,
                (_, true, false) => 3,
                (_, true, true) => 4,
                _ => 0,
            };
            write!(out, "f{}/{special}", raw()).unwrap();
        }
        Value::Boolean(b) => write!(out, "b{}", u8::from(*b.value())).unwrap(),
        Value::Datetime(_) => write!(out, "d{}", raw()).unwrap(),
        Value::Array(a) => {
            out.push('[');
            for v in a.iter() {
                value(out, src, v);
                out.push(';');
            }
            out.push(']');
        }
        Value::InlineTable(t) => {
            if t.is_dotted() {
                out.push('d');
            }
            out.push('{');
            for (k, _) in t.iter() {
                let (key, item) = t.get_key_value(k).unwrap();
                bytes(out, key.get());
                span(out, key.span());
                out.push('=');
                self::item(out, src, item);
                out.push(';');
            }
            out.push('}');
        }
    }
}

fn table(out: &mut String, src: &str, t: &Table) {
    span(out, t.span());
    out.push('T');
    if t.is_implicit() {
        out.push('i');
    }
    if t.is_dotted() {
        out.push('d');
    }
    out.push('{');
    for (k, _) in t.iter() {
        let (key, item) = t.get_key_value(k).unwrap();
        bytes(out, key.get());
        span(out, key.span());
        out.push('=');
        self::item(out, src, item);
        out.push(';');
    }
    out.push('}');
}

fn item(out: &mut String, src: &str, item: &Item) {
    match item {
        Item::None => out.push_str("<none>"),
        Item::Value(v) => value(out, src, v),
        Item::Table(t) => table(out, src, t),
        Item::ArrayOfTables(a) => {
            span(out, a.span());
            out.push_str("A[");
            for t in a.iter() {
                table(out, src, t);
                out.push(';');
            }
            out.push(']');
        }
    }
}

/// The Rust build's dump, in `read_toml`'s format.
fn rust_dump(src: &str) -> String {
    match ImDocument::parse(src) {
        Ok(doc) => {
            let mut out = String::new();
            table(&mut out, src, doc.as_table());
            out
        }
        Err(_) => {
            let error = toml::from_str::<toml::Table>(src).expect_err("toml_edit rejected it");
            format!("!{error}")
        }
    }
}

fn check(src: &str) {
    let want = rust_dump(src);
    let got = vesper_loom_calculus::read_toml(src);
    assert_eq!(got, want, "on {src:?}");
}

const CASES: &[&str] = &[
    "",
    "\u{feff}a = 1",
    "a = 1",
    "a = \"\\q\"",
    "a = 99999999999999999999",
    "a = 0x8000000000000000",
    "a = -9223372036854775808",
    "a = 9223372036854775807",
    "a = -0x1",
    "a = 0o777\nb = 0b1010\nc = 0xDEAD_beef",
    "[a]\n[a]\n",
    "[[a]]\n[a]\n",
    "a = 1\n[a]\n",
    "a.b = 1\n[a]\n",
    "a = \"x\r\"",
    "a = 1\r",
    "a = 1\r\n",
    "a = tru",
    "a = true\nb = false",
    "a = 1e400",
    "a = 1e308\nb = -1.5E-3\nc = 3.0\nd = inf\ne = -inf\nf = +nan\ng = -nan",
    "a = 2026-02-30",
    "a = 1979-05-27T07:32:00Z\nb = 1979-05-27 07:32:00.999-07:00\nc = 07:32:00\nd = 1979-05-27",
    "a = [1,,]",
    "a = [1, 2, [3, \"x\"], ]",
    "a = [\n  1, # one\n  2,\n]",
    "a = {a=1,}",
    "a = {a=1, a=2}",
    "a = { b.c = 1, b.d = 2 }",
    "x = \"é\" é",
    "[",
    "a = \"\\u00\"",
    "a = \"\\uD800\"",
    "a = \"\\u00e9\\U0001F600\\n\\t\\\\\\\"\"",
    "a.b = 1\na.b.c = 2",
    "a = 0123",
    "a = +",
    "a = 1__2",
    "é = 1",
    "a = 1979-05-27T25:00:00",
    "a = 'x",
    "a = \"\"\"x",
    "a = '''x",
    "a = \"\"\"\nline\\\n   joined\"\"\"\"\"",
    "a = '''\n'raw' ''''",
    "a = [",
    "a = {",
    "[[a]]\n[[a.b]]\n[a]\n",
    "[a.b]\n[a]\n[a]\n",
    "a = 1 b",
    "\"a\" = 1\n'a' = 2",
    "[package]\nname = \"@o/a\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"std/bytes\" = { version = \"^1\" }\n",
    "[a]\nb = 1\n[a.c]\nd = 2\n[[e]]\nf = 1\n[[e]]\nf = 2\n",
    "a.b.c = 1\na.d = 2\n[x.y]\n",
    "# comment\n\n  a = 1 # trailing\n",
    "a = 1\na = 2",
    "[a]\nb = 1\n[a]\nc = 2",
    "[ a . b ]\n",
    "[a]]\n",
    "[[a]\n",
    "a = \"\u{7f}\"",
    "a = \"\u{1}\"",
    "a = 1 # \u{1}",
    "a",
    "=1",
    "a =",
    "a = 1\n\n\n  b",
    "[[a]]\n['a']\n",
    "['a b']\n['a b']\n",
    "[x.'b c']\n[x.'b c']\n",
    "['a\"b']\n['a\"b']\n",
    "['']\n['']\n",
    "[\"a\\tb\\u0001'\"]\n[\"a\\tb\\u0001'\"]\n",
    "['é']\n['é']\n",
];

#[test]
fn toml_cases_match_the_toml_crate() {
    for case in CASES {
        check(case);
    }
}

/// The repository checkout, or `None` (with a note) when the test runs
/// without it, as under Bazel, whose runfiles hold only declared inputs.
fn repository() -> Option<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if root.join("Cargo.lock").is_file() && root.join("packages").is_dir() {
        Some(root)
    } else {
        eprintln!("no repository checkout next to the crate; not compared");
        None
    }
}

/// Every Loom.toml in the repository parses to the same tree.
#[test]
fn repository_manifests_match_the_toml_crate() {
    let Some(root) = repository() else { return };
    let mut seen = 0;
    for dir in ["packages", "crates", "examples"] {
        let mut stack = vec![root.join(dir)];
        while let Some(path) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    if p.file_name()
                        .is_some_and(|n| n != "target" && n != "node_modules")
                    {
                        stack.push(p);
                    }
                } else if p.extension().is_some_and(|e| e == "toml")
                    && let Ok(text) = std::fs::read_to_string(&p)
                {
                    check(&text);
                    seen += 1;
                }
            }
        }
    }
    assert!(seen > 50, "only {seen} TOML files");
}

/// Random edits of the cases: inserted TOML punctuation, deletions and
/// swapped lines, deterministically seeded.
#[test]
fn mutated_documents_match_the_toml_crate() {
    const ALPHABET: &[&str] = &[
        "[", "]", "{", "}", "=", ",", ".", "\"", "'", "\"\"\"", "'''", "\n", "\r\n", " ", "#",
        "\\", "\\u", "a", "1", "0x", "e", "-", "+", "_", ":", "T", "Z", "é", "\t", "inf", "nan",
        "true",
    ];
    let mut state: u64 = std::env::var("VESPER_TOML_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let iterations: u64 = std::env::var("VESPER_TOML_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    for _ in 0..iterations {
        let base = CASES[(next() % CASES.len() as u64) as usize];
        let mut s: Vec<char> = base.chars().collect();
        let edits = 1 + next() % 4;
        for _ in 0..edits {
            let at = if s.is_empty() {
                0
            } else {
                (next() % (s.len() as u64 + 1)) as usize
            };
            match next() % 3 {
                0 => {
                    let piece = ALPHABET[(next() % ALPHABET.len() as u64) as usize];
                    for (i, c) in piece.chars().enumerate() {
                        s.insert(at + i, c);
                    }
                }
                1 if at < s.len() => {
                    s.remove(at);
                }
                _ => {
                    let text: String = s.iter().collect();
                    let mut lines: Vec<&str> = text.split('\n').collect();
                    if lines.len() > 1 {
                        let i = (next() % lines.len() as u64) as usize;
                        let j = (next() % lines.len() as u64) as usize;
                        lines.swap(i, j);
                    }
                    s = lines.join("\n").chars().collect();
                }
            }
        }
        let text: String = s.into_iter().collect();
        check(&text);
    }
}

/// Random documents of table headers and key/value lines over a few names,
/// so headers, dotted keys, arrays of tables and inline tables meet in
/// every order (duplicate, extend and wrong-type errors included).
#[test]
fn generated_table_structures_match_the_toml_crate() {
    const KEYS: &[&str] = &[
        "a",
        "b",
        "a.b",
        "b.a",
        "a.b.c",
        "\"a\"",
        "'b'.c",
        "a . b",
        "'a b'",
        "a.'b c'",
        "\"a\\tb\"",
        "'a\"b'",
    ];
    const VALUES: &[&str] = &[
        "1",
        "{}",
        "{ c = 1 }",
        "{ b.c = 1 }",
        "[]",
        "[{}]",
        "[{ c = 1 }, 2]",
        "\"s\"",
        "1979-05-27",
        "{ a = { b = 1 } }",
        "[[1], [2]]",
    ];
    let mut state: u64 = std::env::var("VESPER_TOML_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x2545_f491_4f6c_dd1d);
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut pick = |n: usize| -> usize { (next() % n as u64) as usize };
    let iterations: usize = std::env::var("VESPER_TOML_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    for _ in 0..iterations {
        let mut doc = String::new();
        for _ in 0..1 + pick(6) {
            let key = KEYS[pick(KEYS.len())];
            match pick(4) {
                0 => writeln!(doc, "[{key}]").unwrap(),
                1 => writeln!(doc, "[[{key}]]").unwrap(),
                _ => writeln!(doc, "{key} = {}", VALUES[pick(VALUES.len())]).unwrap(),
            }
        }
        check(&doc);
    }
}

/// The chars `{:?}` of a `str` writes as `\u{...}` (Rust's printable and
/// Grapheme_Extend tables), as inclusive ranges.
fn rust_escape_ranges() -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for u in 0u32..=0x10_ffff {
        let Some(c) = char::from_u32(u) else { continue };
        if format!("{:?}", c.to_string()).starts_with("\"\\u{") {
            match ranges.last_mut() {
                Some(r) if r.1 + 1 == u => r.1 = u,
                _ => ranges.push((u, u)),
            }
        }
    }
    ranges
}

/// packages/vesper-std/toml/src/escape.ves holds `rust_escape_ranges` for
/// one Unicode version. On a toolchain with that version it must match;
/// `VESPER_TOML_BLESS=1` rewrites it from the running toolchain.
#[test]
fn debug_escape_table_matches_the_rust_toolchain() {
    let Some(root) = repository() else { return };
    let path = root.join("packages/vesper-std/toml/src/escape.ves");
    let (major, minor, update) = char::UNICODE_VERSION;
    let ranges = rust_escape_ranges();
    if std::env::var_os("VESPER_TOML_BLESS").is_some() {
        let mut out = String::new();
        out.push_str(
            "// @vesper-std/toml: the chars Rust's `Debug` for `str` and `char` writes\n\
             // as `\\u{...}` (not printable, or Grapheme_Extend), as inclusive\n\
             // (first, last) ranges in code point order, six hex digits each.\n\
             // Generated from the Rust toolchain by `VESPER_TOML_BLESS=1 cargo test\n\
             // -p vesper-loom-calculus --test toml_reader debug_escape` and checked\n\
             // by that test.\n\n",
        );
        writeln!(
            out,
            "pub const UNICODE_VERSION: Str = \"{major}.{minor}.{update}\";\n"
        )
        .unwrap();
        out.push_str("pub const ESCAPE_RANGES: Str = \"");
        for (a, b) in &ranges {
            write!(out, "{a:06x}{b:06x}").unwrap();
        }
        out.push_str("\";\n");
        std::fs::write(&path, out).unwrap();
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let version = format!("\"{major}.{minor}.{update}\"");
    if !text.contains(&version) {
        eprintln!("escape.ves is for another Unicode version than {version}; not compared");
        return;
    }
    let table = text
        .split_once("ESCAPE_RANGES: Str = \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .expect("escape.ves defines ESCAPE_RANGES")
        .0;
    let hex = |h: &str| u32::from_str_radix(h, 16).unwrap();
    let committed: Vec<(u32, u32)> = table
        .as_bytes()
        .chunks(12)
        .map(|r| {
            let r = std::str::from_utf8(r).unwrap();
            (hex(&r[..6]), hex(&r[6..]))
        })
        .collect();
    assert_eq!(committed, ranges);
}
