//! UTF-8 strings: the owned, growable `String` and the borrowed `Str` view
//! (§13, port wave P0 lane 7, feature F2).
//!
//! `String` is a `ChunkSeq[u8]` that always holds valid UTF-8. It is
//! `affine`. `Str` is a read-only view of the bytes `[start, start + len)` of
//! a string, with the same invariant. Like `Slice[T]`, a `Str` is second
//! class: it appears only as the whole type of an `in` parameter (`E1530`),
//! and a view (`s.as_str()`, `s[a..b]`) is written only as a call argument
//! (`E1531`). A `String` argument passes to an `in` `Str` parameter as its
//! whole view, so `t.push_str("abc")` and `f(s)` need no conversion.
//!
//! The compiler provides only what Vesper source cannot express: byte
//! access, views, growth and comparison ([`StrOp`] and the comparison
//! operators). Everything that needs UTF-8 knowledge (`next_char`,
//! `char_at`, `push_char`, `from_utf8`, `concat`, ...) is ordinary Vesper in
//! the string prelude ([`PRELUDE`]), added to a module that mentions one of
//! its names, so every backend runs it without dedicated support. No
//! operation can break the invariant: bytes enter a string only as a whole
//! encoded char (`push_char`), another string (`push_str`) or a literal.
//!
//! Representation, shared by every backend:
//! * The evaluator holds a string (and a view, copied) as a Rust `String`.
//! * Native code holds both as a 24-byte value `{len, cap, data}`. Its
//!   address is a valid `ChunkSeq[u8]` handle, so growth reuses the sequence
//!   runtime. A literal is `{n, 0, &static_bytes}`: capacity 0 with data
//!   marks borrowed static bytes, which `vesper_seq_grow` copies before the
//!   first write. A view is `{len, 0, data + start}`, so a string is already
//!   a view of itself. `as_bytes` is the slice `{&value, 0, len}`.
//! * Lean extraction models neither yet and refuses the operations.
//!
//! A view interval outside `0 <= a <= b <= len`, or with an end off a char
//! boundary, is a run-time fault like a sequence interval: the evaluator and
//! the JIT report it and an executable exits with
//! [`crate::seq::FAULT_STATUS`]. Byte reads through `byte_at` raise
//! `Fail[RuntimeError]` `IndexOutOfBounds`; `try_byte_at` is total.
use crate::ast::*;
use std::collections::BTreeSet;

/// The owned string type.
pub const STRING_TYPE: &str = "String";
/// The borrowed string view type.
pub const STR_TYPE: &str = "Str";

/// A string primitive in Place IR, carried as `Op::Seq` with
/// [`crate::seq::SeqOp::Str`], because a `String` is a `ChunkSeq[u8]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum StrOp {
    /// `s.len()`: the length in bytes.
    Len,
    /// `s.byte_at(i)`: raises `IndexOutOfBounds` past the end.
    ByteAt,
    /// `s.try_byte_at(i)`: `Option[u8]`, total.
    TryByteAt,
    /// `s.as_str()`: the whole view.
    AsStr,
    /// `s.as_bytes()`: the bytes as a `Slice[u8]` view.
    AsBytes,
    /// `s[a..b]`: a view of a byte range on char boundaries.
    SubStr,
    /// `s.push_str(in t)`.
    PushStr,
    /// `s.push_char(c)`: appends the UTF-8 encoding of the scalar value
    /// `c`; `false` (and no change) for a surrogate or a value past
    /// `0x10FFFF`.
    PushChar,
    /// `s.clear()`.
    Clear,
    /// `s.reserve(n)`: room for `n` more bytes.
    Reserve,
}

impl StrOp {
    pub fn name(self) -> &'static str {
        match self {
            Self::Len => "len",
            Self::ByteAt => "byte_at",
            Self::TryByteAt => "try_byte_at",
            Self::AsStr => "as_str",
            Self::AsBytes => "as_bytes",
            Self::SubStr => "slice",
            Self::PushStr => "push_str",
            Self::PushChar => "push_char",
            Self::Clear => "clear",
            Self::Reserve => "reserve",
        }
    }
    pub fn fallible(self) -> bool {
        self == Self::ByteAt
    }
    /// Whether the operation updates its receiver (argument 0) in place.
    pub fn mutates(self) -> bool {
        matches!(
            self,
            Self::PushStr | Self::PushChar | Self::Clear | Self::Reserve
        )
    }
    /// Whether the operation is a second-class view.
    pub fn is_view(self) -> bool {
        matches!(self, Self::AsStr | Self::AsBytes | Self::SubStr)
    }
    /// The number of arguments after the receiver.
    pub fn arity(self) -> usize {
        match self {
            Self::Len | Self::AsStr | Self::AsBytes | Self::Clear => 0,
            Self::ByteAt | Self::TryByteAt | Self::PushStr | Self::PushChar | Self::Reserve => 1,
            Self::SubStr => 2,
        }
    }
}

