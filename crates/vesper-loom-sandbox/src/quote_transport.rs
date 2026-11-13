//! Versioned untrusted quotation claims, independent from flat sandbox v1.
//! Input slot ordinals are not scopes, host handles, provenance or approval.
use crate::transport::{DefId, Delimiter, Diagnostic, Span, TokenKind};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const WIT_PACKAGE: &str = "vesper:forge-quote@0.1.0";
pub const QUOTATION_INTERFACE: &str = "vesper:forge-quote/quotation@0.1.0";
pub const WIT_SOURCE: &str = include_str!("../wit/forge-quote/quotation.wit");
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MacroKind {
    ProcMacro,
    DeriveMacro,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Category {
    Expr,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputToken {
    pub kind: TokenKind,
    pub text: String,
    pub reported_span: Option<Span>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputSlot {
    pub category: Category,
    pub tokens: Vec<InputToken>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Invocation {
    pub protocol_version: u32,
    pub kind: MacroKind,
    pub entry: String,
    pub args: Vec<String>,
    pub slots: Vec<InputSlot>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GeneratedToken {
    pub kind: TokenKind,
    pub text: String,
    pub reported_span: Option<Span>,
    pub claimed_def: Option<DefId>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Item {
    Generated(GeneratedToken),
    InputSlot(u32),
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub items: Vec<Item>,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Slice {
    pub offset: u32,
    pub len: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedGenerated {
    pub kind: TokenKind,
    pub text: Slice,
    pub reported_span: Option<Span>,
    pub claimed_def: Option<DefId>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackedItem {
    Generated(PackedGenerated),
    InputSlot(u32),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedDiagnostic {
    pub severity: crate::transport::Severity,
    pub message: Slice,
    pub reported_span: Option<Span>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackedResult {
    pub items: Vec<PackedItem>,
    pub diagnostics: Vec<PackedDiagnostic>,
    pub text: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub max_slots: usize,
    pub max_input_tokens: usize,
    pub max_input_text_bytes: usize,
    pub max_output_items: usize,
    pub max_slot_occurrences: usize,
    pub max_copied_output_text_bytes: usize,
    pub max_diagnostics: usize,
    pub max_diagnostic_text_bytes: usize,
    pub max_expanded_tokens: usize,
    pub max_expanded_text_bytes: usize,
    pub max_expanded_scopes: usize,
    pub max_depth: usize,
    pub max_record_bytes: usize,
    pub max_invocation_bytes: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_slots: 64,
            max_input_tokens: 65536,
            max_input_text_bytes: 1 << 20,
            max_output_items: 65536,
            max_slot_occurrences: 4096,
            max_copied_output_text_bytes: 1 << 20,
            max_diagnostics: 256,
            max_diagnostic_text_bytes: 64 << 10,
            max_expanded_tokens: 65536,
            max_expanded_text_bytes: 1 << 20,
            max_expanded_scopes: 262144,
            max_depth: 128,
            max_record_bytes: 8 << 20,
            max_invocation_bytes: 4 << 20,
            max_work: 16 << 20,
        }
    }
}
impl Limits {
    pub const HARD: Self = Self {
        max_slots: 256,
        max_input_tokens: 262144,
        max_input_text_bytes: 8 << 20,
        max_output_items: 262144,
        max_slot_occurrences: 16384,
        max_copied_output_text_bytes: 8 << 20,
        max_diagnostics: 4096,
        max_diagnostic_text_bytes: 1 << 20,
        max_expanded_tokens: 262144,
        max_expanded_text_bytes: 8 << 20,
        max_expanded_scopes: 1 << 20,
        max_depth: 128,
        max_record_bytes: 32 << 20,
        max_invocation_bytes: 32 << 20,
        max_work: 128 << 20,
    };
    pub fn validate(self) -> Result<(), Error> {
        let a = self.fields();
        let b = Self::HARD.fields();
        if a.into_iter().zip(b).any(|(a, b)| a > b) {
            return Err(Error::InvalidLimits);
        }
        Ok(())
    }
    fn fields(self) -> [usize; 15] {
        [
            self.max_slots,
            self.max_input_tokens,
            self.max_input_text_bytes,
            self.max_output_items,
            self.max_slot_occurrences,
            self.max_copied_output_text_bytes,
            self.max_diagnostics,
            self.max_diagnostic_text_bytes,
            self.max_expanded_tokens,
            self.max_expanded_text_bytes,
            self.max_expanded_scopes,
            self.max_depth,
            self.max_record_bytes,
            self.max_invocation_bytes,
            self.max_work,
        ]
    }
    pub fn check_invocation(self, inv: &Invocation) -> Result<(), Error> {
        self.validate()?;
        if inv.protocol_version != PROTOCOL_VERSION {
            return Err(Error::Protocol);
        }
        if inv.entry.is_empty() || inv.entry.len() > 256 || inv.entry.chars().any(char::is_control)
        {
            return Err(Error::Entry);
        }
        bound(inv.slots.len(), self.max_slots, Resource::Slots)?;
        bound(inv.args.len(), 64, Resource::Arguments)?;
        let mut work = Budget::new(self.max_work);
        work.charge(inv.entry.len())?;
        let mut text = inv.entry.len();
        for arg in &inv.args {
            text = add(text, arg.len(), Resource::Text)?;
            bound(text, self.max_input_text_bytes, Resource::Text)?;
            work.charge(arg.len() + 1)?;
        }
        let mut tokens = 0;
        for slot in &inv.slots {
            work.charge(1)?;
            tokens = add(tokens, slot.tokens.len(), Resource::Tokens)?;
            bound(tokens, self.max_input_tokens, Resource::Tokens)?;
            let mut stack = Vec::new();
            for token in &slot.tokens {
                text = add(text, token.text.len(), Resource::Text)?;
                bound(text, self.max_input_text_bytes, Resource::Text)?;
                work.charge(
                    token
                        .text
                        .len()
                        .checked_mul(2)
                        .and_then(|n| n.checked_add(1))
                        .ok_or(Error::Budget(Resource::Work))?,
                )?;
                if !valid_token(token.kind, &token.text) {
                    return Err(Error::Token);
                }
                delimiters(token.kind, &mut stack, self.max_depth)?;
            }
            if !stack.is_empty() {
                return Err(Error::Delimiters);
            }
        }
        bound(text, self.max_input_text_bytes, Resource::Text)?;
        let encoded = invocation_size(inv)?;
        bound(encoded, self.max_invocation_bytes, Resource::Invocation)?;
        work.charge(encoded)
    }
    pub fn check_outcome(self, out: &Outcome, slots: usize, failed: bool) -> Result<(), Error> {
        self.validate()?;
        bound(slots, self.max_slots, Resource::Slots)?;
        bound(out.items.len(), self.max_output_items, Resource::Items)?;
        bound(
            out.diagnostics.len(),
            self.max_diagnostics,
            Resource::Diagnostics,
        )?;
        if failed && !out.items.is_empty() {
            return Err(Error::ItemsInFailure);
        }
        let (mut text, mut refs) = (0, 0);
        let mut work = Budget::new(self.max_work);
        for item in &out.items {
            work.charge(1)?;
            match item {
                Item::Generated(t) => {
                    text = add(text, t.text.len(), Resource::Text)?;
                    bound(text, self.max_copied_output_text_bytes, Resource::Text)?;
                    work.charge(
                        t.text
                            .len()
                            .checked_mul(2)
                            .ok_or(Error::Budget(Resource::Work))?,
                    )?;
                    if !valid_token(t.kind, &t.text) {
                        return Err(Error::Token);
                    }
                }
                Item::InputSlot(i) => {
                    if *i as usize >= slots {
                        return Err(Error::Slot);
                    }
                    refs = add(refs, 1, Resource::References)?;
                    bound(refs, self.max_slot_occurrences, Resource::References)?;
                }
            }
        }
        let mut text = 0;
        for d in &out.diagnostics {
            text = add(text, d.message.len(), Resource::DiagnosticText)?;
            bound(
                text,
                self.max_diagnostic_text_bytes,
                Resource::DiagnosticText,
            )?;
            work.charge(d.message.len() + 1)?;
        }
        Ok(())
    }
    /// All referenced slices, their UTF8/kind and aggregate aliased copy costs
    /// are checked in a first pass before any per-item strings are allocated.
    pub fn unpack(
        self,
        packed: &PackedResult,
        slots: usize,
        failed: bool,
    ) -> Result<Outcome, Error> {
        self.validate()?;
        bound(slots, self.max_slots, Resource::Slots)?;
        bound(packed.items.len(), self.max_output_items, Resource::Items)?;
        bound(
            packed.diagnostics.len(),
            self.max_diagnostics,
            Resource::Diagnostics,
        )?;
        let table_cap = add(
            self.max_copied_output_text_bytes,
            self.max_diagnostic_text_bytes,
            Resource::Text,
        )?;
        bound(packed.text.len(), table_cap, Resource::Text)?;
        if failed && !packed.items.is_empty() {
            return Err(Error::ItemsInFailure);
        }
        let (mut copied, mut refs, mut diagtext) = (0, 0, 0);
        let mut work = Budget::new(self.max_work);
        for item in &packed.items {
            work.charge(1)?;
            match item {
                PackedItem::Generated(t) => {
                    let bytes = slice_bytes(&packed.text, t.text)?;
                    copied = add(copied, bytes.len(), Resource::Text)?;
                    bound(copied, self.max_copied_output_text_bytes, Resource::Text)?;
                    work.charge(
                        bytes
                            .len()
                            .checked_mul(4)
                            .ok_or(Error::Budget(Resource::Work))?,
                    )?;
                    let s = std::str::from_utf8(bytes).map_err(|_| Error::Utf8)?;
                    if !valid_token(t.kind, s) {
                        return Err(Error::Token);
                    }
                }
                PackedItem::InputSlot(i) => {
                    if *i as usize >= slots {
                        return Err(Error::Slot);
                    }
                    refs = add(refs, 1, Resource::References)?;
                    bound(refs, self.max_slot_occurrences, Resource::References)?;
                }
            }
        }
        for d in &packed.diagnostics {
            let bytes = slice_bytes(&packed.text, d.message)?;
            diagtext = add(diagtext, bytes.len(), Resource::DiagnosticText)?;
            bound(
                diagtext,
                self.max_diagnostic_text_bytes,
                Resource::DiagnosticText,
            )?;
            work.charge(
                bytes
                    .len()
                    .checked_mul(4)
                    .and_then(|n| n.checked_add(1))
                    .ok_or(Error::Budget(Resource::Work))?,
            )?;
            std::str::from_utf8(bytes).map_err(|_| Error::Utf8)?;
        }
        let mut out = Outcome::default();
        out.items
            .try_reserve_exact(packed.items.len())
            .map_err(|_| Error::Allocation)?;
        out.diagnostics
            .try_reserve_exact(packed.diagnostics.len())
            .map_err(|_| Error::Allocation)?;
        for item in &packed.items {
            out.items.push(match item {
                PackedItem::Generated(t) => Item::Generated(GeneratedToken {
                    kind: t.kind,
                    text: copy(borrow_slice(&packed.text, t.text)?)?,
                    reported_span: t.reported_span,
                    claimed_def: t.claimed_def,
                }),
                PackedItem::InputSlot(i) => Item::InputSlot(*i),
            });
        }
        for d in &packed.diagnostics {
            out.diagnostics.push(Diagnostic {
                severity: d.severity,
                message: copy(borrow_slice(&packed.text, d.message)?)?,
                span: d.reported_span,
            });
        }
        Ok(out)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resource {
    Slots,
    Arguments,
    Tokens,
    Scopes,
    Text,
    Items,
    References,
    Diagnostics,
    DiagnosticText,
    Depth,
    Record,
    Invocation,
    Work,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidLimits,
    Budget(Resource),
    Protocol,
    Entry,
    Slot,
    Slice,
    Utf8,
    Token,
    Delimiters,
    ItemsInFailure,
    Allocation,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "quotation transport {self:?}")
    }
}
impl std::error::Error for Error {}
pub(crate) fn bound(n: usize, cap: usize, r: Resource) -> Result<(), Error> {
    if n > cap {
        Err(Error::Budget(r))
    } else {
        Ok(())
    }
}
pub(crate) fn add(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
    a.checked_add(b).ok_or(Error::Budget(r))
}
pub(crate) fn copy(s: &str) -> Result<String, Error> {
    let mut out = String::new();
    out.try_reserve_exact(s.len())
        .map_err(|_| Error::Allocation)?;
    out.push_str(s);
    Ok(out)
}
pub(crate) struct Budget(usize);
impl Budget {
    pub(crate) fn new(n: usize) -> Self {
        Self(n)
    }
    pub(crate) fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.0 = self.0.checked_sub(n).ok_or(Error::Budget(Resource::Work))?;
        Ok(())
    }
}
fn slice_bytes(text: &[u8], s: Slice) -> Result<&[u8], Error> {
    let start = s.offset as usize;
    let end = start.checked_add(s.len as usize).ok_or(Error::Slice)?;
    text.get(start..end).ok_or(Error::Slice)
}
fn borrow_slice(text: &[u8], s: Slice) -> Result<&str, Error> {
    std::str::from_utf8(slice_bytes(text, s)?).map_err(|_| Error::Utf8)
}
fn delimiters(kind: TokenKind, stack: &mut Vec<Delimiter>, depth: usize) -> Result<(), Error> {
    match kind {
        TokenKind::Open(d) => {
            bound(stack.len() + 1, depth, Resource::Depth)?;
            stack.try_reserve(1).map_err(|_| Error::Allocation)?;
            stack.push(d);
        }
        TokenKind::Close(d) if stack.pop() != Some(d) => return Err(Error::Delimiters),
        _ => {}
    }
    Ok(())
}
/// Lexical coherence only; category/template parsing is the Forge host's job.
pub fn valid_token(kind: TokenKind, text: &str) -> bool {
    match kind {
        TokenKind::Ident => {
            let mut c = text.chars();
            c.next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '@')
                && c.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
        }
        TokenKind::IntLiteral => valid_int(text),
        TokenKind::Punct => {
            text == "::"
                || (text.len() == 1
                    && text.as_bytes()[0].is_ascii_punctuation()
                    && !matches!(
                        text.as_bytes()[0],
                        b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'"' | b'@' | b'_'
                    ))
        }
        TokenKind::Open(d) => {
            text == match d {
                Delimiter::Paren => "(",
                Delimiter::Bracket => "[",
                Delimiter::Brace => "{",
            }
        }
        TokenKind::Close(d) => {
            text == match d {
                Delimiter::Paren => ")",
                Delimiter::Bracket => "]",
                Delimiter::Brace => "}",
            }
        }
        TokenKind::StringLiteral => valid_string(text),
    }
}
fn valid_string(s: &str) -> bool {
    let b = s.as_bytes();
    if b.first() != Some(&b'"') || b.last() != Some(&b'"') || b.len() < 2 {
        return false;
    }
    let mut i = 1;
    while i < b.len() - 1 {
        match b[i] {
            0..=31 | b'"' => return false,
            b'\\' => {
                i += 1;
                if i >= b.len() - 1 {
                    return false;
                }
                match b[i] {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                    b'u' => {
                        let Some(unit) = hex4(b, i + 1) else {
                            return false;
                        };
                        i += 4;
                        if (0xd800..=0xdbff).contains(&unit) {
                            if b.get(i + 1..i + 3) != Some(b"\\u") {
                                return false;
                            }
                            let Some(low) = hex4(b, i + 3) else {
                                return false;
                            };
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return false;
                            }
                            i += 6;
                        } else if (0xdc00..=0xdfff).contains(&unit) {
                            return false;
                        }
                    }
                    _ => return false,
                }
            }
            _ => {}
        }
        i += 1;
    }
    i == b.len() - 1
}
fn hex4(b: &[u8], at: usize) -> Option<u16> {
    let digits = b.get(at..at.checked_add(4)?)?;
    let mut n = 0u16;
    for digit in digits {
        n = n.checked_mul(16)?.checked_add(match digit {
            b'0'..=b'9' => (*digit - b'0') as u16,
            b'a'..=b'f' => (*digit - b'a' + 10) as u16,
            b'A'..=b'F' => (*digit - b'A' + 10) as u16,
            _ => return None,
        })?;
    }
    Some(n)
}

fn valid_int(text: &str) -> bool {
    if !text.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return false;
    }
    let (text, base) = if let Some(s) = text.strip_prefix("0x") {
        (s, 16u64)
    } else {
        (text, 10u64)
    };
    let mut value = 0u64;
    let mut digits = 0;
    for b in text.bytes() {
        if b == b'_' {
            continue;
        }
        let digit = match b {
            b'0'..=b'9' => (b - b'0') as u64,
            b'a'..=b'f' if base == 16 => (b - b'a' + 10) as u64,
            b'A'..=b'F' if base == 16 => (b - b'A' + 10) as u64,
            _ => return false,
        };
        if digit >= base {
            return false;
        }
        let Some(n) = value.checked_mul(base).and_then(|n| n.checked_add(digit)) else {
            return false;
        };
        value = n;
        digits += 1;
    }
    digits > 0
}

/// Exact VFQI/schema1 canonical invocation length used by the raw runner.
/// Structural count/text validation is performed by check_invocation first.
pub fn invocation_size(inv: &Invocation) -> Result<usize, Error> {
    let mut n = add(21, inv.entry.len(), Resource::Invocation)?;
    for arg in &inv.args {
        n = add(
            n,
            add(4, arg.len(), Resource::Invocation)?,
            Resource::Invocation,
        )?;
    }
    for slot in &inv.slots {
        n = add(n, 5, Resource::Invocation)?;
        for token in &slot.tokens {
            n = add(
                n,
                add(19, token.text.len(), Resource::Invocation)?,
                Resource::Invocation,
            )?;
        }
    }
    Ok(n)
}
