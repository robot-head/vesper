//! Differential and ABBA flip-gate tests for `@vesper/migrate`
//! (Rust-exit SH6 lane 18, #1413 / #2333).
use std::hint::black_box;
use std::time::Instant;
use vesper_migrate::ErrorKind;
use vesper_migrate::c::{filter_directives, strip_comments};

const ABBA_WARMUP_EPOCHS: usize = 2;
const ABBA_EPOCHS: usize = 30;
const ABBA_OPS_PER_WINDOW: usize = 256;

type RefErr = (ErrorKind, Option<usize>, &'static str);

mod reference {
    use super::*;

    pub fn strip_comments(src: &str) -> Result<String, RefErr> {
        let b = src.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        let mut line = 1;
        while i < b.len() {
            match b[i] {
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    let start = line;
                    i += 2;
                    loop {
                        if i + 1 >= b.len() {
                            return Err((
                                ErrorKind::Syntax,
                                Some(start),
                                "unterminated block comment",
                            ));
                        }
                        if b[i] == b'*' && b[i + 1] == b'/' {
                            i += 2;
                            out.push(b' ');
                            break;
                        }
                        if b[i] == b'\n' {
                            line += 1;
                            out.push(b'\n');
                        }
                        i += 1;
                    }
                }
                q @ (b'"' | b'\'') => {
                    out.push(q);
                    i += 1;
                    while i < b.len() && b[i] != q {
                        if b[i] == b'\n' {
                            return Err((ErrorKind::Syntax, Some(line), "unterminated literal"));
                        }
                        if b[i] == b'\\' && i + 1 < b.len() {
                            out.push(b[i]);
                            i += 1;
                        }
                        out.push(b[i]);
                        i += 1;
                    }
                    if i < b.len() {
                        out.push(q);
                        i += 1;
                    }
                }
                c => {
                    if c == b'\n' {
                        line += 1;
                    }
                    out.push(c);
                    i += 1;
                }
            }
        }
        String::from_utf8(out).map_err(|_| (ErrorKind::Syntax, None, "header is not UTF-8"))
    }

    fn is_ident(s: &str) -> bool {
        let mut chars = s.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    #[derive(PartialEq)]
    enum Cond {
        Guard,
        Cplusplus,
    }

    pub fn filter_directives(src: &str) -> Result<String, RefErr> {
        let mut out = String::with_capacity(src.len());
        let mut stack: Vec<Cond> = vec![];
        let mut skip_depth = 0usize;
        for (index, raw) in src.split('\n').enumerate() {
            let line_no = index + 1;
            let err = |msg: &'static str| (ErrorKind::Preprocessor, Some(line_no), msg);
            if index > 0 {
                out.push('\n');
            }
            let line = raw.trim();
            if line.ends_with('\\') {
                return Err(err("line continuations are outside the supported subset"));
            }
            let Some(directive) = line.strip_prefix('#') else {
                if skip_depth == 0 {
                    out.push_str(raw);
                }
                continue;
            };
            let mut words = directive.split_whitespace();
            let name = words.next().unwrap_or("");
            let args: Vec<&str> = words.collect();
            if skip_depth > 0 {
                match name {
                    "if" | "ifdef" | "ifndef" => skip_depth += 1,
                    "endif" => {
                        skip_depth -= 1;
                        if skip_depth == 0 {
                            stack.pop();
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match (name, args.as_slice()) {
                ("include", [h]) if matches!(*h, "<stdint.h>" | "<stddef.h>" | "<stdbool.h>") => {}
                ("include", _) => {
                    return Err(err(
                        "only <stdint.h>, <stddef.h> and <stdbool.h> may be included; inline other headers",
                    ));
                }
                ("pragma", ["once"]) => {}
                ("ifndef", [g]) if is_ident(g) => stack.push(Cond::Guard),
                ("define", [g]) if is_ident(g) && stack.last() == Some(&Cond::Guard) => {}
                ("define", _) => {
                    return Err(err(
                        "macros are outside the supported subset (only include guards)",
                    ));
                }
                ("ifdef", ["__cplusplus"]) => {
                    stack.push(Cond::Cplusplus);
                    skip_depth = 1;
                }
                ("endif", []) => {
                    if stack.pop().is_none() {
                        return Err(err("#endif without a matching conditional"));
                    }
                }
                _ => return Err(err("preprocessor directive outside the supported subset")),
            }
        }
        if !stack.is_empty() {
            return Err((ErrorKind::Preprocessor, None, "unterminated conditional"));
        }
        Ok(out)
    }
}

const SAMPLE_HEADER: &str = r#"
/* Multi-line
 * header comment
 */
#ifndef LEGACY_CODEC_H
#define LEGACY_CODEC_H
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#ifdef NESTED_CXX
#endif
#endif

// Line comment with "quotes" and /* nested */
typedef struct Packet {
    uint32_t magic; /* inline */
    uint32_t flags;
    size_t len;
} Packet;

int32_t decode_packet(const uint8_t *buf, size_t len, Packet *out);

#ifdef __cplusplus
}
#endif
#endif
"#;

#[test]
fn c_preprocess_matches_rust_reference() {
    let cases = [
        SAMPLE_HEADER,
        "int x = 1; /* unterminated",
        "const char *s = \"unterminated\n\";",
        "#define FOO 1\n",
        "#include <stdio.h>\n",
        "#endif\n",
        "#ifndef GUARD\n#define GUARD\nint x;\n",
        "int x = 1; \\\nint y = 2;\n",
        "#ifdef OTHER\n#endif\n",
        "const char *s = \"a \\\" // not a comment\"; // real comment\n",
    ];

    for src in cases {
        let got_s = strip_comments(src);
        let want_s = reference::strip_comments(src);
        assert_eq!(
            got_s
                .as_ref()
                .map_err(|e| (e.kind, e.line, e.message.as_str())),
            want_s.as_ref().map_err(|e| *e),
            "strip_comments: {src:?}"
        );
        if let Ok(stripped) = got_s {
            let got_f = filter_directives(&stripped);
            let want_f = reference::filter_directives(&stripped);
            assert_eq!(
                got_f
                    .as_ref()
                    .map_err(|e| (e.kind, e.line, e.message.as_str())),
                want_f.as_ref().map_err(|e| *e),
                "filter_directives: {src:?}"
            );
        }
    }
    println!(
        "flip-gate differential differences=0 cases={}",
        cases.len() * 2
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_c_preprocess() {
    let mut big = String::new();
    big.push_str("#ifndef BIG_H\n#define BIG_H\n#include <stdint.h>\n");
    for i in 0..32 {
        big.push_str(&format!(
            "/* block comment {i}\n * second line */\n#ifdef __cplusplus\nextern \"C\" {{}}\n#endif\n// line comment {i}\nint32_t fn_{i}(const uint8_t *p, size_t n);\n"
        ));
    }
    big.push_str("#endif\n");

    let run_rust = || {
        for _ in 0..ABBA_OPS_PER_WINDOW {
            let s = reference::strip_comments(black_box(&big)).unwrap();
            let f = reference::filter_directives(black_box(&s)).unwrap();
            black_box(f.len());
        }
    };
    let run_vesper = || {
        for _ in 0..ABBA_OPS_PER_WINDOW {
            let s = strip_comments(black_box(&big)).unwrap();
            let f = filter_directives(black_box(&s)).unwrap();
            black_box(f.len());
        }
    };

    for _ in 0..ABBA_WARMUP_EPOCHS {
        run_rust();
        run_vesper();
        run_vesper();
        run_rust();
    }

    let mut pairs = Vec::with_capacity(ABBA_EPOCHS);
    for _ in 0..ABBA_EPOCHS {
        let t0 = Instant::now();
        run_rust();
        let a1 = t0.elapsed().as_nanos().max(1);
        let t1 = Instant::now();
        run_vesper();
        let b1 = t1.elapsed().as_nanos().max(1);
        let t2 = Instant::now();
        run_vesper();
        let b2 = t2.elapsed().as_nanos().max(1);
        let t3 = Instant::now();
        run_rust();
        let a2 = t3.elapsed().as_nanos().max(1);
        pairs.push(format!("{}:{}", a1 + a2, b1 + b2));
    }
    println!("flip-gate pairs c_preprocess {}", pairs.join(" "));
}