pub fn is_string_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(n, a) if n == STRING_TYPE && a.is_empty())
}

pub fn is_str_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(n, a) if n == STR_TYPE && a.is_empty())
}

/// `String` or `Str`.
pub fn is_text(ty: &Type) -> bool {
    is_string_type(ty) || is_str_type(ty)
}

/// Whether `ty` mentions `Str` anywhere.
pub fn mentions_str(ty: &Type) -> bool {
    match ty {
        Type::Named(n, args) => n == STR_TYPE || args.iter().any(mentions_str),
        Type::Tuple(ts) => ts.iter().any(mentions_str),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| mentions_str(t)) || mentions_str(result)
        }
        _ => false,
    }
}

/// The string primitive a method call names on a `String` or `Str`
/// receiver. Operations that update the receiver exist only on `String`.
pub fn resolve(method: &str, receiver_ty: &Type) -> Option<StrOp> {
    let op = match method {
        "len" => StrOp::Len,
        "byte_at" => StrOp::ByteAt,
        "try_byte_at" => StrOp::TryByteAt,
        "as_str" => StrOp::AsStr,
        "as_bytes" => StrOp::AsBytes,
        "push_str" => StrOp::PushStr,
        "push_char" => StrOp::PushChar,
        "clear" => StrOp::Clear,
        "reserve" => StrOp::Reserve,
        _ => return None,
    };
    if is_string_type(receiver_ty) || (is_str_type(receiver_ty) && !op.mutates()) {
        Some(op)
    } else {
        None
    }
}

/// A whole view `base.as_str()` or `base.as_bytes()`, by syntax.
pub fn whole_view(e: &Expr) -> Option<&Expr> {
    match &e.kind {
        ExprKind::Call { callee, args, .. } if args.is_empty() => match &callee.kind {
            ExprKind::Field(base, m) if m == "as_str" || m == "as_bytes" => Some(base),
            _ => None,
        },
        _ => None,
    }
}

/// The comparison operators strings support, in byte order.
pub fn is_comparison(op: &str) -> bool {
    matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=")
}

