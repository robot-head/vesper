//! Parser for the Forge lexical fragment v1 over host tokens.
//!
//! ```text
//! expr    := 'let' IDENT '=' expr ';' expr
//!          | 'handle' path 'with' path block
//!          | postfix
//! postfix := primary ( '(' [ expr { ',' expr } ] ')' )*
//! primary := path | INT | STRING | '(' expr ')' | block
//! block   := '{' [ expr { ';' expr } [';'] ] '}'
//! path    := IDENT { '::' IDENT }
//! ```
//!
//! `let`, `handle` and `with` are reserved. A `let` scopes its binder over the
//! following expression only (not over its own initializer).

use serde::{Deserialize, Serialize};

use crate::error::ForgeError;
use crate::host::HygieneHost;
use crate::identity::Symbol;
use crate::token::{Delim, HostToken, TokenKind};

/// A path: one or more identifier tokens; its symbol is the `::`-joined text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Path {
    /// Indices of the identifier tokens (and `::` separators are implied).
    pub tokens: Vec<usize>,
    pub symbol: Symbol,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Term {
    Ref(Path),
    Lit(usize),
    Let {
        binder: usize,
        value: Box<Term>,
        body: Box<Term>,
    },
    Call {
        callee: Box<Term>,
        /// Token index of the opening parenthesis.
        open: usize,
        args: Vec<Term>,
    },
    Handle {
        keyword: usize,
        effect: Path,
        handler: Path,
        body: Box<Term>,
    },
    Block(Vec<Term>),
}

const KEYWORDS: [&str; 3] = ["let", "handle", "with"];

pub(crate) fn parse(host: &mut HygieneHost, tokens: &[HostToken]) -> Result<Term, ForgeError> {
    let max_depth = host.quote_limits.max_depth;
    let mut p = Parser {
        host,
        tokens,
        pos: 0,
        depth: 0,
        max_depth,
    };
    let t = p.expr()?;
    if p.pos != tokens.len() {
        return Err(p.error("end of input"));
    }
    Ok(t)
}

struct Parser<'a> {
    host: &'a mut HygieneHost,
    tokens: &'a [HostToken],
    pos: usize,
    depth: usize,
    max_depth: usize,
}

impl Parser<'_> {
    fn error(&self, expected: &'static str) -> ForgeError {
        ForgeError::Parse {
            token: self.pos,
            expected,
        }
    }

    fn peek_text(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(|t| self.host.text(t.text))
    }

    fn peek_kind(&self) -> Option<TokenKind> {
        self.tokens.get(self.pos).map(|t| t.kind)
    }

    fn is_punct(&self, s: &str) -> bool {
        self.peek_kind() == Some(TokenKind::Punct) && self.peek_text() == Some(s)
    }

    fn is_keyword(&self, s: &str) -> bool {
        self.peek_kind() == Some(TokenKind::Ident) && self.peek_text() == Some(s)
    }

    fn expect_punct(&mut self, s: &'static str) -> Result<usize, ForgeError> {
        if self.is_punct(s) {
            self.pos += 1;
            Ok(self.pos - 1)
        } else {
            Err(self.error(s))
        }
    }

    fn expect_kind(&mut self, k: TokenKind, what: &'static str) -> Result<usize, ForgeError> {
        if self.peek_kind() == Some(k) {
            self.pos += 1;
            Ok(self.pos - 1)
        } else {
            Err(self.error(what))
        }
    }

    fn ident(&mut self) -> Result<usize, ForgeError> {
        if self.peek_kind() == Some(TokenKind::Ident)
            && !KEYWORDS.contains(&self.peek_text().unwrap_or(""))
        {
            self.pos += 1;
            Ok(self.pos - 1)
        } else {
            Err(self.error("identifier"))
        }
    }

    fn enter(&mut self) -> Result<(), ForgeError> {
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(ForgeError::QuoteLimit {
                what: "nesting depth",
                limit: self.max_depth,
            });
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<Term, ForgeError> {
        self.enter()?;
        let r = self.expr_inner();
        self.depth -= 1;
        r
    }

    fn expr_inner(&mut self) -> Result<Term, ForgeError> {
        if self.is_keyword("let") {
            self.pos += 1;
            let binder = self.ident()?;
            self.expect_punct("=")?;
            let value = self.expr()?;
            self.expect_punct(";")?;
            let body = self.expr()?;
            return Ok(Term::Let {
                binder,
                value: Box::new(value),
                body: Box::new(body),
            });
        }
        if self.is_keyword("handle") {
            let keyword = self.pos;
            self.pos += 1;
            let effect = self.path()?;
            if !self.is_keyword("with") {
                return Err(self.error("with"));
            }
            self.pos += 1;
            let handler = self.path()?;
            let body = self.block()?;
            return Ok(Term::Handle {
                keyword,
                effect,
                handler,
                body: Box::new(body),
            });
        }
        let mut t = self.primary()?;
        while self.peek_kind() == Some(TokenKind::Open(Delim::Paren)) {
            let open = self.pos;
            self.pos += 1;
            let mut args = Vec::new();
            if self.peek_kind() != Some(TokenKind::Close(Delim::Paren)) {
                loop {
                    args.push(self.expr()?);
                    if self.is_punct(",") {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            self.expect_kind(TokenKind::Close(Delim::Paren), ")")?;
            t = Term::Call {
                callee: Box::new(t),
                open,
                args,
            };
        }
        Ok(t)
    }

    fn primary(&mut self) -> Result<Term, ForgeError> {
        match self.peek_kind() {
            Some(TokenKind::Int | TokenKind::Str) => {
                self.pos += 1;
                Ok(Term::Lit(self.pos - 1))
            }
            Some(TokenKind::Open(Delim::Paren)) => {
                self.pos += 1;
                let e = self.expr()?;
                self.expect_kind(TokenKind::Close(Delim::Paren), ")")?;
                Ok(e)
            }
            Some(TokenKind::Open(Delim::Brace)) => self.block(),
            _ => Ok(Term::Ref(self.path()?)),
        }
    }

    fn block(&mut self) -> Result<Term, ForgeError> {
        self.expect_kind(TokenKind::Open(Delim::Brace), "{")?;
        let mut items = Vec::new();
        while self.peek_kind() != Some(TokenKind::Close(Delim::Brace)) {
            items.push(self.expr()?);
            if self.is_punct(";") {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.expect_kind(TokenKind::Close(Delim::Brace), "}")?;
        Ok(Term::Block(items))
    }

    fn path(&mut self) -> Result<Path, ForgeError> {
        let mut toks = vec![self.ident()?];
        while self.is_punct("::") {
            self.pos += 1;
            toks.push(self.ident()?);
        }
        let text = toks
            .iter()
            .map(|&i| self.host.text(self.tokens[i].text))
            .collect::<Vec<_>>()
            .join("::");
        let symbol = self.host.intern(&text)?;
        Ok(Path {
            tokens: toks,
            symbol,
        })
    }
}
