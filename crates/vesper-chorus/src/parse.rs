//! Textual choreography syntax.
//!
//! ```text
//! program := 'roles' ident (',' ident)* ';' stmt*
//! stmt    := ident '->' ident ':' ident '(' type? ')' ';'      message
//!          | ident '->' ident '{' arm+ '}'                     choice at the sender
//!          | ident ':' ident ';'                               local action
//!          | 'rec' ident '{' stmt* '}'                          recursion
//!          | 'continue' ident ';'                              loop back (last in block)
//!          | 'end' ';'                                         stop (last in block)
//! arm     := ident '(' type? ')' '{' stmt* '}'
//! type    := ident ('::' ident)* ('[' type (',' type)* ']')?
//! ```
//!
//! Statements are sequenced: whatever follows a choice or a `rec` block runs
//! after every branch (or loop exit) that does not itself `continue` or `end`.
//! `//` starts a comment to end of line. Inputs are bounded by
//! [`MAX_SOURCE_BYTES`] and the AST limits in [`crate::chor`]; violations are
//! typed errors.

use crate::chor::{
    Choreography, GBranch, GlobalChor, MAX_DEPTH, MAX_NESTING, MAX_NODES, RoleId, WellFormedError,
};
use std::fmt;

pub const MAX_SOURCE_BYTES: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    TooLong {
        bytes: usize,
    },
    UnexpectedChar(char),
    UnexpectedToken {
        expected: &'static str,
        found: String,
    },
    UnexpectedEof {
        expected: &'static str,
    },
    UnknownRole(String),
    IdentifierTooLong,
    /// A statement follows `continue` or `end` in the same block.
    Unreachable,
    TooDeep,
    TooLarge,
    Invalid(WellFormedError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: u32,
    pub col: u32,
    pub kind: ParseErrorKind,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: ", self.line, self.col)?;
        match &self.kind {
            ParseErrorKind::TooLong { bytes } => {
                write!(f, "source is {bytes} bytes (limit {MAX_SOURCE_BYTES})")
            }
            ParseErrorKind::UnexpectedChar(c) => write!(f, "unexpected character {c:?}"),
            ParseErrorKind::UnexpectedToken { expected, found } => {
                write!(f, "expected {expected}, found `{found}`")
            }
            ParseErrorKind::UnexpectedEof { expected } => {
                write!(f, "expected {expected}, found end of input")
            }
            ParseErrorKind::UnknownRole(r) => write!(f, "role `{r}` is not declared"),
            ParseErrorKind::IdentifierTooLong => write!(f, "identifier too long"),
            ParseErrorKind::Unreachable => {
                write!(f, "statement after `continue` or `end` is unreachable")
            }
            ParseErrorKind::TooDeep => write!(
                f,
                "nesting exceeds {MAX_NESTING} braces or {MAX_DEPTH} sequential steps"
            ),
            ParseErrorKind::TooLarge => write!(f, "choreography expands beyond {MAX_NODES} nodes"),
            ParseErrorKind::Invalid(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Arrow,
    Colon,
    PathSep,
    Semi,
    Comma,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
}

impl Tok {
    fn text(&self) -> String {
        match self {
            Tok::Ident(s) => s.clone(),
            Tok::Arrow => "->".into(),
            Tok::Colon => ":".into(),
            Tok::PathSep => "::".into(),
            Tok::Semi => ";".into(),
            Tok::Comma => ",".into(),
            Tok::LBrace => "{".into(),
            Tok::RBrace => "}".into(),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::LBracket => "[".into(),
            Tok::RBracket => "]".into(),
        }
    }
}

struct Lexed {
    toks: Vec<(Tok, u32, u32)>,
    end: (u32, u32),
}

fn lex(src: &str) -> Result<Lexed, ParseError> {
    let mut toks = Vec::new();
    let (mut line, mut col) = (1u32, 1u32);
    let mut it = src.chars().peekable();
    while let Some(&c) = it.peek() {
        let (l0, c0) = (line, col);
        let err = |kind| ParseError {
            line: l0,
            col: c0,
            kind,
        };
        let mut bump = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
            let ch = it.next();
            if ch == Some('\n') {
                line = line.saturating_add(1);
                col = 1;
            } else {
                col = col.saturating_add(1);
            }
            ch
        };
        if c.is_whitespace() {
            bump(&mut it);
            continue;
        }
        if c == '/' {
            bump(&mut it);
            if it.peek() == Some(&'/') {
                while let Some(&d) = it.peek() {
                    if d == '\n' {
                        break;
                    }
                    bump(&mut it);
                }
                continue;
            }
            return Err(err(ParseErrorKind::UnexpectedChar('/')));
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let mut s = String::new();
            while let Some(&d) = it.peek() {
                if d.is_ascii_alphanumeric() || d == '_' {
                    if s.len() >= crate::chor::MAX_IDENT {
                        return Err(err(ParseErrorKind::IdentifierTooLong));
                    }
                    s.push(d);
                    bump(&mut it);
                } else {
                    break;
                }
            }
            toks.push((Tok::Ident(s), l0, c0));
            continue;
        }
        bump(&mut it);
        let tok = match c {
            '-' if it.peek() == Some(&'>') => {
                bump(&mut it);
                Tok::Arrow
            }
            ':' if it.peek() == Some(&':') => {
                bump(&mut it);
                Tok::PathSep
            }
            ':' => Tok::Colon,
            ';' => Tok::Semi,
            ',' => Tok::Comma,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            other => return Err(err(ParseErrorKind::UnexpectedChar(other))),
        };
        toks.push((tok, l0, c0));
    }
    Ok(Lexed {
        toks,
        end: (line, col),
    })
}

/// Parsed statement before sequencing is resolved.
enum Stmt {
    Msg {
        from: RoleId,
        to: RoleId,
        label: String,
        payload: String,
    },
    Choice {
        from: RoleId,
        to: RoleId,
        arms: Vec<(String, String, Vec<Spanned>)>,
    },
    Local {
        role: RoleId,
        action: String,
    },
    Rec {
        var: String,
        body: Vec<Spanned>,
    },
    Continue(String),
    End,
}

struct Spanned {
    stmt: Stmt,
    line: u32,
    col: u32,
}

struct Parser {
    toks: Vec<(Tok, u32, u32)>,
    pos: usize,
    end: (u32, u32),
    roles: Vec<String>,
}

const KEYWORDS: [&str; 4] = ["roles", "rec", "continue", "end"];

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }
    fn here(&self) -> (u32, u32) {
        self.toks.get(self.pos).map_or(self.end, |t| (t.1, t.2))
    }
    fn fail<T>(&self, kind: ParseErrorKind) -> Result<T, ParseError> {
        let (line, col) = self.here();
        Err(ParseError { line, col, kind })
    }
    fn unexpected<T>(&self, expected: &'static str) -> Result<T, ParseError> {
        match self.peek() {
            Some(t) => self.fail(ParseErrorKind::UnexpectedToken {
                expected,
                found: t.text(),
            }),
            None => self.fail(ParseErrorKind::UnexpectedEof { expected }),
        }
    }
    fn eat(&mut self, tok: &Tok, expected: &'static str) -> Result<(), ParseError> {
        if self.peek() == Some(tok) {
            self.pos += 1;
            Ok(())
        } else {
            self.unexpected(expected)
        }
    }
    fn ident(&mut self, expected: &'static str) -> Result<String, ParseError> {
        match self.peek() {
            Some(Tok::Ident(s)) if !KEYWORDS.contains(&s.as_str()) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => self.unexpected(expected),
        }
    }
    fn keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s == kw)
    }
    fn role(&mut self) -> Result<RoleId, ParseError> {
        let at = self.pos;
        let name = self.ident("role name")?;
        match self.roles.iter().position(|r| *r == name) {
            Some(i) => Ok(RoleId(i as u16)),
            None => {
                self.pos = at;
                self.fail(ParseErrorKind::UnknownRole(name))
            }
        }
    }
    fn ty(&mut self, depth: usize) -> Result<String, ParseError> {
        if depth > 32 {
            return self.fail(ParseErrorKind::TooDeep);
        }
        let mut s = self.ident("type name")?;
        while self.peek() == Some(&Tok::PathSep) {
            self.pos += 1;
            s.push_str("::");
            s.push_str(&self.ident("type name")?);
        }
        if self.peek() == Some(&Tok::LBracket) {
            self.pos += 1;
            s.push('[');
            loop {
                s.push_str(&self.ty(depth + 1)?);
                match self.peek() {
                    Some(Tok::Comma) => {
                        self.pos += 1;
                        s.push_str(", ");
                    }
                    Some(Tok::RBracket) => {
                        self.pos += 1;
                        s.push(']');
                        break;
                    }
                    _ => return self.unexpected("`,` or `]`"),
                }
            }
        }
        if s.len() > 4 * crate::chor::MAX_IDENT {
            return self.fail(ParseErrorKind::IdentifierTooLong);
        }
        Ok(s)
    }
    fn label_payload(&mut self) -> Result<(String, String), ParseError> {
        let label = self.ident("message label")?;
        self.eat(&Tok::LParen, "`(`")?;
        let payload = if self.peek() == Some(&Tok::RParen) {
            String::new()
        } else {
            self.ty(0)?
        };
        self.eat(&Tok::RParen, "`)`")?;
        Ok((label, payload))
    }
    fn block(&mut self, depth: usize, top: bool) -> Result<Vec<Spanned>, ParseError> {
        if depth > MAX_NESTING {
            return self.fail(ParseErrorKind::TooDeep);
        }
        let mut out: Vec<Spanned> = Vec::new();
        loop {
            match self.peek() {
                None if top => return Ok(out),
                None => return self.unexpected("`}`"),
                Some(Tok::RBrace) if !top => return Ok(out),
                _ => {}
            }
            if matches!(
                out.last(),
                Some(Spanned {
                    stmt: Stmt::Continue(_) | Stmt::End,
                    ..
                })
            ) {
                return self.fail(ParseErrorKind::Unreachable);
            }
            let (line, col) = self.here();
            let stmt = self.stmt(depth)?;
            out.push(Spanned { stmt, line, col });
        }
    }
    fn braced(&mut self, depth: usize) -> Result<Vec<Spanned>, ParseError> {
        self.eat(&Tok::LBrace, "`{`")?;
        let body = self.block(depth + 1, false)?;
        self.eat(&Tok::RBrace, "`}`")?;
        Ok(body)
    }
    fn stmt(&mut self, depth: usize) -> Result<Stmt, ParseError> {
        if self.keyword("rec") {
            self.pos += 1;
            let var = self.ident("recursion variable")?;
            let body = self.braced(depth)?;
            return Ok(Stmt::Rec { var, body });
        }
        if self.keyword("continue") {
            self.pos += 1;
            let var = self.ident("recursion variable")?;
            self.eat(&Tok::Semi, "`;`")?;
            return Ok(Stmt::Continue(var));
        }
        if self.keyword("end") {
            self.pos += 1;
            self.eat(&Tok::Semi, "`;`")?;
            return Ok(Stmt::End);
        }
        let from = self.role()?;
        match self.peek() {
            Some(Tok::Colon) => {
                self.pos += 1;
                let action = self.ident("local action name")?;
                self.eat(&Tok::Semi, "`;`")?;
                Ok(Stmt::Local { role: from, action })
            }
            Some(Tok::Arrow) => {
                self.pos += 1;
                let to = self.role()?;
                match self.peek() {
                    Some(Tok::Colon) => {
                        self.pos += 1;
                        let (label, payload) = self.label_payload()?;
                        self.eat(&Tok::Semi, "`;`")?;
                        Ok(Stmt::Msg {
                            from,
                            to,
                            label,
                            payload,
                        })
                    }
                    Some(Tok::LBrace) => {
                        self.pos += 1;
                        let mut arms = Vec::new();
                        while self.peek() != Some(&Tok::RBrace) {
                            if self.peek().is_none() {
                                return self.unexpected("choice arm or `}`");
                            }
                            let (label, payload) = self.label_payload()?;
                            let body = self.braced(depth + 1)?;
                            arms.push((label, payload, body));
                        }
                        self.pos += 1;
                        Ok(Stmt::Choice { from, to, arms })
                    }
                    _ => self.unexpected("`:` or `{`"),
                }
            }
            _ => self.unexpected("`->` or `:`"),
        }
    }
}