/// The string prelude: every UTF-8 operation, in Vesper. `String` methods
/// that only read delegate to the `Str` method of the same name, through
/// the whole-view coercion of their receiver.
pub const PRELUDE: &str = r#"
impl Str {
    fn is_char_boundary(in self, i: u64) -> bool {
        if i == self.len() {
            true
        } else {
            match self.try_byte_at(i) {
                Option::Some(b) => (b & 192) != 128,
                Option::None => false,
            }
        }
    }
    fn utf8_continuation(in self, i: u64) -> u32 {
        match self.try_byte_at(i) {
            Option::Some(b) => widen_u32(b) & 63,
            Option::None => 0,
        }
    }
    fn next_char(in self, i: u64) -> Option[(u32, u64)] {
        match self.try_byte_at(i) {
            Option::None => Option::None,
            Option::Some(lead) => {
                let b0 = widen_u32(lead);
                if b0 < 128 {
                    Option::Some((b0, wrapping_add(i, 1)))
                } else if b0 < 192 {
                    Option::None
                } else if b0 < 224 {
                    let c1 = Str::utf8_continuation(in self, wrapping_add(i, 1));
                    Option::Some((wrapping_mul(b0 & 31, 64) | c1, wrapping_add(i, 2)))
                } else if b0 < 240 {
                    let c1 = Str::utf8_continuation(in self, wrapping_add(i, 1));
                    let c2 = Str::utf8_continuation(in self, wrapping_add(i, 2));
                    Option::Some((wrapping_mul(b0 & 15, 4096) | wrapping_mul(c1, 64) | c2, wrapping_add(i, 3)))
                } else {
                    let c1 = Str::utf8_continuation(in self, wrapping_add(i, 1));
                    let c2 = Str::utf8_continuation(in self, wrapping_add(i, 2));
                    let c3 = Str::utf8_continuation(in self, wrapping_add(i, 3));
                    Option::Some((wrapping_mul(b0 & 7, 262144) | wrapping_mul(c1, 4096) | wrapping_mul(c2, 64) | c3, wrapping_add(i, 4)))
                }
            }
        }
    }
    fn char_at(in self, i: u64) -> u32 eff [Fail[RuntimeError]] {
        match Str::next_char(in self, i) {
            Option::Some(decoded) => {
                let (c, next) = decoded;
                c
            }
            Option::None => Fail::raise(RuntimeError::IndexOutOfBounds),
        }
    }
    fn char_count(in self) -> u64 {
        let n = self.len();
        let mut count: u64 = 0;
        for i in 0..n {
            match self.try_byte_at(i) {
                Option::Some(b) => {
                    if (b & 192) != 128 {
                        count = wrapping_add(count, 1);
                    }
                }
                Option::None => {}
            }
        }
        count
    }
    fn concat(in self, in other: Str) -> String {
        let mut out = String::with_capacity(wrapping_add(self.len(), other.len()));
        out.push_str(in self);
        out.push_str(in other);
        out
    }
}
impl String {
    fn new() -> String {
        ""
    }
    fn with_capacity(n: u64) -> String {
        let mut s = "";
        s.reserve(n);
        s
    }
    fn is_char_boundary(in self, i: u64) -> bool {
        Str::is_char_boundary(in self, i)
    }
    fn next_char(in self, i: u64) -> Option[(u32, u64)] {
        Str::next_char(in self, i)
    }
    fn char_at(in self, i: u64) -> u32 eff [Fail[RuntimeError]] {
        Str::char_at(in self, i)
    }
    fn char_count(in self) -> u64 {
        Str::char_count(in self)
    }
    fn concat(in self, in other: Str) -> String {
        Str::concat(in self, in other)
    }
    fn utf8_byte(in bytes: Slice[u8], i: u64) -> u32 {
        match bytes.try_get(i) {
            Option::Some(b) => widen_u32(b),
            Option::None => 0,
        }
    }
    fn utf8_width(in bytes: Slice[u8], i: u64) -> u64 {
        let b0 = String::utf8_byte(in bytes, i);
        let b1 = String::utf8_byte(in bytes, wrapping_add(i, 1));
        let c2 = (String::utf8_byte(in bytes, wrapping_add(i, 2)) & 192) == 128;
        let c3 = (String::utf8_byte(in bytes, wrapping_add(i, 3)) & 192) == 128;
        if b0 < 128 {
            1
        } else if b0 < 194 {
            0
        } else if b0 < 224 {
            if (b1 & 192) == 128 { 2 } else { 0 }
        } else if b0 < 240 {
            let lo: u32 = if b0 == 224 { 160 } else { 128 };
            let hi: u32 = if b0 == 237 { 159 } else { 191 };
            if b1 >= lo && b1 <= hi && c2 { 3 } else { 0 }
        } else if b0 < 245 {
            let lo: u32 = if b0 == 240 { 144 } else { 128 };
            let hi: u32 = if b0 == 244 { 143 } else { 191 };
            if b1 >= lo && b1 <= hi && c2 && c3 { 4 } else { 0 }
        } else {
            0
        }
    }
    fn utf8_decode(in bytes: Slice[u8], i: u64, w: u64) -> u32 {
        let b0 = String::utf8_byte(in bytes, i);
        let c1 = String::utf8_byte(in bytes, wrapping_add(i, 1)) & 63;
        let c2 = String::utf8_byte(in bytes, wrapping_add(i, 2)) & 63;
        let c3 = String::utf8_byte(in bytes, wrapping_add(i, 3)) & 63;
        if w == 1 {
            b0
        } else if w == 2 {
            wrapping_mul(b0 & 31, 64) | c1
        } else if w == 3 {
            wrapping_mul(b0 & 15, 4096) | wrapping_mul(c1, 64) | c2
        } else {
            wrapping_mul(b0 & 7, 262144) | wrapping_mul(c1, 4096) | wrapping_mul(c2, 64) | c3
        }
    }
    fn from_utf8(in bytes: Slice[u8]) -> Option[String] {
        let n = bytes.len();
        let mut out = String::with_capacity(n);
        let mut i: u64 = 0;
        let mut valid = true;
        loop {
            if i >= n {
                break;
            }
            let w = String::utf8_width(in bytes, i);
            if w == 0 || w > wrapping_sub(n, i) {
                valid = false;
                break;
            }
            out.push_char(String::utf8_decode(in bytes, i, w));
            i = wrapping_add(i, w);
        }
        if valid {
            Option::Some(out)
        } else {
            Option::None
        }
    }
}
"#;

/// The method names that bring in the prelude when a module mentions
/// them. Its static functions bring it in only by path
/// ([`PRELUDE_PATHS`]), since `ChunkSeq::with_capacity` shares a name.
pub const PRELUDE_NAMES: &[&str] = &[
    "is_char_boundary",
    "next_char",
    "char_at",
    "char_count",
    "concat",
    "from_utf8",
];

/// The prelude's static functions, by path.
pub const PRELUDE_PATHS: &[&str] = &["String::new", "String::with_capacity", "String::from_utf8"];

