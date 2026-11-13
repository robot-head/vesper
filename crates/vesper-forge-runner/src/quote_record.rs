//! Canonical bounded VFRQ/schema1 cache records: untrusted claims only.
//! No host token, scope, nonce, resolution or approval is serialized.
use vesper_loom_sandbox::{
    quote_transport as q,
    transport::{DefId, Delimiter, Diagnostic, Severity, Span, TokenKind},
};
pub const RECORD_SCHEMA_VERSION: u32 = 1;
const HEADER: usize = 72;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawRecord {
    pub key: [u8; 32],
    pub fuel_consumed: u64,
    pub peak_memory_bytes: u64,
    pub outcome: q::Outcome,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Transport(q::Error),
    Truncated,
    Magic,
    Version,
    Tag,
    NonCanonical,
    Length,
    ErrorDiagnostic,
    Allocation,
}
impl From<q::Error> for Error {
    fn from(e: q::Error) -> Self {
        Self::Transport(e)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "structured raw record {self:?}")
    }
}
impl std::error::Error for Error {}
fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or(Error::Length)
}
fn bound(n: usize, cap: usize, r: q::Resource) -> Result<(), Error> {
    if n > cap {
        Err(q::Error::Budget(r).into())
    } else {
        Ok(())
    }
}
fn preflight_work(bytes: usize, nodes: usize, limits: q::Limits) -> Result<(), Error> {
    let n = bytes
        .checked_mul(4)
        .and_then(|n| nodes.checked_mul(2).and_then(|v| n.checked_add(v)))
        .ok_or(Error::Length)?;
    bound(n, limits.max_work, q::Resource::Work)
}
fn copy(s: &str) -> Result<String, Error> {
    let mut out = String::new();
    out.try_reserve_exact(s.len())
        .map_err(|_| Error::Allocation)?;
    out.push_str(s);
    Ok(out)
}
impl RawRecord {
    pub fn encode(
        &self,
        limits: q::Limits,
        slots: usize,
        max_stdio: usize,
    ) -> Result<Vec<u8>, Error> {
        limits.check_outcome(&self.outcome, slots, false)?;
        if self
            .outcome
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
        {
            return Err(Error::ErrorDiagnostic);
        }
        bound(self.stdout.len(), max_stdio, q::Resource::Record)?;
        bound(self.stderr.len(), max_stdio, q::Resource::Record)?;
        let mut n = add(HEADER, add(self.stdout.len(), self.stderr.len())?)?;
        for item in &self.outcome.items {
            n = add(
                n,
                match item {
                    q::Item::InputSlot(_) => 5,
                    q::Item::Generated(t) => add(29, t.text.len())?,
                },
            )?;
        }
        for d in &self.outcome.diagnostics {
            n = add(n, add(18, d.message.len())?)?;
        }
        bound(n, limits.max_record_bytes, q::Resource::Record)?;
        preflight_work(
            n,
            self.outcome.items.len() + self.outcome.diagnostics.len(),
            limits,
        )?;
        let mut w = Writer::new(n)?;
        w.bytes(b"VFRQ");
        w.u32(RECORD_SCHEMA_VERSION);
        w.bytes(&self.key);
        w.u64(self.fuel_consumed);
        w.u64(self.peak_memory_bytes);
        w.count(self.outcome.items.len())?;
        w.count(self.outcome.diagnostics.len())?;
        w.count(self.stdout.len())?;
        w.count(self.stderr.len())?;
        for item in &self.outcome.items {
            match item {
                q::Item::InputSlot(n) => {
                    w.u8(1);
                    w.u32(*n);
                }
                q::Item::Generated(t) => {
                    w.u8(0);
                    let (k, d) = kind(t.kind);
                    w.u8(k);
                    w.u8(d);
                    w.span(t.reported_span);
                    w.claim(t.claimed_def);
                    w.string(&t.text)?;
                }
            }
        }
        for d in &self.outcome.diagnostics {
            w.u8(match d.severity {
                Severity::Error => 0,
                Severity::Warning => 1,
                Severity::Note => 2,
            });
            w.span(d.span);
            w.string(&d.message)?;
        }
        w.bytes(&self.stdout);
        w.bytes(&self.stderr);
        debug_assert_eq!(w.0.len(), n);
        Ok(w.0)
    }
    pub fn decode(
        bytes: &[u8],
        limits: q::Limits,
        slots: usize,
        max_stdio: usize,
    ) -> Result<Self, Error> {
        limits.validate()?;
        bound(slots, limits.max_slots, q::Resource::Slots)?;
        bound(bytes.len(), limits.max_record_bytes, q::Resource::Record)?;
        let mut r = Reader::new(bytes);
        if r.take(4)? != b"VFRQ" {
            return Err(Error::Magic);
        }
        if r.u32()? != RECORD_SCHEMA_VERSION {
            return Err(Error::Version);
        }
        let key = r.take(32)?.try_into().map_err(|_| Error::Truncated)?;
        let fuel_consumed = r.u64()?;
        let peak_memory_bytes = r.u64()?;
        let items = r.count()?;
        let diagnostics = r.count()?;
        let stdout = r.count()?;
        let stderr = r.count()?;
        bound(items, limits.max_output_items, q::Resource::Items)?;
        bound(
            diagnostics,
            limits.max_diagnostics,
            q::Resource::Diagnostics,
        )?;
        bound(stdout, max_stdio, q::Resource::Record)?;
        bound(stderr, max_stdio, q::Resource::Record)?;
        let minimum = items
            .checked_mul(5)
            .and_then(|n| diagnostics.checked_mul(18).and_then(|d| n.checked_add(d)))
            .and_then(|n| n.checked_add(stdout))
            .and_then(|n| n.checked_add(stderr))
            .ok_or(Error::Length)?;
        if minimum > r.remaining() {
            return Err(Error::Truncated);
        }
        preflight_work(bytes.len(), items + diagnostics, limits)?;
        let body = r.at;
        let (mut text, mut refs, mut diagtext) = (0usize, 0usize, 0usize);
        for _ in 0..items {
            match read_item(&mut r)? {
                BorrowedItem::Slot(n) => {
                    if n as usize >= slots {
                        return Err(q::Error::Slot.into());
                    }
                    refs = add(refs, 1)?;
                    bound(refs, limits.max_slot_occurrences, q::Resource::References)?;
                }
                BorrowedItem::Generated { kind, text: s, .. } => {
                    text = add(text, s.len())?;
                    bound(text, limits.max_copied_output_text_bytes, q::Resource::Text)?;
                    if !q::valid_token(kind, s) {
                        return Err(q::Error::Token.into());
                    }
                }
            }
        }
        for _ in 0..diagnostics {
            let d = read_diagnostic(&mut r)?;
            if d.0 == Severity::Error {
                return Err(Error::ErrorDiagnostic);
            }
            diagtext = add(diagtext, d.2.len())?;
            bound(
                diagtext,
                limits.max_diagnostic_text_bytes,
                q::Resource::DiagnosticText,
            )?;
        }
        r.take(stdout)?;
        r.take(stderr)?;
        if r.remaining() != 0 {
            return Err(Error::NonCanonical);
        }
        let mut r = Reader { bytes, at: body };
        let mut outcome = q::Outcome::default();
        outcome
            .items
            .try_reserve_exact(items)
            .map_err(|_| Error::Allocation)?;
        outcome
            .diagnostics
            .try_reserve_exact(diagnostics)
            .map_err(|_| Error::Allocation)?;
        for _ in 0..items {
            outcome.items.push(match read_item(&mut r)? {
                BorrowedItem::Slot(n) => q::Item::InputSlot(n),
                BorrowedItem::Generated {
                    kind,
                    text,
                    reported_span,
                    claimed_def,
                } => q::Item::Generated(q::GeneratedToken {
                    kind,
                    text: copy(text)?,
                    reported_span,
                    claimed_def,
                }),
            });
        }
        for _ in 0..diagnostics {
            let (severity, span, message) = read_diagnostic(&mut r)?;
            outcome.diagnostics.push(Diagnostic {
                severity,
                span,
                message: copy(message)?,
            });
        }
        let mut out = Vec::new();
        out.try_reserve_exact(stdout)
            .map_err(|_| Error::Allocation)?;
        out.extend_from_slice(r.take(stdout)?);
        let mut err = Vec::new();
        err.try_reserve_exact(stderr)
            .map_err(|_| Error::Allocation)?;
        err.extend_from_slice(r.take(stderr)?);
        Ok(Self {
            key,
            fuel_consumed,
            peak_memory_bytes,
            outcome,
            stdout: out,
            stderr: err,
        })
    }
}
pub(crate) struct Writer(pub(crate) Vec<u8>);
impl Writer {
    pub(crate) fn new(n: usize) -> Result<Self, Error> {
        let mut v = Vec::new();
        v.try_reserve_exact(n).map_err(|_| Error::Allocation)?;
        Ok(Self(v))
    }
    pub(crate) fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b)
    }
    pub(crate) fn u8(&mut self, n: u8) {
        self.0.push(n)
    }
    pub(crate) fn u32(&mut self, n: u32) {
        self.bytes(&n.to_le_bytes())
    }
    pub(crate) fn u64(&mut self, n: u64) {
        self.bytes(&n.to_le_bytes())
    }
    pub(crate) fn count(&mut self, n: usize) -> Result<(), Error> {
        self.u32(n.try_into().map_err(|_| Error::Length)?);
        Ok(())
    }
    pub(crate) fn string(&mut self, s: &str) -> Result<(), Error> {
        self.count(s.len())?;
        self.bytes(s.as_bytes());
        Ok(())
    }
    pub(crate) fn span(&mut self, s: Option<Span>) {
        self.u8(u8::from(s.is_some()));
        let s = s.unwrap_or(Span {
            file: 0,
            start: 0,
            end: 0,
        });
        self.u32(s.file);
        self.u32(s.start);
        self.u32(s.end);
    }
    pub(crate) fn claim(&mut self, d: Option<DefId>) {
        self.u8(u8::from(d.is_some()));
        let d = d.unwrap_or(DefId {
            package: 0,
            item: 0,
        });
        self.u32(d.package);
        self.u32(d.item);
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::Length)?;
        let s = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| Error::Truncated)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| Error::Truncated)?,
        ))
    }
    fn count(&mut self) -> Result<usize, Error> {
        Ok(self.u32()? as usize)
    }
    fn string(&mut self) -> Result<&'a str, Error> {
        let n = self.count()?;
        std::str::from_utf8(self.take(n)?).map_err(|_| q::Error::Utf8.into())
    }
    fn span(&mut self) -> Result<Option<Span>, Error> {
        let flag = self.u8()?;
        let s = Span {
            file: self.u32()?,
            start: self.u32()?,
            end: self.u32()?,
        };
        match flag {
            1 => Ok(Some(s)),
            0 if s
                == (Span {
                    file: 0,
                    start: 0,
                    end: 0,
                }) =>
            {
                Ok(None)
            }
            0 => Err(Error::NonCanonical),
            _ => Err(Error::Tag),
        }
    }
    fn claim(&mut self) -> Result<Option<DefId>, Error> {
        let flag = self.u8()?;
        let d = DefId {
            package: self.u32()?,
            item: self.u32()?,
        };
        match flag {
            1 => Ok(Some(d)),
            0 if d
                == (DefId {
                    package: 0,
                    item: 0,
                }) =>
            {
                Ok(None)
            }
            0 => Err(Error::NonCanonical),
            _ => Err(Error::Tag),
        }
    }
}
enum BorrowedItem<'a> {
    Slot(u32),
    Generated {
        kind: TokenKind,
        text: &'a str,
        reported_span: Option<Span>,
        claimed_def: Option<DefId>,
    },
}
fn read_item<'a>(r: &mut Reader<'a>) -> Result<BorrowedItem<'a>, Error> {
    match r.u8()? {
        1 => Ok(BorrowedItem::Slot(r.u32()?)),
        0 => {
            let kind = unkind(r.u8()?, r.u8()?)?;
            let reported_span = r.span()?;
            let claimed_def = r.claim()?;
            let text = r.string()?;
            Ok(BorrowedItem::Generated {
                kind,
                text,
                reported_span,
                claimed_def,
            })
        }
        _ => Err(Error::Tag),
    }
}
fn read_diagnostic<'a>(r: &mut Reader<'a>) -> Result<(Severity, Option<Span>, &'a str), Error> {
    let severity = match r.u8()? {
        0 => Severity::Error,
        1 => Severity::Warning,
        2 => Severity::Note,
        _ => return Err(Error::Tag),
    };
    let span = r.span()?;
    Ok((severity, span, r.string()?))
}
pub(crate) fn kind(k: TokenKind) -> (u8, u8) {
    let delim = |d| match d {
        Delimiter::Paren => 0,
        Delimiter::Bracket => 1,
        Delimiter::Brace => 2,
    };
    match k {
        TokenKind::Ident => (0, 0),
        TokenKind::Punct => (1, 0),
        TokenKind::IntLiteral => (2, 0),
        TokenKind::StringLiteral => (3, 0),
        TokenKind::Open(d) => (4, delim(d)),
        TokenKind::Close(d) => (5, delim(d)),
    }
}
fn unkind(k: u8, d: u8) -> Result<TokenKind, Error> {
    let delim = match d {
        0 => Delimiter::Paren,
        1 => Delimiter::Bracket,
        2 => Delimiter::Brace,
        _ => return Err(Error::Tag),
    };
    Ok(match k {
        0 if d == 0 => TokenKind::Ident,
        1 if d == 0 => TokenKind::Punct,
        2 if d == 0 => TokenKind::IntLiteral,
        3 if d == 0 => TokenKind::StringLiteral,
        4 => TokenKind::Open(delim),
        5 => TokenKind::Close(delim),
        _ => return Err(Error::Tag),
    })
}
