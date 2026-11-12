//! Macro fixture component for the `vesper-forge-runner` tests.
//!
//! It implements the read-only `vesper:sandbox/step@0.1.0` protocol and
//! behaves like a small Tier 2 macro: `template` reads a template file from
//! a declared read-only input and returns its tokens as flat wire output.
//! Everything it returns is untrusted; the host decides provenance.
//!
//! Template text uses the Forge lexical-fragment token rules (identifiers
//! may contain `-` and `/` and start with `@`; `::` is one token) plus
//! `$` directives:
//!
//! * `$input`        the invocation's input tokens, echoed verbatim;
//! * `$seed`         an integer from the WASI secure random stream;
//! * `$clock`        the WASI wall-clock seconds as an integer;
//! * `$claim:P:I`    attach a `pre-resolved` claim `{P, I}` to the next
//!   identifier (a claim, never authority).
//!
//! Optional `key=value` arguments after the template path forge wire
//! metadata on every returned token: `scopes=1,2` and `span=file:start:end`.
//!
//! Other entries return hostile or failing results the host must reject.

wit_bindgen::generate!({
    path: "../../../vesper-loom-sandbox/wit",
    world: "sandboxed-step",
});

use exports::vesper::sandbox::step::Guest;
use vesper::sandbox::types::{
    DefId, Delimiter, Invocation, PackedDiagnostic, PackedResult, PackedToken, Severity, Slice,
    Span, Token, TokenKind,
};

const PROTOCOL_VERSION: u32 = 1;

struct Diagnostic {
    severity: Severity,
    message: String,
}

#[derive(Default)]
struct Packer {
    tokens: Vec<PackedToken>,
    diagnostics: Vec<PackedDiagnostic>,
    text: Vec<u8>,
    scopes: Vec<u32>,
}

impl Packer {
    fn text(&mut self, s: &str) -> Slice {
        let offset = self.text.len() as u32;
        self.text.extend_from_slice(s.as_bytes());
        Slice {
            offset,
            len: s.len() as u32,
        }
    }

    fn token(&mut self, t: &Token) {
        let text = self.text(&t.text);
        let scopes = Slice {
            offset: self.scopes.len() as u32,
            len: t.scopes.len() as u32,
        };
        self.scopes.extend_from_slice(&t.scopes);
        self.tokens.push(PackedToken {
            kind: t.kind,
            text,
            span: t.span,
            scopes,
            pre_resolved: t.pre_resolved,
        });
    }

    fn diag(&mut self, d: &Diagnostic) {
        let message = self.text(&d.message);
        self.diagnostics.push(PackedDiagnostic {
            severity: d.severity,
            message,
            span: None,
        });
    }

    fn finish(self) -> PackedResult {
        PackedResult {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
            text: self.text,
            scopes: self.scopes,
        }
    }
}

const ZERO: Span = Span {
    file: 0,
    start: 0,
    end: 0,
};

fn token(kind: TokenKind, text: impl Into<String>) -> Token {
    Token {
        kind,
        text: text.into(),
        span: ZERO,
        scopes: Vec::new(),
        pre_resolved: None,
    }
}

fn error(message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        message: message.into(),
    }
}

fn note(message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Note,
        message: message.into(),
    }
}

fn ok(tokens: &[Token], diagnostics: &[Diagnostic]) -> Result<PackedResult, PackedResult> {
    let mut p = Packer::default();
    for t in tokens {
        p.token(t);
    }
    for d in diagnostics {
        p.diag(d);
    }
    Ok(p.finish())
}

fn fail(diagnostics: &[Diagnostic]) -> Result<PackedResult, PackedResult> {
    let mut p = Packer::default();
    for d in diagnostics {
        p.diag(d);
    }
    Err(p.finish())
}

fn raw_ident(text: Slice) -> PackedToken {
    PackedToken {
        kind: TokenKind::Ident,
        text,
        span: ZERO,
        scopes: Slice { offset: 0, len: 0 },
        pre_resolved: None,
    }
}

struct Fixture;

impl Guest for Fixture {
    fn protocol_version() -> u32 {
        PROTOCOL_VERSION
    }

