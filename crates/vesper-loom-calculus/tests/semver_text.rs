//! The Vesper SemVer text parser (`semver_text.ves`, through `semver_text`)
//! against the semver crate's `Version` and `VersionReq` parsers: the same
//! parts, or the same error text.
#![cfg(vesper_port)]

use semver::{Op, Version, VersionReq};
use std::fmt::Write as _;

fn rust_version(text: &str) -> String {
    match Version::parse(text) {
        Ok(v) => format!("{}.{}.{}-{}+{}", v.major, v.minor, v.patch, v.pre, v.build),
        Err(e) => format!("!{e}"),
    }
}

fn rust_req(text: &str) -> String {
    match VersionReq::parse(text) {
        Ok(r) => {
            let mut out = String::new();
            for c in &r.comparators {
                let op = match c.op {
                    Op::Exact => 0,
                    Op::Greater => 1,
                    Op::GreaterEq => 2,
                    Op::Less => 3,
                    Op::LessEq => 4,
                    Op::Tilde => 5,
                    Op::Caret => 6,
                    Op::Wildcard => 7,
                    _ => 99,
                };
                let opt = |v: Option<u64>| v.map(|v| v.to_string()).unwrap_or_default();
                write!(
                    out,
                    "{op} {} {} {} {};",
                    c.major,
                    opt(c.minor),
                    opt(c.patch),
                    c.pre
                )
                .unwrap();
            }
            out
        }
        Err(e) => format!("!{e}"),
    }
}

fn check(text: &str) {
    assert_eq!(
        vesper_loom_calculus::semver_text(text, 0),
        rust_version(text),
        "version {text:?}"
    );
    assert_eq!(
        vesper_loom_calculus::semver_text(text, 1),
        rust_req(text),
        "req {text:?}"
    );
}

const CASES: &[&str] = &[
    "",
    " ",
    "1",
    "1.",
    "1.2",
    "1.2.3",
    "01.2.3",
    "1.02.3",
    "1.2.03",
    "0.0.0",
    "1.2.3-",
    "1.2.3+",
    "1.2.3-alpha.1+build.5",
    "1.2.3-01",
    "1.2.3-0a",
    "1.2.3-a..b",
    "1.2.3-a.",
    "1.2.3+01",
    "1.2.3+a b",
    "18446744073709551615.0.0",
    "18446744073709551616.0.0",
    "1.2.3 ",
    "v1.2.3",
    "1.2.3\0",
    "1.2.3é",
    "1.2.3\u{301}",
    "1.2.3'",
    "1.2.3\"",
    "1.2.3\\",
    "1.2.3\t",
    "1.2.3\u{7f}",
    "1.2.3\u{200b}",
    "*",
    "x",
    "X",
    " * ",
    "*, 1",
    "x.1",
    "*.*",
    "1.*",
    "1.x.3",
    "1.*.*",
    "1.2.*",
    "^1",
    "~1.2",
    "=1.2.3",
    ">1",
    ">=1.2",
    "<2",
    "<=2.0.0",
    "^ 1",
    ">= 1.2, < 2",
    "^1,",
    "^1, ",
    "^1 ^2",
    "^1,^2",
    "1.2.3-pre",
    "^1.2-pre",
    ">=1.2.3-rc.1+meta, <2",
    "^1, *",
    "^1, x",
    "^1, xy",
    "=",
    "^",
    "^a",
    "1 2",
    "1,,2",
    "^1.2.3-",
    "~1.2.3+",
    "1.2.3.4",
];

#[test]
fn semver_cases_match_the_semver_crate() {
    for case in CASES {
        check(case);
    }
    let many = vec!["^1"; 33].join(", ");
    check(&many);
    check(&vec!["^1"; 32].join(", "));
    check(&vec!["^1"; 31].join(", "));
}

#[test]
fn mutated_semver_text_matches_the_semver_crate() {
    const ALPHABET: &[&str] = &[
        "0",
        "1",
        "9",
        "00",
        ".",
        "-",
        "+",
        "*",
        "x",
        "X",
        "^",
        "~",
        "=",
        ">",
        "<",
        ">=",
        "<=",
        ",",
        " ",
        "a",
        "Z",
        "é",
        "\u{301}",
        "'",
        "\0",
        "18446744073709551616",
    ];
    let mut state: u64 = std::env::var("VESPER_TOML_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x1234_5678_9abc_def1);
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
        for _ in 0..1 + next() % 3 {
            let at = (next() % (s.len() as u64 + 1)) as usize;
            if next() % 3 == 0 && at < s.len() {
                s.remove(at);
            } else {
                let piece = ALPHABET[(next() % ALPHABET.len() as u64) as usize];
                for (i, c) in piece.chars().enumerate() {
                    s.insert(at + i, c);
                }
            }
        }
        check(&s.into_iter().collect::<String>());
    }
}
