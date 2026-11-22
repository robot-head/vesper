use crate::{Error, Limits};
use std::collections::BTreeSet;

// This deliberately finite supported SPDX subset is not a registry lookup and
// does not assert that an unknown identifier is a valid SPDX license.
const LICENSES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "MPL-2.0",
    "GPL-2.0-only",
    "GPL-2.0-or-later",
    "GPL-3.0-only",
    "GPL-3.0-or-later",
    "LGPL-2.1-only",
    "LGPL-2.1-or-later",
    "LGPL-3.0-only",
    "LGPL-3.0-or-later",
    "AGPL-3.0-only",
    "AGPL-3.0-or-later",
    "Unlicense",
    "Zlib",
    "BSL-1.0",
    "CC0-1.0",
];
const EXCEPTIONS: &[&str] = &[
    "LLVM-exception",
    "Classpath-exception-2.0",
    "GCC-exception-3.1",
];

pub(crate) enum Failure {
    Unknown(String),
    Limit(String),
}
impl From<&str> for Failure {
    fn from(s: &str) -> Self {
        Self::Unknown(s.into())
    }
}
impl From<String> for Failure {
    fn from(s: String) -> Self {
        Self::Unknown(s)
    }
}

#[derive(Debug)]
enum Expr {
    Grant(String),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

impl Expr {
    fn permitted(&self, allow: &BTreeSet<String>) -> bool {
        match self {
            Self::Grant(s) => allow.contains(s),
            Self::And(a, b) => a.permitted(allow) && b.permitted(allow),
            Self::Or(a, b) => a.permitted(allow) || b.permitted(allow),
        }
    }
}

struct Parser<'a> {
    tokens: Vec<&'a str>,
    pos: usize,
    max_depth: usize,
}
impl Parser<'_> {
    fn take(&mut self, token: &str) -> bool {
        if self.tokens.get(self.pos) == Some(&token) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expression(&mut self, depth: usize) -> Result<Expr, Failure> {
        let mut left = self.conjunction(depth)?;
        while self.take("OR") {
            left = Expr::Or(Box::new(left), Box::new(self.conjunction(depth)?));
        }
        Ok(left)
    }
    fn conjunction(&mut self, depth: usize) -> Result<Expr, Failure> {
        let mut left = self.atom(depth)?;
        while self.take("AND") {
            left = Expr::And(Box::new(left), Box::new(self.atom(depth)?));
        }
        Ok(left)
    }
    fn atom(&mut self, depth: usize) -> Result<Expr, Failure> {
        if depth > self.max_depth {
            return Err(Failure::Limit("license nesting budget exhausted".into()));
        }
        if self.take("(") {
            let value = self.expression(depth + 1)?;
            if !self.take(")") {
                return Err("missing closing license parenthesis".into());
            }
            return Ok(value);
        }
        let license = self
            .tokens
            .get(self.pos)
            .ok_or("expected SPDX license identifier")?;
        if !LICENSES.contains(license) {
            return Err(format!("unknown/unsupported SPDX license identifier: {license}").into());
        }
        self.pos += 1;
        let mut grant = (*license).to_string();
        if self.take("WITH") {
            let exception = self
                .tokens
                .get(self.pos)
                .ok_or("expected SPDX exception identifier")?;
            if !EXCEPTIONS.contains(exception) {
                return Err(format!("unknown/unsupported SPDX exception: {exception}").into());
            }
            self.pos += 1;
            grant.push_str(" WITH ");
            grant.push_str(exception);
        }
        Ok(Expr::Grant(grant))
    }
}

fn parse(text: &str, limits: &Limits) -> Result<Expr, Failure> {
    if text.len() > limits.max_string_bytes {
        return Err(Failure::Limit("license byte budget exhausted".into()));
    }
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, byte) in text.bytes().enumerate() {
        if byte.is_ascii_whitespace() || matches!(byte, b'(' | b')') {
            if let Some(start) = start.take() {
                tokens.push(&text[start..index]);
            }
            if byte == b'(' || byte == b')' {
                tokens.push(&text[index..index + 1]);
            }
        } else if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.') {
            start.get_or_insert(index);
        } else {
            return Err("unsupported/freeform license syntax".into());
        }
        if tokens.len() > limits.max_license_tokens {
            return Err(Failure::Limit("license token budget exhausted".into()));
        }
    }
    if let Some(start) = start {
        tokens.push(&text[start..]);
    }
    if tokens.len() > limits.max_license_tokens {
        return Err(Failure::Limit("license token budget exhausted".into()));
    }
    let mut parser = Parser {
        tokens,
        pos: 0,
        max_depth: limits.max_license_depth,
    };
    let result = parser.expression(0)?;
    if parser.pos != parser.tokens.len() {
        return Err("unexpected trailing license token".into());
    }
    Ok(result)
}

pub(crate) fn allowlist(values: &[String], limits: &Limits) -> Result<BTreeSet<String>, Error> {
    let mut result = BTreeSet::new();
    for text in values {
        let Expr::Grant(grant) = parse(text, limits).map_err(|failure| match failure {
            Failure::Unknown(reason) => Error::InvalidPolicy(reason),
            Failure::Limit(reason) => Error::Limit(reason),
        })?
        else {
            return Err(Error::InvalidPolicy(
                "allow entries must be single licenses or explicit license WITH exception grants"
                    .into(),
            ));
        };
        if !result.insert(grant) {
            return Err(Error::InvalidPolicy("duplicate license grant".into()));
        }
    }
    Ok(result)
}

pub(crate) fn assess(
    text: Option<&str>,
    allow: &BTreeSet<String>,
    limits: &Limits,
) -> Result<bool, Failure> {
    let text = text.ok_or("license declaration absent")?;
    // Parse every branch first. MIT OR an unknown token cannot silently pass.
    Ok(parse(text, limits)?.permitted(allow))
}