    fn run(inv: Invocation) -> Result<PackedResult, PackedResult> {
        if inv.protocol_version != PROTOCOL_VERSION {
            return fail(&[error(format!(
                "protocol {} unsupported",
                inv.protocol_version
            ))]);
        }
        match inv.entry.as_str() {
            "template" => match template(&inv) {
                Ok((tokens, notes)) => ok(&tokens, &notes),
                Err(d) => fail(&[d]),
            },
            "echo" => ok(&inv.tokens, &[note("echo")]),
            // A successful result that nevertheless carries an error.
            "error-diagnostic" => ok(
                &[token(TokenKind::IntLiteral, "1")],
                &[error("macro reported a problem")],
            ),
            "fail" => fail(&[error("macro failed")]),
            "panic" => panic!("fixture panic requested"),
            "spin" => {
                let mut x: u64 = 0;
                loop {
                    x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
            }
            "write-out" => {
                if let Err(e) = std::fs::write("/out/stray.txt", "not a token") {
                    return fail(&[error(format!("write: {e}"))]);
                }
                ok(&[token(TokenKind::IntLiteral, "1")], &[])
            }
            "flood" => {
                let n: usize = inv
                    .args
                    .first()
                    .and_then(|a| a.parse().ok())
                    .unwrap_or(0);
                let x = token(TokenKind::Ident, "x");
                ok(&vec![x; n], &[])
            }
            "unbalanced" => ok(
                &[
                    token(TokenKind::Open(Delimiter::Brace), "{"),
                    token(TokenKind::IntLiteral, "1"),
                ],
                &[],
            ),
            // 1 MiB of text referenced by 100k tokens.
            "alias-bomb" => {
                let mut p = Packer {
                    text: vec![b'a'; 1 << 20],
                    ..Packer::default()
                };
                let all = Slice {
                    offset: 0,
                    len: 1 << 20,
                };
                p.tokens = (0..100_000).map(|_| raw_ident(all)).collect();
                Ok(p.finish())
            }
            "bad-slice" => {
                let mut p = Packer {
                    text: b"abc".to_vec(),
                    ..Packer::default()
                };
                p.tokens = vec![raw_ident(Slice {
                    offset: 2,
                    len: u32::MAX,
                })];
                Ok(p.finish())
            }
            "bad-utf8" => {
                let mut p = Packer {
                    text: vec![0xff, 0xfe],
                    ..Packer::default()
                };
                p.tokens = vec![raw_ident(Slice { offset: 0, len: 2 })];
                Ok(p.finish())
            }
            "error-with-tokens" => {
                let mut p = Packer {
                    text: b"x".to_vec(),
                    ..Packer::default()
                };
                p.tokens = vec![raw_ident(Slice { offset: 0, len: 1 })];
                Err(p.finish())
            }
            other => fail(&[error(format!("unknown entry `{other}`"))]),
        }
    }
}

fn template(inv: &Invocation) -> Result<(Vec<Token>, Vec<Diagnostic>), Diagnostic> {
    let path = inv
        .args
        .first()
        .ok_or_else(|| error("missing template path"))?;
    let src = std::fs::read_to_string(path).map_err(|e| error(format!("read {path}: {e}")))?;
    let mut scopes = Vec::new();
    let mut span = None;
    for opt in &inv.args[1..] {
        let (k, v) = opt
            .split_once('=')
            .ok_or_else(|| error(format!("bad option `{opt}`")))?;
        match k {
            "scopes" => {
                scopes = v
                    .split(',')
                    .map(|s| s.parse::<u32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| error(format!("bad scopes `{v}`")))?
            }
            "span" => {
                let n: Vec<u32> = v
                    .split(':')
                    .map(|s| s.parse::<u32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| error(format!("bad span `{v}`")))?;
                let [file, start, end] = n[..] else {
                    return Err(error(format!("bad span `{v}`")));
                };
                span = Some(Span { file, start, end });
            }
            _ => return Err(error(format!("unknown option `{k}`"))),
        }
    }
    let mut out = lex(&src, inv)?;
    for t in &mut out {
        if !scopes.is_empty() {
            t.scopes = scopes.clone();
        }
        if let Some(s) = span {
            t.span = s;
        }
    }
    let notes = vec![note(format!("template {path}: {} tokens", out.len()))];
    Ok((out, notes))
}

fn lex(src: &str, inv: &Invocation) -> Result<Vec<Token>, Diagnostic> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut claim: Option<DefId> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c == '$' {
            i += 1;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == ':') {
                i += 1;
            }
            let directive: String = chars[start + 1..i].iter().collect();
            match directive.as_str() {
                "input" => out.extend(inv.tokens.iter().cloned()),
                "seed" => out.push(token(
                    TokenKind::IntLiteral,
                    wasip2::random::random::get_random_u64().to_string(),
                )),
                "clock" => out.push(token(
                    TokenKind::IntLiteral,
                    wasip2::clocks::wall_clock::now().seconds.to_string(),
                )),
                d if d.starts_with("claim:") => {
                    let parts: Vec<&str> = d.split(':').collect();
                    let (Some(p), Some(it)) = (
                        parts.get(1).and_then(|p| p.parse().ok()),
                        parts.get(2).and_then(|p| p.parse().ok()),
                    ) else {
                        return Err(error(format!("bad claim `{d}`")));
                    };
                    claim = Some(DefId {
                        package: p,
                        item: it,
                    });
                }
                d => return Err(error(format!("unknown directive `${d}`"))),
            }
            continue;
        }
        let kind = if c.is_alphabetic() || c == '_' || c == '@' {
            i += 1;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-' | '/'))
            {
                i += 1;
            }
            TokenKind::Ident
        } else if c.is_ascii_digit() {
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            TokenKind::IntLiteral
        } else if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            TokenKind::StringLiteral
        } else if c == ':' && chars.get(i + 1) == Some(&':') {
            i += 2;
            TokenKind::Punct
        } else {
            i += 1;
            match c {
                '(' => TokenKind::Open(Delimiter::Paren),
                ')' => TokenKind::Close(Delimiter::Paren),
                '[' => TokenKind::Open(Delimiter::Bracket),
                ']' => TokenKind::Close(Delimiter::Bracket),
                '{' => TokenKind::Open(Delimiter::Brace),
                '}' => TokenKind::Close(Delimiter::Brace),
                _ => TokenKind::Punct,
            }
        };
        let mut t = token(kind, chars[start..i].iter().collect::<String>());
        if matches!(kind, TokenKind::Ident) {
            t.pre_resolved = claim.take();
        }
        out.push(t);
    }
    Ok(out)
}

export!(Fixture);