struct Builder {
    nodes: usize,
}

impl Builder {
    fn charge(&mut self, n: usize, s: &Spanned) -> Result<(), ParseError> {
        self.nodes = self.nodes.saturating_add(n);
        if self.nodes > MAX_NODES {
            return Err(ParseError {
                line: s.line,
                col: s.col,
                kind: ParseErrorKind::TooLarge,
            });
        }
        Ok(())
    }
    fn deep(&self, s: &Spanned) -> ParseError {
        ParseError {
            line: s.line,
            col: s.col,
            kind: ParseErrorKind::TooDeep,
        }
    }
    /// Right fold of a block onto its continuation `cont` (of tree depth
    /// `cont_depth`). Returns the tree and its depth, which never exceeds
    /// [`MAX_DEPTH`], so later recursive passes and drops stay bounded.
    fn build(
        &mut self,
        stmts: Vec<Spanned>,
        mut cont: GlobalChor,
        mut cont_depth: usize,
    ) -> Result<(GlobalChor, usize), ParseError> {
        for s in stmts.into_iter().rev() {
            self.charge(1, &s)?;
            let mut depth = cont_depth + 1;
            let here = Spanned {
                stmt: Stmt::End,
                line: s.line,
                col: s.col,
            };
            cont = match s.stmt {
                Stmt::Msg {
                    from,
                    to,
                    label,
                    payload,
                } => GlobalChor::Comm {
                    from,
                    to,
                    branches: vec![GBranch {
                        label,
                        payload,
                        next: cont,
                    }],
                },
                Stmt::Local { role, action } => GlobalChor::Local {
                    role,
                    action,
                    next: Box::new(cont),
                },
                Stmt::Continue(v) => {
                    depth = 1;
                    GlobalChor::Var(v)
                }
                Stmt::End => {
                    depth = 1;
                    GlobalChor::End
                }
                Stmt::Rec { var, body } => {
                    let (b, d) = self.build(body, cont, cont_depth)?;
                    depth = d + 1;
                    GlobalChor::Rec {
                        var,
                        body: Box::new(b),
                    }
                }
                Stmt::Choice { from, to, arms } => {
                    // Each arm gets its own copy of the continuation.
                    let size = cont.node_count();
                    self.charge(size.saturating_mul(arms.len().saturating_sub(1)), &here)?;
                    let mut branches = Vec::with_capacity(arms.len());
                    let mut max = 0;
                    for (label, payload, body) in arms {
                        let (next, d) = self.build(body, cont.clone(), cont_depth)?;
                        max = max.max(d);
                        branches.push(GBranch {
                            label,
                            payload,
                            next,
                        });
                    }
                    depth = max + 1;
                    GlobalChor::Comm { from, to, branches }
                }
            };
            if depth > MAX_DEPTH {
                return Err(self.deep(&here));
            }
            cont_depth = depth;
        }
        Ok((cont, cont_depth))
    }
}

