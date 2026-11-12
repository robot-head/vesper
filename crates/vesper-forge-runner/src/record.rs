//! Bounded binary encoding of one raw, untrusted macro execution.
//!
//! A record holds exactly what the guest returned on the wire (tokens with
//! their untrusted spans, scope marks and `pre-resolved` claims, plus notes
//! and warnings), the guest's captured stdout/stderr and its metering, all
//! bound to the execution key it was produced under. It never holds host
//! scopes, expansion ids, splice handles, resolutions or audits: those are
//! recreated by every session that uses the record.
//!
//! ```text
//! magic "VFRX" | schema u32 | key [32] | fuel u64 | peak-memory u64
//! | tokens u32 | diagnostics u32 | scopes u32 | text u32 | stdout u32 | stderr u32
//! | token*      kind u8, delim u8, span 3*u32, text-len u32, scopes-len u32,
//!               has-claim u8, claim 2*u32                         (31 bytes)
//! | diagnostic* severity u8, has-span u8, span 3*u32, text-len u32 (18 bytes)
//! | scope*      u32
//! | text        token texts then diagnostic messages, in order
//! | stdout | stderr
//! ```
//!
//! All integers are little-endian. Decoding checks the header counts against
//! the request's limits and the exact total length *before* allocating any
//! per-token value, so a hostile or corrupt record cannot amplify
//! allocation; text is inline (never aliased), so total text is at most the
//! record size. A decoded record must re-encode to the same bytes, which
//! makes the encoding canonical.

use vesper_loom_sandbox::transport::{
    Delimiter, Diagnostic, Outcome, Severity, Span, Token, TokenKind, TransportLimits,
};

/// Version of the raw record encoding (bound into every execution key).
pub const RECORD_SCHEMA_VERSION: u32 = 1;

const MAGIC: &[u8; 4] = b"VFRX";
const HEADER: usize = 4 + 4 + 32 + 8 + 8 + 6 * 4;
const TOKEN: usize = 1 + 1 + 12 + 4 + 4 + 1 + 8;
const DIAGNOSTIC: usize = 1 + 1 + 12 + 4;

/// One raw execution as stored in the CAS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawRecord {
    /// The execution key this record was produced under.
    pub key: [u8; 32],
    pub fuel_consumed: u64,
    pub peak_memory_bytes: u64,
    /// Untrusted wire output (no error diagnostics: failed guests are not
    /// recorded).
    pub outcome: Outcome,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Bounds every record must satisfy, derived from the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordLimits {
    pub transport: TransportLimits,
    pub max_stdio_bytes: usize,
    pub max_record_bytes: usize,
}

/// Why bytes are not a valid record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordError {
    TooLarge {
        bytes: usize,
        limit: usize,
    },
    Truncated,
    BadMagic,
    Schema {
        found: u32,
    },
    /// A header count exceeds the request's limits.
    OverLimit {
        field: &'static str,
        count: usize,
        limit: usize,
    },
    /// Declared lengths do not add up to the record size.
    Length {
        expected: Option<usize>,
        actual: usize,
    },
    /// Per-entry lengths disagree with the header totals.
    Totals {
        field: &'static str,
    },
    BadTag {
        field: &'static str,
        index: usize,
    },
    InvalidUtf8 {
        index: usize,
    },
    /// The decoded outcome violates the transport limits or nests badly.
    Transport(String),
    /// An error diagnostic in a stored success.
    ErrorDiagnostic,
    /// The bytes decode but are not the canonical encoding.
    NonCanonical,
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

fn kind_tag(k: TokenKind) -> (u8, u8) {
    let d = |d: Delimiter| match d {
        Delimiter::Paren => 0,
        Delimiter::Bracket => 1,
        Delimiter::Brace => 2,
    };
    match k {
        TokenKind::Ident => (0, 0),
        TokenKind::Punct => (1, 0),
        TokenKind::IntLiteral => (2, 0),
        TokenKind::StringLiteral => (3, 0),
        TokenKind::Open(x) => (4, d(x)),
        TokenKind::Close(x) => (5, d(x)),
    }
}

fn tag_kind(kind: u8, delim: u8) -> Option<TokenKind> {
    let d = match delim {
        0 => Delimiter::Paren,
        1 => Delimiter::Bracket,
        2 => Delimiter::Brace,
        _ => return None,
    };
    let plain = |k| (delim == 0).then_some(k);
    match kind {
        0 => plain(TokenKind::Ident),
        1 => plain(TokenKind::Punct),
        2 => plain(TokenKind::IntLiteral),
        3 => plain(TokenKind::StringLiteral),
        4 => Some(TokenKind::Open(d)),
        5 => Some(TokenKind::Close(d)),
        _ => None,
    }
}

fn severity_tag(s: Severity) -> u8 {
    match s {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Note => 2,
    }
}

