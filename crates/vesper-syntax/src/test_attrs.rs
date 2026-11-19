//! Token-stream parser for `@test`, `@test(...)`, and `@test_case(...)`
//! attributes attached to top-level `fn` declarations.
//!
//! This module extracts structured test metadata directly from the real
//! [`crate::lexer`] token stream without altering `vesper_core::ast::Function`.

use crate::ast::{Diagnostic, Span};
use crate::lexer::{self, Token, TokenKind as K};
use std::collections::BTreeMap;

/// A typed value inside `@test(key = val, ...)` or `@test_case(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrValue {
    /// Identifier, error code, or enum variant (e.g., `E0302`, `TornWalSector`).
    Ident(String),
    /// Unescaped string literal (e.g., `"linear value"`).
    String(String),
    /// Signed integer literal (e.g., `1000`, `-42`, `0xFF`).
    Int(i128),
    /// Floating-point literal source spelling (e.g., `3.14`, `-0.5`).
    Float(String),
    /// Boolean literal (`true` or `false`).
    Bool(bool),
    /// Compound token slice for non-atomic arguments (e.g., `(1, 2)`, `Foo::Bar`).
    Raw(String),
}

impl AttrValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) | Self::Ident(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Int(v) => u64::try_from(*v).ok(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(v) => i64::try_from(*v).ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// One positional or named argument in `@test_case(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCaseArg {
    /// `Some(key)` for `key = value`, or `None` for positional arguments.
    pub name: Option<String>,
    pub value: AttrValue,
    pub span: Span,
}

/// A single repeatable `@test_case(...)` attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCaseAttr {
    pub args: Vec<TestCaseArg>,
    pub span: Span,
}

/// Parsed `@test` or `@test(key = val, ...)` attribute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestAttr {
    /// Byte span of the `@test` or `@test(...)` attribute in source.
    pub span: Span,
    /// `expect_error = E0302`
    pub expect_error: Option<String>,
    /// `primary = "..."`
    pub primary: Option<String>,
    /// `related = "..."`
    pub related: Option<String>,
    /// `span = "..."`
    pub expected_span: Option<String>,
    /// `cases = 1000`
    pub cases: Option<u64>,
    /// `sim_schedules = 256`
    pub sim_schedules: Option<u64>,
    /// `fault_profile = TornWalSector`
    pub fault_profile: Option<String>,
    /// `max_stack = 384`
    pub max_stack: Option<u64>,
    /// `max_cycles = 5000`
    pub max_cycles: Option<u64>,
    /// `zero_alloc = true`
    pub zero_alloc: Option<bool>,
    /// Additional `key = val` pairs not covered by built-in fields.
    pub extra: BTreeMap<String, AttrValue>,
}

/// All `@test` / `@test_case` attributes attached to a top-level `fn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionTestAttrs {
    /// Name of the annotated function.
    pub fn_name: String,
    /// Byte span of the function's identifier token.
    pub fn_name_span: Span,
    /// Byte span from the first attached test attribute through `fn_name`.
    pub span: Span,
    /// Parsed `@test` / `@test(...)` attribute, if present.
    pub test: Option<TestAttr>,
    /// Repeatable `@test_case(...)` attributes in source order.
    pub test_cases: Vec<TestCaseAttr>,
}

/// Extracts all `@test` and `@test_case` attribute sets attached to top-level
/// `fn` declarations in `source`.
pub fn extract_test_attrs(source: &str) -> Vec<FunctionTestAttrs> {
    extract_test_attrs_with_diagnostics(source).0
}

/// Extracts all `@test` and `@test_case` attribute sets along with any
/// attribute-argument parse diagnostics.
pub fn extract_test_attrs_with_diagnostics(
    source: &str,
) -> (Vec<FunctionTestAttrs>, Vec<Diagnostic>) {
    let (raw_tokens, mut diagnostics) = lexer::lex(source);
    let tokens: Vec<Token> = raw_tokens
        .into_iter()
        .filter(|t| !t.kind.trivia())
        .collect();
    let mut parser = AttrStream {
        source,
        tokens: &tokens,
        pos: 0,
        diagnostics: &mut diagnostics,
    };
    let out = parser.extract_all();
    (out, diagnostics)
}

