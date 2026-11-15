//! Bounded parser for the documented literal `@bench` contract fields.
//!
//! This parses one complete attribute, not arbitrary Vesper source. It does
//! not attach contracts to declarations, obtain measurement evidence, or make
//! any gate decision. Unsupported keys and expressions fail closed.

/// Exact nonnegative decimal: `coefficient / 10^scale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decimal {
    pub coefficient: u64,
    pub scale: u32,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BenchContract {
    pub zero_alloc: Option<bool>,
    pub expect_vectorized: Option<bool>,
    pub max_instructions_per_byte: Option<Decimal>,
    pub stabilizer_pages: Option<u64>,
    pub max_l1d_misses_per_op: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    InputLimit,
    Syntax,
    UnknownField,
    DuplicateField,
    NumberOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ErrorKind,
    pub offset: usize,
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}
impl Parser<'_> {
    fn error(&self, kind: ErrorKind) -> ParseError {
        ParseError {
            kind,
            offset: self.pos,
        }
    }
    fn space(&mut self) {
        while self
            .input
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }
    fn byte(&mut self, byte: u8) -> Result<(), ParseError> {
        self.space();
        if self.input.get(self.pos) != Some(&byte) {
            return Err(self.error(ErrorKind::Syntax));
        }
        self.pos += 1;
        Ok(())
    }
    fn name(&mut self) -> &'_ [u8] {
        self.space();
        let start = self.pos;
        while self
            .input
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        {
            self.pos += 1;
        }
        &self.input[start..self.pos]
    }
    fn boolean(&mut self) -> Result<bool, ParseError> {
        match self.name() {
            b"true" => Ok(true),
            b"false" => Ok(false),
            _ => Err(self.error(ErrorKind::Syntax)),
        }
    }
    fn number(&mut self, fractional: bool) -> Result<Decimal, ParseError> {
        self.space();
        let start = self.pos;
        let mut value = 0u64;
        while let Some(b @ b'0'..=b'9') = self.input.get(self.pos).copied() {
            value = value
                .checked_mul(10)
                .and_then(|n| n.checked_add(u64::from(b - b'0')))
                .ok_or_else(|| self.error(ErrorKind::NumberOverflow))?;
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.error(ErrorKind::Syntax));
        }
        let mut scale = 0u32;
        if fractional && self.input.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            let fraction_start = self.pos;
            while let Some(b @ b'0'..=b'9') = self.input.get(self.pos).copied() {
                scale += 1;
                if scale > 18 {
                    return Err(self.error(ErrorKind::NumberOverflow));
                }
                value = value
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(u64::from(b - b'0')))
                    .ok_or_else(|| self.error(ErrorKind::NumberOverflow))?;
                self.pos += 1;
            }
            if self.pos == fraction_start {
                return Err(self.error(ErrorKind::Syntax));
            }
        }
        Ok(Decimal {
            coefficient: value,
            scale,
        })
    }
}

/// Parse one complete attribute of at most 4096 bytes. ASCII whitespace is
/// accepted; comments, expressions and undocumented options are not.
pub fn parse_attribute(source: &str) -> Result<BenchContract, ParseError> {
    if source.len() > 4096 {
        return Err(ParseError {
            kind: ErrorKind::InputLimit,
            offset: 4096,
        });
    }
    let mut p = Parser {
        input: source.as_bytes(),
        pos: 0,
    };
    p.byte(b'@')?;
    if p.name() != b"bench" {
        return Err(p.error(ErrorKind::Syntax));
    }
    p.space();
    if p.pos == p.input.len() {
        return Ok(BenchContract::default());
    }
    p.byte(b'(')?;
    let mut result = BenchContract::default();
    loop {
        p.space();
        if p.input.get(p.pos) == Some(&b')') {
            p.pos += 1;
            break;
        }
        let start = p.pos;
        let field = p.name();
        let id = match field {
            b"zero_alloc" => 0,
            b"expect_vectorized" => 1,
            b"max_instructions_per_byte" => 2,
            b"stabilizer_pages" => 3,
            b"max_l1d_misses_per_op" => 4,
            b"" => return Err(p.error(ErrorKind::Syntax)),
            _ => {
                return Err(ParseError {
                    kind: ErrorKind::UnknownField,
                    offset: start,
                });
            }
        };
        p.byte(b'=')?;
        macro_rules! set {
            ($field:ident, $value:expr) => {{
                if result.$field.is_some() {
                    return Err(ParseError {
                        kind: ErrorKind::DuplicateField,
                        offset: start,
                    });
                }
                result.$field = Some($value);
            }};
        }
        match id {
            0 => set!(zero_alloc, p.boolean()?),
            1 => set!(expect_vectorized, p.boolean()?),
            2 => set!(max_instructions_per_byte, p.number(true)?),
            3 => set!(stabilizer_pages, p.number(false)?.coefficient),
            4 => set!(max_l1d_misses_per_op, p.number(false)?.coefficient),
            _ => unreachable!(),
        }
        p.space();
        match p.input.get(p.pos) {
            Some(b',') => {
                p.pos += 1;
            }
            Some(b')') => {
                p.pos += 1;
                break;
            }
            _ => return Err(p.error(ErrorKind::Syntax)),
        }
    }
    p.space();
    if p.pos != p.input.len() {
        return Err(p.error(ErrorKind::Syntax));
    }
    Ok(result)
}
