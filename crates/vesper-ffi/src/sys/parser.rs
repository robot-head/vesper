use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Word(String),
    Text(String),
    Number(u64),
    Punct(u8),
    Arrow,
    Eof,
}
#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    span: Range<usize>,
}
fn lex(source: &str, limits: Limits) -> Result<Vec<Token>, Error> {
    if source.len() > limits.source_bytes {
        return Err(Error::new(
            ErrorKind::Limit,
            0..source.len(),
            "source byte budget exceeded",
        ));
    }
    let bytes = source.as_bytes();
    let mut tokens = vec![];
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at..].starts_with(b"//") {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        if tokens.len() >= limits.tokens {
            return Err(Error::new(
                ErrorKind::Limit,
                at..at,
                "token budget exceeded",
            ));
        }
        let start = at;
        let kind = match bytes[at] {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                at += 1;
                while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                    at += 1;
                }
                if at - start > 128 {
                    return Err(Error::new(
                        ErrorKind::Limit,
                        start..at,
                        "identifier byte budget exceeded",
                    ));
                }
                TokenKind::Word(source[start..at].into())
            }
            b'0'..=b'9' => {
                at += 1;
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
                let n = source[start..at].parse().map_err(|_| {
                    Error::new(ErrorKind::Type, start..at, "integer literal exceeds u64")
                })?;
                TokenKind::Number(n)
            }
            b'"' => {
                at += 1;
                let mut s = String::new();
                let mut closed = false;
                while at < bytes.len() {
                    if bytes[at] == b'"' {
                        at += 1;
                        closed = true;
                        break;
                    }
                    let c = if bytes[at] == b'\\' {
                        at += 1;
                        match bytes.get(at) {
                            Some(b'"') => b'"',
                            Some(b'\\') => b'\\',
                            _ => {
                                return Err(Error::new(
                                    ErrorKind::Syntax,
                                    start..at,
                                    "only quote/backslash string escapes are supported",
                                ));
                            }
                        }
                    } else {
                        bytes[at]
                    };
                    if !(0x20..=0x7e).contains(&c) {
                        return Err(Error::new(
                            ErrorKind::Syntax,
                            start..at + 1,
                            "manifest strings must be printable ASCII",
                        ));
                    }
                    if s.len() >= limits.string_bytes {
                        return Err(Error::new(
                            ErrorKind::Limit,
                            start..at,
                            "string byte budget exceeded",
                        ));
                    }
                    s.push(c as char);
                    at += 1;
                }
                if !closed {
                    return Err(Error::new(
                        ErrorKind::Syntax,
                        start..at,
                        "unterminated string",
                    ));
                }
                TokenKind::Text(s)
            }
            b'-' if bytes.get(at + 1) == Some(&b'>') => {
                at += 2;
                TokenKind::Arrow
            }
            c @ (b'{' | b'}' | b'(' | b')' | b'[' | b']' | b',' | b';' | b':' | b'=') => {
                at += 1;
                TokenKind::Punct(c)
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    start..start + 1,
                    "unsupported token (variadics, attributes and imperative code are forbidden)",
                ));
            }
        };
        tokens.push(Token {
            kind,
            span: start..at,
        });
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: at..at,
    });
    Ok(tokens)
}
struct Parser {
    tokens: Vec<Token>,
    at: usize,
    limits: Limits,
}
impl Parser {
    fn token(&self) -> &Token {
        &self.tokens[self.at]
    }
    fn error(&self, kind: ErrorKind, msg: &str) -> Error {
        Error::new(kind, self.token().span.clone(), msg)
    }
    fn bump(&mut self) -> TokenKind {
        let t = self.tokens[self.at].kind.clone();
        if t != TokenKind::Eof {
            self.at += 1;
        }
        t
    }
    fn eat(&mut self, p: u8) -> bool {
        if self.token().kind == TokenKind::Punct(p) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, p: u8) -> Result<(), Error> {
        if self.eat(p) {
            Ok(())
        } else {
            Err(self.error(ErrorKind::Syntax, &format!("expected '{}'", p as char)))
        }
    }
    fn is_word(&self, s: &str) -> bool {
        matches!(&self.token().kind,TokenKind::Word(w) if w==s)
    }
    fn keyword(&mut self, s: &str) -> Result<(), Error> {
        if self.is_word(s) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.error(ErrorKind::Syntax, &format!("expected '{s}'")))
        }
    }
    fn word(&mut self) -> Result<String, Error> {
        if !matches!(self.token().kind, TokenKind::Word(_)) {
            return Err(self.error(ErrorKind::Syntax, "expected identifier"));
        }
        match self.bump() {
            TokenKind::Word(w) => Ok(w),
            _ => unreachable!(),
        }
    }
    fn text(&mut self) -> Result<String, Error> {
        if !matches!(self.token().kind, TokenKind::Text(_)) {
            return Err(self.error(ErrorKind::Syntax, "expected quoted text"));
        }
        match self.bump() {
            TokenKind::Text(w) => Ok(w),
            _ => unreachable!(),
        }
    }
    fn number(&mut self) -> Result<u64, Error> {
        if !matches!(self.token().kind, TokenKind::Number(_)) {
            return Err(self.error(ErrorKind::Syntax, "expected unsigned decimal integer"));
        }
        match self.bump() {
            TokenKind::Number(w) => Ok(w),
            _ => unreachable!(),
        }
    }
    fn enumeration(&mut self) -> Result<EnumDescriptor, Error> {
        self.keyword("enum")?;
        let name = self.word()?;
        reject_reserved_type(&name)?;
        self.expect(b':')?;
        let scalar = scalar(&self.word()?).ok_or_else(|| {
            self.error(
                ErrorKind::Unsupported,
                "enum needs an unsigned fixed-width integer carrier",
            )
        })?;
        if scalar.signed() || scalar == Scalar::Usize {
            return Err(self.error(
                ErrorKind::Unsupported,
                "enum carrier must be u8/u16/u32/u64",
            ));
        }
        self.expect(b'{')?;
        let mut variants = vec![];
        let mut values = BTreeSet::new();
        while !self.eat(b'}') {
            if variants.len() >= self.limits.variants {
                return Err(self.error(ErrorKind::Limit, "enum variant budget exceeded"));
            }
            let name = self.word()?;
            self.expect(b'=')?;
            let value = self.number()?;
            if scalar.bits() < 64 && value >= 1u64 << scalar.bits() {
                return Err(self.error(ErrorKind::Type, "enum discriminant exceeds carrier width"));
            }
            if !values.insert(value) {
                return Err(self.error(ErrorKind::Duplicate, "enum discriminants must be unique"));
            }
            variants.push((name, value));
            if !self.eat(b',') {
                self.expect(b'}')?;
                break;
            }
        }
        if variants.is_empty() {
            return Err(self.error(ErrorKind::Type, "empty enum is unsupported"));
        }
        unique(
            variants.iter().map(|(n, _)| n.as_str()),
            "duplicate enum variant",
        )?;
        variants.sort();
        Ok(EnumDescriptor {
            name,
            scalar,
            variants,
        })
    }
    fn value_type(&mut self) -> Result<RawType, Error> {
        let name = self.word()?;
        if name == "Ptr" || name == "NullablePtr" {
            self.expect(b'[')?;
            let pointee = scalar(&self.word()?).ok_or_else(|| {
                self.error(
                    ErrorKind::Unsupported,
                    "pointer pointee must be an integer scalar",
                )
            })?;
            self.expect(b']')?;
            return Ok(RawType::Address {
                pointee,
                nullable: name == "NullablePtr",
            });
        }
        if matches!(
            name.as_str(),
            "CSlice"
                | "ForeignVal"
                | "Option"
                | "fn"
                | "bool32"
                | "c_long"
                | "long"
                | "char"
                | "f32"
                | "f64"
        ) {
            return Err(self.error(
                ErrorKind::Unsupported,
                "unsupported value layout; use explicit supported carriers",
            ));
        }
        Ok(RawType::Name(name))
    }
    fn function(&mut self) -> Result<RawFunction, Error> {
        self.keyword("fn")?;
        let symbol = self.word()?;
        self.expect(b'(')?;
        let mut parameters = vec![];
        while !self.eat(b')') {
            if parameters.len() >= self.limits.parameters {
                return Err(self.error(ErrorKind::Limit, "function parameter budget exceeded"));
            }
            let borrowed = if self.is_word("nocapture") {
                self.bump();
                let access = match self.word()?.as_str() {
                    "in" => Access::In,
                    "inout" => Access::Inout,
                    _ => {
                        return Err(self.error(
                            ErrorKind::Unsupported,
                            "nocapture must be followed by in or inout",
                        ));
                    }
                };
                Some(access)
            } else {
                None
            };
            let name = self.word()?;
            self.expect(b':')?;
            let ty = if let Some(access) = borrowed {
                self.keyword("CSlice")?;
                self.expect(b'[')?;
                let element = scalar(&self.word()?).ok_or_else(|| {
                    self.error(
                        ErrorKind::Unsupported,
                        "CSlice requires a total-bit integer element",
                    )
                })?;
                self.expect(b']')?;
                RawParameterType::Slice { element, access }
            } else {
                RawParameterType::Value(self.value_type()?)
            };
            parameters.push((name, ty));
            if !self.eat(b',') {
                self.expect(b')')?;
                break;
            }
        }
        unique(
            parameters.iter().map(|(n, _)| n.as_str()),
            "duplicate parameter",
        )?;
        if self.bump() != TokenKind::Arrow {
            return Err(self.error(
                ErrorKind::Syntax,
                "foreign returns must explicitly use -> ForeignVal[T]",
            ));
        }
        self.keyword("ForeignVal")?;
        self.expect(b'[')?;
        let result = self.value_type()?;
        self.expect(b']')?;
        self.expect(b';')?;
        Ok(RawFunction {
            symbol,
            parameters,
            result,
        })
    }
}
#[derive(Clone)]
enum RawType {
    Name(String),
    Address { pointee: Scalar, nullable: bool },
}
enum RawParameterType {
    Value(RawType),
    Slice { element: Scalar, access: Access },
}
struct RawFunction {
    symbol: String,
    parameters: Vec<(String, RawParameterType)>,
    result: RawType,
}
fn scalar(name: &str) -> Option<Scalar> {
    Some(match name {
        "u8" => Scalar::U8,
        "i8" => Scalar::I8,
        "u16" => Scalar::U16,
        "i16" => Scalar::I16,
        "u32" => Scalar::U32,
        "i32" => Scalar::I32,
        "u64" => Scalar::U64,
        "i64" => Scalar::I64,
        "usize" => Scalar::Usize,
        _ => return None,
    })
}
fn reject_reserved_type(n: &str) -> Result<(), Error> {
    if scalar(n).is_some()
        || matches!(
            n,
            "Unit" | "bool" | "CSlice" | "ForeignVal" | "Ptr" | "NullablePtr"
        )
    {
        return Err(Error::new(ErrorKind::Type, 0..0, "reserved type name"));
    }
    Ok(())
}
fn resolve(t: RawType, enums: &[EnumDescriptor]) -> Result<ValueType, Error> {
    match t {
        RawType::Address { pointee, nullable } => Ok(ValueType::Address { pointee, nullable }),
        RawType::Name(n) => {
            if let Some(s) = scalar(&n) {
                Ok(ValueType::Scalar(s))
            } else if n == "Unit" {
                Ok(ValueType::Unit)
            } else if n == "bool" {
                Ok(ValueType::Bool)
            } else if let Some(e) = enums.iter().find(|e| e.name == n) {
                Ok(ValueType::Enum(e.clone()))
            } else {
                Err(Error::new(
                    ErrorKind::Type,
                    0..0,
                    format!("unknown foreign type '{n}'; aggregates/handles are unsupported"),
                ))
            }
        }
    }
}
fn option<'a>(options: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, Error> {
    options.get(key).map(String::as_str).ok_or_else(|| {
        Error::new(
            ErrorKind::Syntax,
            0..0,
            format!("required library option '{key}' is missing"),
        )
    })
}
fn link_name(n: &str) -> bool {
    n.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn header(n: &str) -> bool {
    !n.is_empty()
        && n.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
}
fn pkg_config(n: &str) -> bool {
    let Some((name, version)) = n.split_once(' ') else {
        return false;
    };
    if !link_name(name) {
        return false;
    }
    let version = version.strip_prefix(['=', '^', '~']).unwrap_or("");
    let components: Vec<_> = version.split('.').collect();
    components.len() == 3
        && components.iter().all(|n| {
            !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<u32>().is_ok()
        })
}
pub(super) fn parse(source: &str, limits: Limits) -> Result<LibraryDescriptor, Error> {
    let mut p = Parser {
        tokens: lex(source, limits)?,
        at: 0,
        limits,
    };
    p.keyword("sys")?;
    p.keyword("library")?;
    let name = p.word()?;
    p.expect(b'(')?;
    let mut options = BTreeMap::new();
    while !p.eat(b')') {
        let key = p.word()?;
        if !matches!(
            key.as_str(),
            "target" | "abi" | "link" | "name" | "artifact" | "header" | "pkg_config"
        ) {
            return Err(p.error(ErrorKind::Unsupported, "unsupported library option"));
        }
        p.expect(b'=')?;
        let value = p.text()?;
        if options.insert(key, value).is_some() {
            return Err(p.error(ErrorKind::Duplicate, "duplicate library option"));
        }
        if !p.eat(b',') {
            p.expect(b')')?;
            break;
        }
    }
    let target = match option(&options, "target")? {
        "x86_64-unknown-linux-gnu" => Target::LinuxX86_64,
        "aarch64-unknown-linux-gnu" => Target::LinuxAarch64,
        "aarch64-apple-darwin" => Target::DarwinAarch64,
        "x86_64-pc-windows-msvc" => Target::WindowsX86_64,
        _ => return Err(p.error(ErrorKind::Target, "unsupported explicit target triple")),
    };
    if option(&options, "abi")? != target.convention().name() {
        return Err(p.error(
            ErrorKind::Target,
            "calling convention does not match explicit target",
        ));
    }
    let mode = match option(&options, "link")? {
        "static" => LinkMode::Static,
        "dynamic" => LinkMode::Dynamic,
        _ => return Err(p.error(ErrorKind::Link, "link must be static or dynamic")),
    };
    let library_name = option(&options, "name")?;
    if !link_name(library_name) {
        return Err(p.error(
            ErrorKind::Link,
            "link name must be a bare library name, never flags/paths",
        ));
    }
    if options.get("header").is_some_and(|s| !header(s)) {
        return Err(p.error(
            ErrorKind::Link,
            "header must be a normalized relative include path",
        ));
    }
    if options.get("pkg_config").is_some_and(|s| !pkg_config(s)) {
        return Err(p.error(
            ErrorKind::Link,
            "pkg_config must be 'name =A.B.C', 'name ^A.B.C', or 'name ~A.B.C'",
        ));
    }
    let digest = option(&options, "artifact")?
        .strip_prefix("blake3:")
        .ok_or_else(|| {
            p.error(
                ErrorKind::Link,
                "artifact requires blake3: plus 64 lowercase hex digits",
            )
        })?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(p.error(ErrorKind::Link, "invalid canonical artifact digest"));
    }
    let mut artifact_blake3 = [0u8; 32];
    for (i, b) in artifact_blake3.iter_mut().enumerate() {
        *b = u8::from_str_radix(&digest[i * 2..i * 2 + 2], 16).expect("checked hex");
    }
    let link = LinkDescriptor {
        mode,
        name: library_name.into(),
        artifact_blake3,
        header: options.get("header").cloned(),
        pkg_config: options.get("pkg_config").cloned(),
    };
    p.expect(b'{')?;
    let mut enums = vec![];
    let mut raw_functions = vec![];
    while !p.eat(b'}') {
        if p.is_word("enum") {
            if enums.len() >= limits.enums {
                return Err(p.error(ErrorKind::Limit, "enum declaration budget exceeded"));
            }
            enums.push(p.enumeration()?);
        } else if p.is_word("fn") {
            if raw_functions.len() >= limits.functions {
                return Err(p.error(ErrorKind::Limit, "function declaration budget exceeded"));
            }
            raw_functions.push(p.function()?);
        } else {
            return Err(p.error(
                ErrorKind::Unsupported,
                "expected fn/enum; handles, structs, code and variadics are unsupported",
            ));
        }
    }
    if p.token().kind != TokenKind::Eof {
        return Err(p.error(ErrorKind::Syntax, "trailing declarations after library"));
    }
    if raw_functions.is_empty() {
        return Err(p.error(
            ErrorKind::Type,
            "library must declare at least one function",
        ));
    }
    unique(
        enums.iter().map(|e| e.name.as_str()),
        "duplicate enum declaration",
    )?;
    unique(
        raw_functions.iter().map(|f| f.symbol.as_str()),
        "duplicate foreign symbol",
    )?;
    enums.sort_by(|a, b| a.name.cmp(&b.name));
    let mut functions = vec![];
    for f in raw_functions {
        let mut parameters = vec![];
        for (name, ty) in f.parameters {
            let ty = match ty {
                RawParameterType::Value(t) => {
                    let t = resolve(t, &enums)?;
                    if matches!(t, ValueType::Unit | ValueType::Address { .. }) {
                        return Err(p.error(
                            ErrorKind::Unsupported,
                            "unit/raw-pointer parameters unsupported; use nocapture CSlice",
                        ));
                    }
                    ParameterType::Value(t)
                }
                RawParameterType::Slice { element, access } => {
                    ParameterType::Slice { element, access }
                }
            };
            parameters.push(ParameterDescriptor { name, ty });
        }
        functions.push(FunctionDescriptor {
            symbol: f.symbol,
            parameters,
            result: resolve(f.result, &enums)?,
            convention: target.convention(),
        });
    }
    functions.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    let mut library = LibraryDescriptor {
        name,
        target,
        link,
        enums,
        functions,
        canonical: vec![],
        identity: DescriptorId([0; 32]),
    };
    library.canonical = canonicalize(&library, limits.canonical_bytes)?;
    library.identity = DescriptorId(*blake3::hash(&library.canonical).as_bytes());
    Ok(library)
}
