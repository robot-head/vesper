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
        Ok(EnumDescriptor::new(name, scalar, variants))
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
    fn peek_word(&self, offset: usize) -> Option<&str> {
        match &self.tokens.get(self.at + offset)?.kind {
            TokenKind::Word(w) => Some(w),
            _ => None,
        }
    }
    /// `wire struct Name { field: type, ... }`.
    fn wire_struct(&mut self) -> Result<RawWireStruct, Error> {
        let start = self.token().span.start;
        self.keyword("wire")?;
        self.keyword("struct")?;
        let name = self.word()?;
        reject_reserved_type(&name)?;
        self.expect(b'{')?;
        let mut fields = vec![];
        while !self.eat(b'}') {
            if fields.len() >= self.limits.wire_fields {
                return Err(self.error(ErrorKind::Limit, "wire struct field budget exceeded"));
            }
            let field = self.word()?;
            self.expect(b':')?;
            let ty = self.wire_field_type()?;
            fields.push(crate::Field::new(field, ty));
            if !self.eat(b',') {
                self.expect(b'}')?;
                break;
            }
        }
        if fields.is_empty() {
            return Err(self.error(ErrorKind::Type, "empty wire struct is unsupported"));
        }
        unique(
            fields.iter().map(|f| f.name.as_str()),
            "duplicate wire struct field",
        )?;
        Ok(RawWireStruct {
            declaration: crate::Declaration::new(name, fields),
            span: start..self.tokens[self.at.saturating_sub(1)].span.end,
        })
    }
    fn wire_field_type(&mut self) -> Result<crate::WireType, Error> {
        use crate::{Endian, Integer, WireType};
        let name = self.word()?;
        if name == "ByteArray" || name == "ZeroPad" {
            self.expect(b'[')?;
            let n = self.number()?;
            self.expect(b']')?;
            let n = usize::try_from(n)
                .ok()
                .filter(|n| *n > 0 && *n <= MAX_WIRE_STRUCT_BYTES)
                .ok_or_else(|| {
                    self.error(
                        ErrorKind::Limit,
                        "byte array length must be 1..=1048576 bytes",
                    )
                })?;
            return Ok(if name == "ByteArray" {
                WireType::ByteArray(n)
            } else {
                WireType::ZeroPad(n)
            });
        }
        let (base, endian) = if let Some(b) = name.strip_suffix("be") {
            (b, Endian::Big)
        } else if let Some(b) = name.strip_suffix("le") {
            (b, Endian::Little)
        } else {
            (name.as_str(), Endian::Native)
        };
        if let Some(s) = scalar(base) {
            if s == Scalar::Usize {
                return Err(self.error(
                    ErrorKind::Unsupported,
                    "wire struct fields use fixed-width integers, not usize",
                ));
            }
            if endian != Endian::Native && s.bits() == 8 {
                return Err(self.error(
                    ErrorKind::Unsupported,
                    "8-bit wire integers have no byte order; write u8/i8",
                ));
            }
            let bits = u16::from(s.bits());
            return Ok(WireType::Integer(if s.signed() {
                Integer::signed(bits, endian)
            } else {
                Integer::unsigned(bits, endian)
            }));
        }
        if matches!(
            name.as_str(),
            "bool"
                | "Unit"
                | "CSlice"
                | "ForeignVal"
                | "Option"
                | "Ptr"
                | "NullablePtr"
                | "f32"
                | "f64"
                | "char"
                | "long"
                | "c_long"
        ) {
            return Err(self.error(
                ErrorKind::Unsupported,
                "wire struct fields must admit every bit pattern: fixed-width integers, ByteArray[N], ZeroPad[N] or wire structs",
            ));
        }
        Ok(WireType::Struct(name))
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
            let span_start = self.token().span.start;
            let nocapture = if self.is_word("nocapture") {
                self.bump();
                true
            } else {
                false
            };
            // An access keyword is followed by the parameter name; a parameter
            // that is itself named `in` is followed by ':'.
            let access = match self.peek_word(0) {
                Some(w @ ("in" | "out" | "inout")) if self.peek_word(1).is_some() => {
                    let a = match w {
                        "in" => PointerAccess::In,
                        "out" => PointerAccess::Out,
                        _ => PointerAccess::Inout,
                    };
                    self.bump();
                    Some(a)
                }
                _ => None,
            };
            let access = match (nocapture, access) {
                (true, Some(a)) => Some(a),
                (true, None) => {
                    return Err(self.error(
                        ErrorKind::Unsupported,
                        "nocapture must be followed by in, out or inout",
                    ));
                }
                (false, Some(a)) => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        span_start..self.token().span.end,
                        format!(
                            "borrowed `{}` pointer parameters must be declared `nocapture`; capturing foreign pointers are unsupported",
                            a.keyword()
                        ),
                    ));
                }
                (false, None) => None,
            };
            let name = self.word()?;
            self.expect(b':')?;
            let ty = match access {
                Some(access) => self.borrowed_type(access)?,
                None => RawParameterType::Value(self.value_type()?),
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
    /// The type after `nocapture <access> name:`: a slice, a single pointee,
    /// or an explicitly nullable `Option[...]` of either.
    fn borrowed_type(&mut self, access: PointerAccess) -> Result<RawParameterType, Error> {
        let nullable = if self.is_word("Option") {
            self.bump();
            self.expect(b'[')?;
            true
        } else {
            false
        };
        let ty = if self.is_word("CSlice") {
            let slice_access = match access {
                PointerAccess::In => Access::In,
                PointerAccess::Inout => Access::Inout,
                PointerAccess::Out => {
                    return Err(self.error(
                        ErrorKind::Unsupported,
                        "CSlice is in or inout; an out slice would expose uninitialized elements",
                    ));
                }
            };
            self.bump();
            self.expect(b'[')?;
            let span = self.token().span.clone();
            let element = self.word()?;
            match element.as_str() {
                "Option" | "Ptr" | "NullablePtr" => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        span,
                        "slice elements are values; arrays of pointers or nullable elements are unsupported",
                    ));
                }
                "CSlice" | "ForeignVal" => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        span,
                        "nested borrowed layouts are unsupported",
                    ));
                }
                _ => {}
            }
            self.expect(b']')?;
            RawParameterType::Slice {
                element,
                span,
                access: slice_access,
                nullable,
            }
        } else {
            let span = self.token().span.clone();
            let pointee = self.word()?;
            match pointee.as_str() {
                "Ptr" | "NullablePtr" if nullable => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        span,
                        "pointer-to-pointer parameters are unsupported",
                    ));
                }
                "Ptr" => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        span,
                        "pointer-to-pointer parameters are unsupported; a nocapture pointee is one scalar or wire struct",
                    ));
                }
                "NullablePtr" => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        span,
                        "a nullable single pointer must be declared explicitly as Option[T]; pointer-to-pointer parameters are unsupported",
                    ));
                }
                "Option" | "ForeignVal" => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        span,
                        "nested borrowed layouts are unsupported",
                    ));
                }
                _ => {}
            }
            RawParameterType::Pointer {
                pointee,
                span,
                access,
                nullable,
            }
        };
        if nullable {
            self.expect(b']')?;
        }
        Ok(ty)
    }
}
struct RawWireStruct {
    declaration: crate::Declaration,
    span: Range<usize>,
}
/// Largest accepted wire struct pointee, and largest byte array field.
const MAX_WIRE_STRUCT_BYTES: usize = 1 << 20;
#[derive(Clone)]
enum RawType {
    Name(String),
    Address { pointee: Scalar, nullable: bool },
}
enum RawParameterType {
    Value(RawType),
    Slice {
        element: String,
        span: Range<usize>,
        access: Access,
        nullable: bool,
    },
    Pointer {
        pointee: String,
        span: Range<usize>,
        access: PointerAccess,
        nullable: bool,
    },
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
            "Unit"
                | "bool"
                | "CSlice"
                | "ForeignVal"
                | "Ptr"
                | "NullablePtr"
                | "Option"
                | "ByteArray"
                | "ZeroPad"
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
            } else if let Some(e) = enums.iter().find(|e| e.name() == n) {
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
pub(super) fn header(n: &str) -> bool {
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
    let mut raw_structs = vec![];
    let mut raw_functions = vec![];
    while !p.eat(b'}') {
        if p.is_word("wire") {
            if raw_structs.len() >= limits.wire_structs {
                return Err(p.error(ErrorKind::Limit, "wire struct declaration budget exceeded"));
            }
            raw_structs.push(p.wire_struct()?);
        } else if p.is_word("enum") {
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
                "expected fn/enum/wire struct; handles, C structs, code and variadics are unsupported",
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
        enums.iter().map(EnumDescriptor::name),
        "duplicate enum declaration",
    )?;
    unique(
        raw_functions.iter().map(|f| f.symbol.as_str()),
        "duplicate foreign symbol",
    )?;
    enums.sort_by(|a, b| a.name().cmp(b.name()));
    let wire_structs = compile_wire_structs(raw_structs, &enums)?;
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
                RawParameterType::Slice {
                    element,
                    span,
                    access,
                    nullable,
                } => ParameterType::Slice {
                    element: resolve_pointee(
                        &element,
                        span,
                        "slice element",
                        &enums,
                        &wire_structs,
                    )?,
                    access,
                    nullable,
                },
                RawParameterType::Pointer {
                    pointee,
                    span,
                    access,
                    nullable,
                } => ParameterType::Pointer {
                    pointee: resolve_pointee(&pointee, span, "pointee", &enums, &wire_structs)?,
                    access,
                    nullable,
                },
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
        wire_structs,
        functions,
        canonical: vec![],
        identity: DescriptorId([0; 32]),
    };
    library.canonical = canonicalize(&library, limits.canonical_bytes)?;
    library.identity = DescriptorId(*blake3::hash(&library.canonical).as_bytes());
    Ok(library)
}

fn compile_wire_structs(
    raw: Vec<RawWireStruct>,
    enums: &[EnumDescriptor],
) -> Result<Vec<WireStructDescriptor>, Error> {
    for w in &raw {
        let name = &w.declaration.name;
        if enums.iter().any(|e| e.name() == name) {
            return Err(Error::new(
                ErrorKind::Duplicate,
                w.span.clone(),
                "wire struct name collides with an enum",
            ));
        }
        for f in &w.declaration.fields {
            if let crate::WireType::Struct(n) = &f.ty
                && enums.iter().any(|e| e.name() == n)
            {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    w.span.clone(),
                    format!(
                        "field `{}` uses enum `{n}`; sparse enums do not admit every bit pattern",
                        f.name
                    ),
                ));
            }
        }
    }
    unique(
        raw.iter().map(|w| w.declaration.name.as_str()),
        "duplicate wire struct declaration",
    )?;
    let registry = crate::Registry::new(raw.iter().map(|w| w.declaration.clone()))
        .map_err(|e| Error::new(ErrorKind::Duplicate, 0..0, e.to_string()))?;
    let mut out = vec![];
    for w in raw {
        let layout = registry
            .compile(&w.declaration.name)
            .map_err(|e| Error::new(ErrorKind::Type, w.span.clone(), e.to_string()))?;
        if layout.size() > MAX_WIRE_STRUCT_BYTES {
            return Err(Error::new(
                ErrorKind::Limit,
                w.span,
                "wire struct pointee exceeds 1048576 bytes",
            ));
        }
        out.push(WireStructDescriptor {
            shared: Arc::new(WireStructData {
                declaration: w.declaration,
                layout,
            }),
        });
    }
    out.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(out)
}
fn resolve_pointee(
    name: &str,
    span: Range<usize>,
    what: &str,
    enums: &[EnumDescriptor],
    structs: &[WireStructDescriptor],
) -> Result<Pointee, Error> {
    if let Some(s) = scalar(name) {
        return Ok(Pointee::Scalar(s));
    }
    if let Some(w) = structs.iter().find(|w| w.name() == name) {
        return Ok(Pointee::Struct(w.clone()));
    }
    let why = if name == "bool" || enums.iter().any(|e| e.name() == name) {
        format!(
            "{what} `{name}` does not admit every bit pattern foreign code may write; use its integer carrier"
        )
    } else if name == "Unit" {
        format!("void {what}s are opaque and unsupported")
    } else {
        format!("unknown {what} `{name}`; declare it as a wire struct")
    };
    Err(Error::new(ErrorKind::Type, span, why))
}