/// The functions of `module`, with the type of each `impl` block.
fn functions(module: &Module) -> Vec<(Option<&Type>, &Function)> {
    let mut out = vec![];
    for item in &module.items {
        match item {
            Item::Function(f) => out.push((None, &**f)),
            Item::Impl { ty, methods, .. } => out.extend(methods.iter().map(|m| (Some(ty), m))),
            Item::Trait(t) => out.extend(t.methods.iter().map(|m| (None, m))),
            Item::Conformance(c) => out.extend(c.methods.iter().map(|m| (None, m))),
            Item::Adt(_) | Item::Effect(_) => {}
        }
    }
    out
}

/// Whether a module needs the prelude: a parameter has type `Str`, a path
/// names a static prelude function, or a prelude method is mentioned.
fn needs_prelude(module: &Module) -> bool {
    let mut used = BTreeSet::new();
    let mut found = false;
    for (_, f) in functions(module) {
        if f.params.iter().any(|p| mentions_str(&p.ty)) {
            found = true;
        }
        if let Some(body) = &f.body {
            crate::capability_effects::used_names(body, &mut used);
            crate::regions::visit_expr(body, &mut |e| {
                if let ExprKind::Name(n) = &e.kind
                    && PRELUDE_PATHS.contains(&n.as_str())
                {
                    found = true;
                }
            });
        }
    }
    found || used.iter().any(|n| PRELUDE_NAMES.contains(&n.as_str()))
}

/// The string prelude as a module whose spans all lie after `after` and
/// after every span of `module`, or an empty module when `module` does not
/// need it. A method `module` defines itself in an `impl String` or
/// `impl Str` block replaces the prelude's.
pub(crate) fn prelude(module: &Module, after: usize) -> Module {
    if !needs_prelude(module) {
        return Module::default();
    }
    let defined: BTreeSet<String> = functions(module)
        .into_iter()
        .filter_map(|(ty, f)| {
            ty.filter(|t| is_text(t))
                .map(|t| format!("{}::{}", t.name(), f.name))
        })
        .collect();
    let end = after.max(crate::integer_widths::module_end(module));
    let padded = format!("{}{PRELUDE}", " ".repeat(end + 1));
    let mut parsed = crate::parser::parse(&padded);
    debug_assert!(
        parsed.diagnostics.is_empty(),
        "string prelude: {:?}",
        parsed.diagnostics
    );
    for item in &mut parsed.module.items {
        if let Item::Impl { ty, methods, .. } = item {
            let ty = ty.name().to_string();
            methods.retain(|m| !defined.contains(&format!("{ty}::{}", m.name)));
        }
    }
    parsed.module
}

/// The names of the prelude's functions as the checker registers them
/// (`Str::next_char`, `String::push_char`, ...).
pub fn prelude_function_names(prelude: &Module) -> BTreeSet<String> {
    prelude
        .items
        .iter()
        .flat_map(|item| match item {
            Item::Impl { ty, methods, .. } => methods
                .iter()
                .map(|m| format!("{}::{}", ty.name(), m.name))
                .collect::<Vec<_>>(),
            _ => vec![],
        })
        .collect()
}

/// The typed refusal of Lean extraction for a string primitive.
pub fn lean_refusal(op: StrOp) -> String {
    format!(
        "Lean extraction does not model the string operation '{}' yet (strings are modeled as Lean String literals only)",
        op.name()
    )
}

/// Where `Str` may appear: only as the whole type of an `in` parameter.
pub(crate) fn check_world(world: &crate::check::World) -> Vec<Diagnostic> {
    let mut diagnostics = vec![];
    let second_class = |what: &str, span: &Span| {
        Diagnostic::new(
            "E1530",
            format!("Str is a second-class borrow and cannot appear in {what}"),
            span.clone(),
        )
        .with_help("take the view as an 'in s: Str' parameter, or own the text as a String")
    };
    for f in world.functions.values() {
        for p in &f.params {
            let top = is_str_type(&p.ty) && p.pass == Pass::In;
            if !top && mentions_str(&p.ty) {
                diagnostics.push(second_class(
                    &format!("the {:?} parameter '{}'", p.pass, p.name).to_lowercase(),
                    &p.span,
                ));
            }
        }
        if mentions_str(&f.result) {
            diagnostics.push(second_class(
                &format!("the result of '{}'", f.name),
                &f.span,
            ));
        }
    }
    for adt in world.adts.values() {
        for field in adt.variants.iter().flat_map(|v| &v.fields) {
            if mentions_str(&field.ty) {
                diagnostics.push(second_class(
                    &format!("field '{}.{}'", adt.name, field.name),
                    &adt.span,
                ));
            }
        }
    }
    diagnostics
}