/// Parse and check a choreography.
pub fn parse_choreography(src: &str) -> Result<Choreography, ParseError> {
    if src.len() > MAX_SOURCE_BYTES {
        return Err(ParseError {
            line: 1,
            col: 1,
            kind: ParseErrorKind::TooLong { bytes: src.len() },
        });
    }
    let lexed = lex(src)?;
    let mut p = Parser {
        toks: lexed.toks,
        pos: 0,
        end: lexed.end,
        roles: Vec::new(),
    };
    if !p.keyword("roles") {
        return p.unexpected("`roles`");
    }
    p.pos += 1;
    loop {
        let r = p.ident("role name")?;
        if p.roles.len() >= crate::chor::MAX_ROLES {
            return p.fail(ParseErrorKind::Invalid(WellFormedError::TooManyRoles {
                count: p.roles.len() + 1,
            }));
        }
        if p.roles.contains(&r) {
            return p.fail(ParseErrorKind::Invalid(WellFormedError::DuplicateRole {
                name: r,
            }));
        }
        p.roles.push(r);
        match p.peek() {
            Some(Tok::Comma) => p.pos += 1,
            Some(Tok::Semi) => {
                p.pos += 1;
                break;
            }
            _ => return p.unexpected("`,` or `;`"),
        }
    }
    let stmts = p.block(0, true)?;
    let (body, _) = Builder { nodes: 0 }.build(stmts, GlobalChor::End, 1)?;
    Choreography::new(p.roles, body).map_err(|e| ParseError {
        line: 1,
        col: 1,
        kind: ParseErrorKind::Invalid(e),
    })
}
