use crate::{
    add, bound, identifier, ColumnDef, Error, ErrorKind, JoinKind, Limits, Result, ScalarType,
    Value,
};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(String),
    Param(String),
    Int(i64),
    Text(String),
    Sym(char),
    Op(Cmp),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
#[derive(Clone, Debug)]
pub(crate) enum Expr {
    Column(Option<String>, String),
    Param(String),
    Literal(Value),
    Compare(Cmp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    IsNull(Box<Expr>, bool),
}
pub(crate) struct Selection {
    pub expr: Option<Expr>,
    pub alias: Option<String>,
}
pub(crate) struct Table {
    pub name: String,
    pub alias: String,
}
pub(crate) struct Join {
    pub kind: JoinKind,
    pub rhs: Table,
    pub on: Expr,
}
pub(crate) struct Select {
    pub projection: Vec<Selection>,
    pub from: Table,
    pub joins: Vec<Join>,
    pub filter: Option<Expr>,
}
pub(crate) enum Ddl {
    Create(String, Vec<ColumnDef>),
    Drop(String),
}

fn lex(sql: &str, limits: Limits) -> Result<Vec<Token>> {
    bound(sql.len(), limits.sql_bytes, "SQL byte limit")?;
    let b = sql.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        bound(add(out.len(), 1)?, limits.tokens, "SQL token limit")?;
        let start = i;
        let token = match b[i] {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b':' => {
                let param = b[i] == b':';
                if param {
                    i += 1;
                }
                let word_start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                let name = identifier(&sql[word_start..i])?;
                if param {
                    Token::Param(name)
                } else {
                    Token::Word(name)
                }
            }
            b'0'..=b'9' | b'-' => {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                Token::Int(sql[start..i].parse().map_err(|_| {
                    Error::new(ErrorKind::Syntax, "invalid or overflowing i64 literal")
                })?)
            }
            b'\'' => {
                i += 1;
                let mut value = String::new();
                let mut part = i;
                loop {
                    if i == b.len() {
                        return Err(Error::new(ErrorKind::Syntax, "unterminated string"));
                    }
                    if b[i] == b'\'' {
                        value.push_str(&sql[part..i]);
                        i += 1;
                        if i < b.len() && b[i] == b'\'' {
                            value.push('\'');
                            i += 1;
                            part = i;
                        } else {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                Token::Text(value)
            }
            b'=' => {
                i += 1;
                Token::Op(Cmp::Eq)
            }
            b'<' | b'>' | b'!' => {
                let first = b[i];
                i += 1;
                let second = b.get(i).copied();
                let op = match (first, second) {
                    (b'<', Some(b'=')) => {
                        i += 1;
                        Cmp::Le
                    }
                    (b'>', Some(b'=')) => {
                        i += 1;
                        Cmp::Ge
                    }
                    (b'<', Some(b'>')) | (b'!', Some(b'=')) => {
                        i += 1;
                        Cmp::Ne
                    }
                    (b'<', _) => Cmp::Lt,
                    (b'>', _) => Cmp::Gt,
                    _ => return Err(Error::new(ErrorKind::Syntax, "unsupported operator")),
                };
                Token::Op(op)
            }
            b'(' | b')' | b',' | b'.' | b';' | b'*' => {
                i += 1;
                Token::Sym(b[start] as char)
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::Syntax,
                    format!("unsupported token at byte {i}"),
                ))
            }
        };
        out.push(token);
    }
    Ok(out)
}
struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    nodes: usize,
    limits: Limits,
}
impl Parser {
    fn new(sql: &str, limits: Limits) -> Result<Self> {
        Ok(Self {
            tokens: lex(sql, limits)?,
            pos: 0,
            nodes: 0,
            limits,
        })
    }
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }
    fn take(&mut self) -> Result<Token> {
        let t = self
            .peek()
            .cloned()
            .ok_or_else(|| self.syntax("unexpected end"))?;
        self.pos += 1;
        Ok(t)
    }
    fn syntax(&self, msg: &str) -> Error {
        Error::new(ErrorKind::Syntax, format!("{msg} near token {}", self.pos))
    }
    fn word(&mut self, s: &str) -> bool {
        if self.peek() == Some(&Token::Word(s.into())) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn sym(&mut self, s: char) -> bool {
        if self.peek() == Some(&Token::Sym(s)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_word(&mut self, s: &str) -> Result<()> {
        if self.word(s) {
            Ok(())
        } else {
            Err(self.syntax(&format!("expected {s}")))
        }
    }
    fn expect_sym(&mut self, s: char) -> Result<()> {
        if self.sym(s) {
            Ok(())
        } else {
            Err(self.syntax(&format!("expected {s}")))
        }
    }
    fn ident(&mut self) -> Result<String> {
        match self.take()? {
            Token::Word(w) if !reserved(&w) => Ok(w),
            _ => Err(self.syntax("expected non-keyword identifier")),
        }
    }
    fn node(&mut self, expr: Expr) -> Result<Expr> {
        self.nodes = add(self.nodes, 1)?;
        bound(self.nodes, self.limits.nodes, "expression node limit")?;
        Ok(expr)
    }
    fn expr(&mut self, depth: usize) -> Result<Expr> {
        bound(depth, self.limits.nesting, "expression nesting limit")?;
        let mut lhs = self.and(depth)?;
        while self.word("or") {
            let rhs = self.and(depth)?;
            lhs = self.node(Expr::Or(Box::new(lhs), Box::new(rhs)))?;
        }
        Ok(lhs)
    }
    fn and(&mut self, depth: usize) -> Result<Expr> {
        let mut lhs = self.not(depth)?;
        while self.word("and") {
            let rhs = self.not(depth)?;
            lhs = self.node(Expr::And(Box::new(lhs), Box::new(rhs)))?;
        }
        Ok(lhs)
    }
    fn not(&mut self, depth: usize) -> Result<Expr> {
        bound(depth, self.limits.nesting, "expression nesting limit")?;
        if self.word("not") {
            let x = self.not(depth + 1)?;
            return self.node(Expr::Not(Box::new(x)));
        }
        let lhs = self.atom(depth)?;
        if let Some(Token::Op(op)) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.atom(depth)?;
            self.node(Expr::Compare(op, Box::new(lhs), Box::new(rhs)))
        } else if self.word("is") {
            let negated = self.word("not");
            self.expect_word("null")?;
            self.node(Expr::IsNull(Box::new(lhs), negated))
        } else {
            Ok(lhs)
        }
    }
    fn atom(&mut self, depth: usize) -> Result<Expr> {
        if self.sym('(') {
            let x = self.expr(depth + 1)?;
            self.expect_sym(')')?;
            return Ok(x);
        }
        let expr = match self.take()? {
            Token::Param(w) => Expr::Param(w),
            Token::Int(i) => Expr::Literal(Value::I64(i)),
            Token::Text(s) => Expr::Literal(Value::Text(s)),
            Token::Word(w) if w == "null" => Expr::Literal(Value::Null),
            Token::Word(w) if w == "true" || w == "false" => {
                Expr::Literal(Value::Bool(w == "true"))
            }
            Token::Word(w) if !reserved(&w) => {
                if self.sym('.') {
                    Expr::Column(Some(w), self.ident()?)
                } else {
                    Expr::Column(None, w)
                }
            }
            _ => return Err(self.syntax("expected scalar expression")),
        };
        self.node(expr)
    }
    fn table(&mut self) -> Result<Table> {
        let name = self.ident()?;
        let alias =
            if self.word("as") || matches!(self.peek(), Some(Token::Word(w)) if !reserved(w)) {
                self.ident()?
            } else {
                name.clone()
            };
        Ok(Table { name, alias })
    }
    fn finish(&mut self) -> Result<()> {
        self.sym(';');
        if self.peek().is_some() {
            Err(self.syntax("unsupported or trailing syntax"))
        } else {
            Ok(())
        }
    }
}
fn reserved(w: &str) -> bool {
    matches!(
        w,
        "select"
            | "from"
            | "where"
            | "join"
            | "inner"
            | "left"
            | "right"
            | "full"
            | "outer"
            | "on"
            | "as"
            | "and"
            | "or"
            | "not"
            | "is"
            | "null"
            | "true"
            | "false"
            | "create"
            | "drop"
            | "table"
            | "order"
            | "by"
            | "group"
            | "limit"
            | "union"
            | "distinct"
            | "having"
            | "offset"
            | "cross"
            | "using"
    )
}

pub(crate) fn select(sql: &str, limits: Limits) -> Result<Select> {
    let mut p = Parser::new(sql, limits)?;
    p.expect_word("select")?;
    let mut projection = Vec::new();
    loop {
        let expr = if p.sym('*') { None } else { Some(p.expr(0)?) };
        let alias = if p.word("as") { Some(p.ident()?) } else { None };
        if expr.is_none() && alias.is_some() {
            return Err(p.syntax("star cannot be aliased"));
        }
        projection.push(Selection { expr, alias });
        bound(projection.len(), limits.nodes, "projection limit")?;
        if !p.sym(',') {
            break;
        }
    }
    p.expect_word("from")?;
    let from = p.table()?;
    bound(1, limits.relations, "relation limit")?;
    let mut joins = Vec::new();
    while p.peek().is_some()
        && p.peek() != Some(&Token::Sym(';'))
        && p.peek() != Some(&Token::Word("where".into()))
    {
        let kind = if p.word("inner") {
            JoinKind::Inner
        } else if p.word("left") {
            p.word("outer");
            JoinKind::Left
        } else if p.word("right") {
            p.word("outer");
            JoinKind::Right
        } else if p.word("full") {
            p.word("outer");
            JoinKind::Full
        } else {
            JoinKind::Inner
        };
        p.expect_word("join")?;
        bound(add(joins.len(), 2)?, limits.relations, "relation limit")?;
        let rhs = p.table()?;
        p.expect_word("on")?;
        let on = p.expr(0)?;
        joins.push(Join { kind, rhs, on });
    }
    let filter = if p.word("where") {
        Some(p.expr(0)?)
    } else {
        None
    };
    p.finish()?;
    Ok(Select {
        projection,
        from,
        joins,
        filter,
    })
}

pub(crate) fn ddl(sql: &str, limits: Limits) -> Result<Vec<Ddl>> {
    let mut p = Parser::new(sql, limits)?;
    let mut out = Vec::new();
    while p.peek().is_some() {
        bound(add(out.len(), 1)?, limits.nodes, "DDL statement limit")?;
        if p.word("create") {
            p.expect_word("table")?;
            let name = p.ident()?;
            p.expect_sym('(')?;
            let mut cols = Vec::new();
            let mut names = BTreeSet::new();
            loop {
                let col = p.ident()?;
                if !names.insert(col.clone()) {
                    return Err(Error::new(ErrorKind::Duplicate, format!("column {col}")));
                }
                let ty = match p.take()? {
                    Token::Word(w) => match w.as_str() {
                        "u64" => ScalarType::U64,
                        "i64" | "bigint" => ScalarType::I64,
                        "f64" | "double" => ScalarType::F64,
                        "bool" | "boolean" => ScalarType::Bool,
                        "text" => ScalarType::Text,
                        "bytes" | "blob" => ScalarType::Bytes,
                        _ => return Err(p.syntax("unsupported scalar type")),
                    },
                    _ => return Err(p.syntax("expected scalar type")),
                };
                let nullable = if p.word("not") {
                    p.expect_word("null")?;
                    false
                } else {
                    p.word("null");
                    true
                };
                bound(
                    add(cols.len(), 1)?,
                    limits.columns_per_table,
                    "column limit",
                )?;
                cols.push(ColumnDef {
                    name: col,
                    ty,
                    nullable,
                });
                if !p.sym(',') {
                    break;
                }
            }
            p.expect_sym(')')?;
            out.push(Ddl::Create(name, cols));
        } else if p.word("drop") {
            p.expect_word("table")?;
            out.push(Ddl::Drop(p.ident()?));
        } else {
            return Err(p.syntax("only CREATE TABLE and DROP TABLE are supported"));
        }
        if !p.sym(';') && p.peek().is_some() {
            return Err(p.syntax("expected statement separator"));
        }
    }
    Ok(out)
}