struct AttrStream<'a> {
    source: &'a str,
    tokens: &'a [Token],
    pos: usize,
    diagnostics: &'a mut Vec<Diagnostic>,
}

impl<'a> AttrStream<'a> {
    fn kind(&self) -> K {
        self.tokens.get(self.pos).map_or(K::Eof, |t| t.kind)
    }

    fn nth_kind(&self, offset: usize) -> K {
        self.tokens
            .get(self.pos + offset)
            .map_or(K::Eof, |t| t.kind)
    }

    fn span_at(&self, offset: usize) -> Span {
        self.tokens
            .get(self.pos + offset)
            .map(|t| t.span.clone())
            .unwrap_or_else(|| self.source.len()..self.source.len())
    }

    fn text_at(&self, offset: usize) -> &'a str {
        &self.source[self.span_at(offset)]
    }

    fn at_word(&self, offset: usize, word: &str) -> bool {
        self.nth_kind(offset) == K::Lower && self.text_at(offset) == word
    }

    fn bump(&mut self) -> Token {
        if let Some(tok) = self.tokens.get(self.pos) {
            if tok.kind != K::Eof {
                self.pos += 1;
            }
            tok.clone()
        } else {
            Token {
                kind: K::Eof,
                span: self.source.len()..self.source.len(),
            }
        }
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic::new("E0002", msg, span));
    }

    fn extract_all(&mut self) -> Vec<FunctionTestAttrs> {
        let mut results = Vec::new();
        let mut brace_depth: usize = 0;
        let mut pending_test: Option<TestAttr> = None;
        let mut pending_cases: Vec<TestCaseAttr> = Vec::new();
        let mut first_attr_start: Option<usize> = None;

        while self.kind() != K::Eof {
            if brace_depth > 0 {
                match self.kind() {
                    K::LBrace => brace_depth += 1,
                    K::RBrace => brace_depth -= 1,
                    _ => {}
                }
                self.bump();
                continue;
            }

            if self.kind() == K::At && self.at_word(1, "test") {
                let start = self.span_at(0).start;
                first_attr_start.get_or_insert(start);
                let attr = self.parse_test_attr();
                pending_test = Some(attr);
                continue;
            }

            if self.kind() == K::At && self.at_word(1, "test_case") {
                let start = self.span_at(0).start;
                first_attr_start.get_or_insert(start);
                let attr = self.parse_test_case_attr();
                pending_cases.push(attr);
                continue;
            }

            if self.kind() == K::At && self.at_word(1, "fip") {
                // `@fip` may appear before or after `@test` / `@test_case`.
                self.bump();
                self.bump();
                continue;
            }

            if self.at_word(0, "pub") {
                // `pub` may appear before or after `@test` / `@test_case` / `@fip`.
                self.bump();
                continue;
            }

            if self.kind() == K::Fn {
                let fn_tok = self.bump();
                if matches!(self.kind(), K::Lower | K::Upper) {
                    let name_tok = self.bump();
                    let fn_name = self.source[name_tok.span.clone()].to_owned();
                    if pending_test.is_some() || !pending_cases.is_empty() {
                        let start = first_attr_start.unwrap_or(fn_tok.span.start);
                        results.push(FunctionTestAttrs {
                            fn_name,
                            fn_name_span: name_tok.span.clone(),
                            span: start..name_tok.span.end,
                            test: pending_test.take(),
                            test_cases: std::mem::take(&mut pending_cases),
                        });
                        first_attr_start = None;
                    }
                } else {
                    pending_test = None;
                    pending_cases.clear();
                    first_attr_start = None;
                }
                continue;
            }

            // Any other top-level token breaks attribute attachment to a following `fn`.
            if pending_test.is_some() || !pending_cases.is_empty() {
                pending_test = None;
                pending_cases.clear();
                first_attr_start = None;
            }

            match self.kind() {
                K::LBrace => {
                    brace_depth += 1;
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }

        results
    }

    fn parse_test_attr(&mut self) -> TestAttr {
        let at_tok = self.bump(); // `@`
        let kw_tok = self.bump(); // `test`
        let mut attr = TestAttr {
            span: at_tok.span.start..kw_tok.span.end,
            ..TestAttr::default()
        };
        if self.kind() != K::LParen {
            return attr;
        }
        self.bump(); // `(`
        while !matches!(self.kind(), K::RParen | K::Eof) {
            let entry_start = self.span_at(0).start;
            let key = if matches!(self.kind(), K::Lower | K::Upper) {
                let tok = self.bump();
                self.source[tok.span].to_owned()
            } else {
                let span = self.span_at(0);
                self.error("expected attribute key name in @test(...)", span);
                self.skip_to_comma_or_rparen();
                if self.kind() == K::Comma {
                    self.bump();
                }
                continue;
            };
            if self.kind() != K::Eq {
                let span = self.span_at(0);
                self.error(format!("expected '=' after attribute key '{key}'"), span);
                self.skip_to_comma_or_rparen();
                if self.kind() == K::Comma {
                    self.bump();
                }
                continue;
            }
            self.bump(); // `=`
            let (value, val_span) = self.parse_attr_value();
            let entry_span = entry_start..val_span.end;
            self.apply_test_kv(&mut attr, key, value, entry_span);
            if self.kind() == K::Comma {
                self.bump();
            } else if self.kind() != K::RParen {
                let span = self.span_at(0);
                self.error("expected ',' or ')' in @test(...) attribute", span);
                self.skip_to_comma_or_rparen();
                if self.kind() == K::Comma {
                    self.bump();
                }
            }
        }
        if self.kind() == K::RParen {
            let end_tok = self.bump();
            attr.span.end = end_tok.span.end;
        } else {
            self.error("unterminated @test(...) attribute", attr.span.clone());
        }
        attr
    }

    fn apply_test_kv(&mut self, attr: &mut TestAttr, key: String, value: AttrValue, span: Span) {
        match key.as_str() {
            "expect_error" => {
                if let Some(s) = value.as_str() {
                    attr.expect_error = Some(s.to_owned());
                } else {
                    self.error(
                        "expect_error requires an error code identifier or string",
                        span,
                    );
                }
            }
            "primary" => {
                if let Some(s) = value.as_str() {
                    attr.primary = Some(s.to_owned());
                } else {
                    self.error("primary requires a string literal", span);
                }
            }
            "related" => {
                if let Some(s) = value.as_str() {
                    attr.related = Some(s.to_owned());
                } else {
                    self.error("related requires a string literal", span);
                }
            }
            "span" => {
                if let Some(s) = value.as_str() {
                    attr.expected_span = Some(s.to_owned());
                } else {
                    self.error("span requires a string literal", span);
                }
            }
            "cases" => {
                if let Some(n) = value.as_u64() {
                    attr.cases = Some(n);
                } else {
                    self.error("cases requires a non-negative integer", span);
                }
            }
            "sim_schedules" => {
                if let Some(n) = value.as_u64() {
                    attr.sim_schedules = Some(n);
                } else {
                    self.error("sim_schedules requires a non-negative integer", span);
                }
            }
            "fault_profile" => {
                if let Some(s) = value.as_str() {
                    attr.fault_profile = Some(s.to_owned());
                } else {
                    self.error("fault_profile requires an identifier or string", span);
                }
            }
            "max_stack" => {
                if let Some(n) = value.as_u64() {
                    attr.max_stack = Some(n);
                } else {
                    self.error("max_stack requires a non-negative integer", span);
                }
            }
            "max_cycles" => {
                if let Some(n) = value.as_u64() {
                    attr.max_cycles = Some(n);
                } else {
                    self.error("max_cycles requires a non-negative integer", span);
                }
            }
            "zero_alloc" => {
                if let Some(b) = value.as_bool() {
                    attr.zero_alloc = Some(b);
                } else {
                    self.error(
                        "zero_alloc requires a boolean literal (true or false)",
                        span,
                    );
                }
            }
            _ => {
                attr.extra.insert(key, value);
            }
        }
    }

    fn parse_test_case_attr(&mut self) -> TestCaseAttr {
        let at_tok = self.bump(); // `@`
        let kw_tok = self.bump(); // `test_case`
        let mut span = at_tok.span.start..kw_tok.span.end;
        let mut args = Vec::new();
        if self.kind() != K::LParen {
            self.error(
                "@test_case requires an argument list in parentheses",
                span.clone(),
            );
            return TestCaseAttr { args, span };
        }
        self.bump(); // `(`
        while !matches!(self.kind(), K::RParen | K::Eof) {
            let arg_start = self.span_at(0).start;
            let name = if matches!(self.kind(), K::Lower | K::Upper) && self.nth_kind(1) == K::Eq {
                let key_tok = self.bump();
                self.bump(); // `=`
                Some(self.source[key_tok.span].to_owned())
            } else {
                None
            };
            let (value, val_span) = self.parse_attr_value();
            args.push(TestCaseArg {
                name,
                value,
                span: arg_start..val_span.end,
            });
            if self.kind() == K::Comma {
                self.bump();
            } else if self.kind() != K::RParen {
                let err_span = self.span_at(0);
                self.error("expected ',' or ')' in @test_case(...) attribute", err_span);
                self.skip_to_comma_or_rparen();
                if self.kind() == K::Comma {
                    self.bump();
                }
            }
        }
        if self.kind() == K::RParen {
            let end_tok = self.bump();
            span.end = end_tok.span.end;
        } else {
            self.error("unterminated @test_case(...) attribute", span.clone());
        }
        TestCaseAttr { args, span }
    }

    /// Parses one attribute value up to the next top-level `,` or `)`.
    fn parse_attr_value(&mut self) -> (AttrValue, Span) {
        let start_pos = self.pos;
        let start_byte = self.span_at(0).start;

        // Collect tokens belonging to this value at balanced paren/bracket/brace depth 0.
        let mut depth: usize = 0;
        while self.kind() != K::Eof {
            match self.kind() {
                K::Comma | K::RParen if depth == 0 => break,
                K::LParen | K::LBracket | K::LBrace => {
                    depth += 1;
                    self.bump();
                }
                K::RParen | K::RBracket | K::RBrace => {
                    depth = depth.saturating_sub(1);
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }

        let slice = &self.tokens[start_pos..self.pos];
        let end_byte = slice.last().map_or(start_byte, |t| t.span.end);
        let span = start_byte..end_byte;
        let value = match slice {
            [tok] if tok.kind == K::String => {
                AttrValue::String(unescape_string(&self.source[tok.span.clone()]))
            }
            [tok] if tok.kind == K::True => AttrValue::Bool(true),
            [tok] if tok.kind == K::False => AttrValue::Bool(false),
            [tok] if tok.kind == K::Int => parse_int(&self.source[tok.span.clone()], false)
                .map(AttrValue::Int)
                .unwrap_or_else(|| AttrValue::Raw(self.source[span.clone()].to_owned())),
            [neg, tok] if neg.kind == K::Minus && tok.kind == K::Int => {
                parse_int(&self.source[tok.span.clone()], true)
                    .map(AttrValue::Int)
                    .unwrap_or_else(|| AttrValue::Raw(self.source[span.clone()].to_owned()))
            }
            [tok] if tok.kind == K::Float => {
                AttrValue::Float(self.source[tok.span.clone()].to_owned())
            }
            [neg, tok] if neg.kind == K::Minus && tok.kind == K::Float => {
                AttrValue::Float(format!("-{}", &self.source[tok.span.clone()]))
            }
            [tok] if matches!(tok.kind, K::Lower | K::Upper) => {
                AttrValue::Ident(self.source[tok.span.clone()].to_owned())
            }
            _ => AttrValue::Raw(self.source[span.clone()].trim().to_owned()),
        };
        (value, span)
    }

    fn skip_to_comma_or_rparen(&mut self) {
        let mut depth: usize = 0;
        while self.kind() != K::Eof {
            match self.kind() {
                K::Comma | K::RParen if depth == 0 => break,
                K::LParen | K::LBracket | K::LBrace => {
                    depth += 1;
                    self.bump();
                }
                K::RParen | K::RBracket | K::RBrace => {
                    depth = depth.saturating_sub(1);
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
    }
}

fn parse_int(raw: &str, negative: bool) -> Option<i128> {
    let cleaned: String = raw.chars().filter(|&c| c != '_').collect();
    let magnitude = if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        i128::from_str_radix(hex, 16).ok()?
    } else {
        cleaned.parse::<i128>().ok()?
    };
    Some(if negative { -magnitude } else { magnitude })
}

fn unescape_string(raw: &str) -> String {
    let inner = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Item;
    use crate::parser;

    fn assert_parses_cleanly(source: &str) -> parser::Parse {
        let parsed = parser::parse(source);
        assert!(
            parsed.diagnostics.is_empty(),
            "expected zero parse diagnostics, got: {:?}",
            parsed.diagnostics
        );
        parsed
    }

    #[test]
    fn shape_1_deterministic_unit_test() {
        let src = r#"
@test
fn unit_add() -> i64 {
    2 + 2
}

@test
pub fn unit_fallible() -> i64 eff [Fail[RuntimeError]] {
    10 / 2
}
"#;
        let parsed = assert_parses_cleanly(src);
        assert_eq!(parsed.module.public_items, vec!["unit_fallible"]);
        let (attrs, diags) = extract_test_attrs_with_diagnostics(src);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].fn_name, "unit_add");
        assert!(attrs[0].test.is_some());
        assert!(attrs[0].test_cases.is_empty());
        assert_eq!(attrs[1].fn_name, "unit_fallible");
        assert!(attrs[1].test.is_some());
    }

    #[test]
    fn shape_2_parameterized_property_and_test_cases() {
        let src = r#"
@test(cases = 1000)
@test_case(0, 1)
@test_case(x = -42, y = 0xFF, label = "edge\ncase", flag = true)
fn prop_commutative(x: i64, y: i64) -> bool {
    x + y == y + x
}
"#;
        let parsed = assert_parses_cleanly(src);
        assert_eq!(parsed.module.items.len(), 1);
        let (attrs, diags) = extract_test_attrs_with_diagnostics(src);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(attrs.len(), 1);
        let a = &attrs[0];
        assert_eq!(a.fn_name, "prop_commutative");
        let test = a.test.as_ref().expect("@test present");
        assert_eq!(test.cases, Some(1000));
        assert_eq!(a.test_cases.len(), 2);

        assert_eq!(a.test_cases[0].args.len(), 2);
        assert_eq!(a.test_cases[0].args[0].name, None);
        assert_eq!(a.test_cases[0].args[0].value, AttrValue::Int(0));
        assert_eq!(a.test_cases[0].args[1].value, AttrValue::Int(1));

        assert_eq!(a.test_cases[1].args.len(), 4);
        assert_eq!(a.test_cases[1].args[0].name.as_deref(), Some("x"));
        assert_eq!(a.test_cases[1].args[0].value, AttrValue::Int(-42));
        assert_eq!(a.test_cases[1].args[1].name.as_deref(), Some("y"));
        assert_eq!(a.test_cases[1].args[1].value, AttrValue::Int(0xFF));
        assert_eq!(a.test_cases[1].args[2].name.as_deref(), Some("label"));
        assert_eq!(
            a.test_cases[1].args[2].value,
            AttrValue::String("edge\ncase".into())
        );
        assert_eq!(a.test_cases[1].args[3].name.as_deref(), Some("flag"));
        assert_eq!(a.test_cases[1].args[3].value, AttrValue::Bool(true));
    }

    #[test]
    fn shape_3_simpulse_chaos_and_dpor() {
        let src = r#"
@test(sim_schedules = 256, fault_profile = TornWalSector)
fn wal_torn_write_recovery() -> i64 {
    0
}
"#;
        assert_parses_cleanly(src);
        let (attrs, diags) = extract_test_attrs_with_diagnostics(src);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(attrs.len(), 1);
        let test = attrs[0].test.as_ref().expect("@test present");
        assert_eq!(test.sim_schedules, Some(256));
        assert_eq!(test.fault_profile.as_deref(), Some("TornWalSector"));
    }

    #[test]
    fn shape_4_hardware_and_budget_gates_with_fip_and_pub_orderings() {
        let src = r#"
@test(max_stack = 384, max_cycles = 5000, zero_alloc = true)
@fip
fn gate_a(x: u64) -> u64 {
    x
}

@fip
pub @test(max_stack = 128, zero_alloc = false)
fn gate_b(x: u64) -> u64 {
    x
}

pub @test_case(1, 2) @fip @test(cases = 16)
fn gate_c(x: u64, y: u64) -> u64 {
    x + y
}
"#;
        let parsed = assert_parses_cleanly(src);
        assert_eq!(parsed.module.public_items, vec!["gate_b", "gate_c"]);
        for item in &parsed.module.items {
            match item {
                Item::Function(f) => assert!(f.fip, "expected @fip on {}", f.name),
                other => panic!("unexpected item: {other:?}"),
            }
        }
        let (attrs, diags) = extract_test_attrs_with_diagnostics(src);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(attrs.len(), 3);
        let t0 = attrs[0].test.as_ref().unwrap();
        assert_eq!(t0.max_stack, Some(384));
        assert_eq!(t0.max_cycles, Some(5000));
        assert_eq!(t0.zero_alloc, Some(true));

        let t1 = attrs[1].test.as_ref().unwrap();
        assert_eq!(t1.max_stack, Some(128));
        assert_eq!(t1.zero_alloc, Some(false));

        assert_eq!(attrs[2].fn_name, "gate_c");
        assert_eq!(attrs[2].test.as_ref().unwrap().cases, Some(16));
        assert_eq!(attrs[2].test_cases.len(), 1);
    }

    #[test]
    fn shape_5_negative_substructural_expect_error() {
        let src = r#"
@test(expect_error = E0302, primary = "tx", related = "linear value", span = "unconsumed")
fn linear_txn_leak() -> i64 {
    0
}
"#;
        assert_parses_cleanly(src);
        let (attrs, diags) = extract_test_attrs_with_diagnostics(src);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].fn_name, "linear_txn_leak");
        let test = attrs[0].test.as_ref().unwrap();
        assert_eq!(test.expect_error.as_deref(), Some("E0302"));
        assert_eq!(test.primary.as_deref(), Some("tx"));
        assert_eq!(test.related.as_deref(), Some("linear value"));
        assert_eq!(test.expected_span.as_deref(), Some("unconsumed"));
    }

    #[test]
    fn ignores_comments_and_nested_braces_and_rejects_non_fn_target() {
        let src = r#"
// @test(cases = 999)
/* @test_case(1, 2) */
fn helper() -> i64 {
    let s = "@test fn fake() {}";
    0
}

@test
fn real_test() -> i64 {
    helper()
}
"#;
        assert_parses_cleanly(src);
        let attrs = extract_test_attrs(src);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].fn_name, "real_test");

        let bad = "@test\nstruct NotAFn { x: i64 }\n";
        let parsed_bad = parser::parse(bad);
        assert!(
            !parsed_bad.diagnostics.is_empty(),
            "expected diagnostic when @test annotates a struct"
        );
        assert!(extract_test_attrs(bad).is_empty());
    }
}