fn tag_severity(t: u8) -> Option<Severity> {
    match t {
        0 => Some(Severity::Error),
        1 => Some(Severity::Warning),
        2 => Some(Severity::Note),
        _ => None,
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], RecordError> {
        let end = self.at.checked_add(n).ok_or(RecordError::Truncated)?;
        let s = self.bytes.get(self.at..end).ok_or(RecordError::Truncated)?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, RecordError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, RecordError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, RecordError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn span(&mut self) -> Result<Span, RecordError> {
        Ok(Span {
            file: self.u32()?,
            start: self.u32()?,
            end: self.u32()?,
        })
    }
}

fn len32(n: usize) -> u32 {
    u32::try_from(n).expect("record fields are bounded below u32::MAX by limits")
}

fn put_span(out: &mut Vec<u8>, s: Span) {
    out.extend_from_slice(&s.file.to_le_bytes());
    out.extend_from_slice(&s.start.to_le_bytes());
    out.extend_from_slice(&s.end.to_le_bytes());
}

impl RawRecord {
    /// The exact encoded size, computed without encoding.
    pub fn encoded_len(&self) -> usize {
        let o = &self.outcome;
        let text: usize = o.tokens.iter().map(|t| t.text.len()).sum::<usize>()
            + o.diagnostics.iter().map(|d| d.message.len()).sum::<usize>();
        let scopes: usize = o.tokens.iter().map(|t| t.scopes.len()).sum();
        HEADER
            + o.tokens.len() * TOKEN
            + o.diagnostics.len() * DIAGNOSTIC
            + scopes * 4
            + text
            + self.stdout.len()
            + self.stderr.len()
    }

    /// Canonical encoding. Callers check [`Self::encoded_len`] against
    /// their bound first; every count is already bounded by the sandbox's
    /// transport and stdio limits.
    pub fn encode(&self) -> Vec<u8> {
        let o = &self.outcome;
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&RECORD_SCHEMA_VERSION.to_le_bytes());
        out.extend_from_slice(&self.key);
        out.extend_from_slice(&self.fuel_consumed.to_le_bytes());
        out.extend_from_slice(&self.peak_memory_bytes.to_le_bytes());
        let text: usize = o.tokens.iter().map(|t| t.text.len()).sum::<usize>()
            + o.diagnostics.iter().map(|d| d.message.len()).sum::<usize>();
        let scopes: usize = o.tokens.iter().map(|t| t.scopes.len()).sum();
        for n in [
            o.tokens.len(),
            o.diagnostics.len(),
            scopes,
            text,
            self.stdout.len(),
            self.stderr.len(),
        ] {
            out.extend_from_slice(&len32(n).to_le_bytes());
        }
        for t in &o.tokens {
            let (k, d) = kind_tag(t.kind);
            out.push(k);
            out.push(d);
            put_span(&mut out, t.span);
            out.extend_from_slice(&len32(t.text.len()).to_le_bytes());
            out.extend_from_slice(&len32(t.scopes.len()).to_le_bytes());
            match t.pre_resolved {
                Some(def) => {
                    out.push(1);
                    out.extend_from_slice(&def.package.to_le_bytes());
                    out.extend_from_slice(&def.item.to_le_bytes());
                }
                None => {
                    out.push(0);
                    out.extend_from_slice(&[0; 8]);
                }
            }
        }
        for d in &o.diagnostics {
            out.push(severity_tag(d.severity));
            match d.span {
                Some(s) => {
                    out.push(1);
                    put_span(&mut out, s);
                }
                None => {
                    out.push(0);
                    out.extend_from_slice(&[0; 12]);
                }
            }
            out.extend_from_slice(&len32(d.message.len()).to_le_bytes());
        }
        for t in &o.tokens {
            for s in &t.scopes {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        for t in &o.tokens {
            out.extend_from_slice(t.text.as_bytes());
        }
        for d in &o.diagnostics {
            out.extend_from_slice(d.message.as_bytes());
        }
        out.extend_from_slice(&self.stdout);
        out.extend_from_slice(&self.stderr);
        debug_assert_eq!(out.len(), self.encoded_len());
        out
    }

    /// Strict, bounded decoding. See the module documentation for the
    /// order of checks.
    pub fn decode(bytes: &[u8], limits: &RecordLimits) -> Result<Self, RecordError> {
        if bytes.len() > limits.max_record_bytes {
            return Err(RecordError::TooLarge {
                bytes: bytes.len(),
                limit: limits.max_record_bytes,
            });
        }
        let mut c = Cursor { bytes, at: 0 };
        if c.take(4)? != MAGIC {
            return Err(RecordError::BadMagic);
        }
        let schema = c.u32()?;
        if schema != RECORD_SCHEMA_VERSION {
            return Err(RecordError::Schema { found: schema });
        }
        let key: [u8; 32] = c.take(32)?.try_into().unwrap();
        let fuel_consumed = c.u64()?;
        let peak_memory_bytes = c.u64()?;
        let mut counts = [0usize; 6];
        for n in &mut counts {
            *n = c.u32()? as usize;
        }
        let [n_tokens, n_diags, n_scopes, n_text, n_stdout, n_stderr] = counts;
        let t = &limits.transport;
        for (field, count, limit) in [
            ("tokens", n_tokens, t.max_tokens),
            ("diagnostics", n_diags, t.max_diagnostics),
            ("scopes", n_scopes, t.max_total_scopes),
            // Token text and diagnostic text are bounded separately by the
            // transport; their sum is at most twice the text limit.
            ("text", n_text, t.max_text_bytes.saturating_mul(2)),
            ("stdout", n_stdout, limits.max_stdio_bytes),
            ("stderr", n_stderr, limits.max_stdio_bytes),
        ] {
            if count > limit {
                return Err(RecordError::OverLimit {
                    field,
                    count,
                    limit,
                });
            }
        }
        let expected = (|| {
            HEADER
                .checked_add(n_tokens.checked_mul(TOKEN)?)?
                .checked_add(n_diags.checked_mul(DIAGNOSTIC)?)?
                .checked_add(n_scopes.checked_mul(4)?)?
                .checked_add(n_text)?
                .checked_add(n_stdout)?
                .checked_add(n_stderr)
        })();
        if expected != Some(bytes.len()) {
            return Err(RecordError::Length {
                expected,
                actual: bytes.len(),
            });
        }

        // Pass 1: fixed-size entries only; no allocation per entry.
        let fixed_start = c.at;
        let mut token_text = 0usize;
        let mut scope_sum = 0usize;
        for index in 0..n_tokens {
            let (k, d) = (c.u8()?, c.u8()?);
            tag_kind(k, d).ok_or(RecordError::BadTag {
                field: "token kind",
                index,
            })?;
            c.span()?;
            token_text = token_text.saturating_add(c.u32()? as usize);
            let scopes = c.u32()? as usize;
            if scopes > t.max_scopes_per_token {
                return Err(RecordError::OverLimit {
                    field: "scopes per token",
                    count: scopes,
                    limit: t.max_scopes_per_token,
                });
            }
            scope_sum = scope_sum.saturating_add(scopes);
            let has = c.u8()?;
            let claim = c.take(8)?;
            if has > 1 || (has == 0 && claim != [0; 8]) {
                return Err(RecordError::BadTag {
                    field: "claim",
                    index,
                });
            }
        }
        let mut diag_text = 0usize;
        for index in 0..n_diags {
            let sev = tag_severity(c.u8()?).ok_or(RecordError::BadTag {
                field: "severity",
                index,
            })?;
            if sev == Severity::Error {
                return Err(RecordError::ErrorDiagnostic);
            }
            let has = c.u8()?;
            let span = c.take(12)?;
            if has > 1 || (has == 0 && span != [0; 12]) {
                return Err(RecordError::BadTag {
                    field: "diagnostic span",
                    index,
                });
            }
            diag_text = diag_text.saturating_add(c.u32()? as usize);
        }
        if scope_sum != n_scopes {
            return Err(RecordError::Totals { field: "scopes" });
        }
        if token_text.checked_add(diag_text) != Some(n_text) {
            return Err(RecordError::Totals { field: "text" });
        }
        for (field, n) in [("token text", token_text), ("diagnostic text", diag_text)] {
            if n > t.max_text_bytes {
                return Err(RecordError::OverLimit {
                    field,
                    count: n,
                    limit: t.max_text_bytes,
                });
            }
        }

        // Pass 2: every size is now known to be within the limits.
        let scopes_at = c.at;
        let text_at = scopes_at + n_scopes * 4;
        let mut scope_cursor = Cursor {
            bytes,
            at: scopes_at,
        };
        let mut text_cursor = Cursor { bytes, at: text_at };
        let mut c = Cursor {
            bytes,
            at: fixed_start,
        };
        let mut tokens = Vec::with_capacity(n_tokens);
        for index in 0..n_tokens {
            let kind = tag_kind(c.u8()?, c.u8()?).expect("checked");
            let span = c.span()?;
            let text_len = c.u32()? as usize;
            let scope_len = c.u32()? as usize;
            let has = c.u8()?;
            let (package, item) = (c.u32()?, c.u32()?);
            let mut scopes = Vec::with_capacity(scope_len);
            for _ in 0..scope_len {
                scopes.push(scope_cursor.u32()?);
            }
            let text = std::str::from_utf8(text_cursor.take(text_len)?)
                .map_err(|_| RecordError::InvalidUtf8 { index })?
                .to_owned();
            tokens.push(Token {
                kind,
                text,
                span,
                scopes,
                pre_resolved: (has == 1)
                    .then_some(vesper_loom_sandbox::transport::DefId { package, item }),
            });
        }
        let mut diagnostics = Vec::with_capacity(n_diags);
        for index in 0..n_diags {
            let severity = tag_severity(c.u8()?).expect("checked");
            let has = c.u8()?;
            let span = c.span()?;
            let len = c.u32()? as usize;
            let message = std::str::from_utf8(text_cursor.take(len)?)
                .map_err(|_| RecordError::InvalidUtf8 {
                    index: n_tokens + index,
                })?
                .to_owned();
            diagnostics.push(Diagnostic {
                severity,
                message,
                span: (has == 1).then_some(span),
            });
        }
        let stdout = text_cursor.take(n_stdout)?.to_vec();
        let stderr = text_cursor.take(n_stderr)?.to_vec();
        let outcome = Outcome {
            tokens,
            diagnostics,
        };
        limits
            .transport
            .check_outcome(&outcome)
            .map_err(|v| RecordError::Transport(format!("{v:?}")))?;
        let record = RawRecord {
            key,
            fuel_consumed,
            peak_memory_bytes,
            outcome,
            stdout,
            stderr,
        };
        if record.encode() != bytes {
            return Err(RecordError::NonCanonical);
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesper_loom_sandbox::transport::DefId;

    fn limits() -> RecordLimits {
        RecordLimits {
            transport: TransportLimits {
                max_tokens: 8,
                max_text_bytes: 64,
                max_scopes_per_token: 2,
                max_total_scopes: 4,
                max_diagnostics: 2,
            },
            max_stdio_bytes: 16,
            max_record_bytes: 4096,
        }
    }

    fn tok(kind: TokenKind, text: &str) -> Token {
        Token {
            kind,
            text: text.into(),
            span: Span {
                file: 1,
                start: 2,
                end: 3,
            },
            scopes: vec![],
            pre_resolved: None,
        }
    }

    fn sample() -> RawRecord {
        let mut a = tok(TokenKind::Ident, "connect");
        a.scopes = vec![9, 10];
        a.pre_resolved = Some(DefId {
            package: 3,
            item: 0,
        });
        RawRecord {
            key: [7; 32],
            fuel_consumed: 42,
            peak_memory_bytes: 65536,
            outcome: Outcome {
                tokens: vec![
                    a,
                    tok(TokenKind::Open(Delimiter::Paren), "("),
                    tok(TokenKind::Close(Delimiter::Paren), ")"),
                ],
                diagnostics: vec![Diagnostic {
                    severity: Severity::Note,
                    message: "ok".into(),
                    span: None,
                }],
            },
            stdout: b"out".to_vec(),
            stderr: vec![],
        }
    }

    #[test]
    fn round_trips_canonically() {
        let r = sample();
        let bytes = r.encode();
        assert_eq!(bytes.len(), r.encoded_len());
        assert_eq!(RawRecord::decode(&bytes, &limits()).unwrap(), r);
    }

    #[test]
    fn every_single_byte_flip_is_rejected_or_changes_the_value() {
        let r = sample();
        let bytes = r.encode();
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x80;
            if let Ok(d) = RawRecord::decode(&b, &limits()) {
                assert_ne!(d, r, "flip at {i} decoded to the same record");
            }
        }
    }

    #[test]
    fn truncation_and_extension_are_rejected() {
        let bytes = sample().encode();
        for n in 0..bytes.len() {
            assert!(RawRecord::decode(&bytes[..n], &limits()).is_err());
        }
        let mut long = bytes.clone();
        long.push(0);
        assert!(matches!(
            RawRecord::decode(&long, &limits()),
            Err(RecordError::Length { .. })
        ));
    }

    #[test]
    fn counts_are_checked_before_allocation() {
        let mut bytes = sample().encode();
        // Claim u32::MAX tokens: must fail on the count, not by allocating.
        let at = 4 + 4 + 32 + 8 + 8;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            RawRecord::decode(&bytes, &limits()),
            Err(RecordError::OverLimit {
                field: "tokens",
                ..
            })
        ));
        let mut small = limits();
        small.max_record_bytes = 10;
        assert!(matches!(
            RawRecord::decode(&sample().encode(), &small),
            Err(RecordError::TooLarge { .. })
        ));
    }

    #[test]
    fn stored_error_diagnostics_and_bad_nesting_are_rejected() {
        let mut r = sample();
        r.outcome.diagnostics[0].severity = Severity::Error;
        assert_eq!(
            RawRecord::decode(&r.encode(), &limits()),
            Err(RecordError::ErrorDiagnostic)
        );
        let mut r = sample();
        r.outcome.tokens.pop();
        assert!(matches!(
            RawRecord::decode(&r.encode(), &limits()),
            Err(RecordError::Transport(_))
        ));
    }
}
