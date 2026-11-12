//! Checked, bounded host rendering of supplied compiler diagnostic evidence.
//! Validation does not establish semantic causality or proof authority.
#![forbid(unsafe_code)]
use ariadne::{Config, IndexType, Label, Report, ReportKind, Source};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    mem::size_of,
};
use vesper_core::ast::Diagnostic;
use vesper_span::{ByteSpan, DiagnosticData, Diagnostics, LineIndex, Range, Resource, Severity};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub span: vesper_span::Limits,
    pub max_name_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            span: vesper_span::Limits {
                max_work: 64 * 1024 * 1024,
                max_edits: 0,
                max_fixes: 0,
                ..Default::default()
            },
            max_name_bytes: 4096,
        }
    }
}
impl Limits {
    pub const HARD: Self = Self {
        span: vesper_span::Limits::HARD,
        max_name_bytes: 4096,
    };
    pub fn validate(self) -> Result<(), Error> {
        self.span.validate().map_err(Error::Span)?;
        if self.max_name_bytes > Self::HARD.max_name_bytes {
            return Err(Error::InvalidLimits);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidLimits,
    Span(vesper_span::Error),
    Name,
    Allocation,
    Output,
    UnsupportedLineEnding,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedLineEnding => f.write_str(
                "terminal source contains a line separator unsupported by the shared line profile",
            ),
            _ => write!(f, "diagnostic rendering {self:?}"),
        }
    }
}
impl std::error::Error for Error {}
fn checked_add(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
    a.checked_add(b)
        .ok_or(Error::Span(vesper_span::Error::Budget(r)))
}
fn checked_mul(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
    a.checked_mul(b)
        .ok_or(Error::Span(vesper_span::Error::Budget(r)))
}
fn bound(n: usize, max: usize, r: Resource) -> Result<(), Error> {
    if n > max {
        Err(Error::Span(vesper_span::Error::Budget(r)))
    } else {
        Ok(())
    }
}
fn copy(text: &str) -> Result<String, Error> {
    let mut s = String::new();
    s.try_reserve_exact(text.len())
        .map_err(|_| Error::Allocation)?;
    s.push_str(text);
    Ok(s)
}
fn span(span: &std::ops::Range<usize>) -> ByteSpan {
    ByteSpan {
        start: span.start,
        end: span.end,
    }
}
fn valid_span(text: &str, span: ByteSpan) -> Result<(), Error> {
    let fail = |e| Error::Span(e);
    if span.start > span.end {
        return Err(fail(vesper_span::Error::ReversedRange));
    }
    if span.end > text.len() {
        return Err(fail(vesper_span::Error::ByteOutOfBounds));
    }
    for offset in [span.start, span.end] {
        if !text.is_char_boundary(offset) {
            return Err(fail(vesper_span::Error::Utf8Boundary));
        }
        if offset > 0
            && text.as_bytes().get(offset - 1) == Some(&b'\r')
            && text.as_bytes().get(offset) == Some(&b'\n')
        {
            return Err(fail(vesper_span::Error::LineEndingInterior));
        }
    }
    Ok(())
}
/// Immutable checked data tied to the exact borrowed index/source. No fixes are
/// synthesized. The supplied labels remain producer assertions, not proofs.
#[derive(Debug)]
pub struct Prepared<'a> {
    index: &'a LineIndex,
    data: Diagnostics,
    limits: Limits,
    label_count: usize,
    text_bytes: usize,
}
/// Checks every aggregate, span, text and count before copying any metadata.
/// The resulting DTO is also checked by vesper_span::Diagnostics.
pub fn prepare<'a>(
    index: &'a LineIndex,
    diagnostics: &[Diagnostic],
    limits: Limits,
) -> Result<Prepared<'a>, Error> {
    limits.validate()?;
    let cap = limits.span;
    bound(index.text().len(), cap.max_source_bytes, Resource::Source)?;
    bound(index.text().len(), cap.max_work, Resource::Work)?;
    let eof = index.position(index.text().len()).map_err(Error::Span)?;
    bound(eof.line as usize + 1, cap.max_lines, Resource::Lines)?;
    bound(
        diagnostics.len(),
        cap.max_diagnostics,
        Resource::Diagnostics,
    )?;
    bound(diagnostics.len(), cap.max_work, Resource::Work)?;
    let (mut labels, mut text) = (0usize, 0usize);
    for d in diagnostics {
        if d.code.is_empty() || d.message.is_empty() {
            return Err(Error::Span(vesper_span::Error::InvalidDiagnostic));
        }
        labels = checked_add(
            labels,
            checked_add(1, d.related.len(), Resource::Labels)?,
            Resource::Labels,
        )?;
        bound(labels, cap.max_labels, Resource::Labels)?;
        bound(
            checked_add(labels, diagnostics.len(), Resource::Work)?,
            cap.max_work,
            Resource::Work,
        )?;
        text = checked_add(text, d.code.len(), Resource::Text)?;
        text = checked_add(text, d.message.len(), Resource::Text)?;
        text = checked_add(text, d.help.as_ref().map_or(0, String::len), Resource::Text)?;
        valid_span(index.text(), span(&d.span))?;
        for label in &d.related {
            valid_span(index.text(), span(&label.span))?;
            text = checked_add(text, label.message.len(), Resource::Text)?;
        }
        bound(text, cap.max_text_bytes, Resource::Text)?;
    }
    let metadata = checked_add(
        checked_mul(
            diagnostics.len(),
            size_of::<DiagnosticData>(),
            Resource::Metadata,
        )?,
        checked_add(
            checked_mul(labels, size_of::<vesper_span::Label>(), Resource::Metadata)?,
            text,
            Resource::Metadata,
        )?,
        Resource::Metadata,
    )?;
    // Both temporary DTOs and the final checked copy are present briefly.
    bound(
        checked_mul(metadata, 2, Resource::Metadata)?,
        cap.max_metadata_bytes,
        Resource::Metadata,
    )?;
    let work = checked_add(
        index.text().len(),
        checked_add(
            checked_mul(
                checked_mul(index.text().len(), 2, Resource::Work)?,
                labels,
                Resource::Work,
            )?,
            checked_add(
                checked_mul(text, 4, Resource::Work)?,
                checked_add(labels, diagnostics.len(), Resource::Work)?,
                Resource::Work,
            )?,
            Resource::Work,
        )?,
        Resource::Work,
    )?;
    bound(work, cap.max_work, Resource::Work)?;
    let mut data = Vec::new();
    data.try_reserve_exact(diagnostics.len())
        .map_err(|_| Error::Allocation)?;
    for d in diagnostics {
        let mut labels = Vec::new();
        labels
            .try_reserve_exact(d.related.len() + 1)
            .map_err(|_| Error::Allocation)?;
        labels.push(vesper_span::Label {
            span: span(&d.span),
            message: String::new(),
            primary: true,
        });
        for label in &d.related {
            labels.push(vesper_span::Label {
                span: span(&label.span),
                message: copy(&label.message)?,
                primary: false,
            });
        }
        data.push(DiagnosticData {
            code: copy(&d.code)?,
            message: copy(&d.message)?,
            severity: Severity::Error,
            labels,
            help: d.help.as_deref().map(copy).transpose()?,
            fixes: Vec::new(),
        });
    }
    let data = Diagnostics::new(index, &data, cap).map_err(Error::Span)?;
    Ok(Prepared {
        index,
        data,
        limits,
        label_count: labels,
        text_bytes: text,
    })
}
impl Prepared<'_> {
    pub fn source(&self) -> &str {
        self.index.text()
    }
    pub fn data(&self) -> &[DiagnosticData] {
        self.data.data()
    }
    fn name(&self, name: &str) -> Result<(), Error> {
        if name.is_empty() || name.len() > self.limits.max_name_bytes {
            return Err(Error::Name);
        }
        bound(name.len(), self.limits.span.max_work, Resource::Work)?;
        if name.chars().any(char::is_control) {
            return Err(Error::Name);
        }
        Ok(())
    }
    /// Standard LSP diagnostics with actual relatedInformation and optional
    /// bounded data.help. The entire result is preflighted before DOM copies.
    pub fn to_lsp(&self, uri: &str) -> Result<Vec<Value>, Error> {
        self.name(uri)?;
        let cap = self.limits.span;
        let names = checked_mul(uri.len(), self.label_count, Resource::Work)?;
        let shape = checked_mul(
            checked_add(self.label_count, self.data().len(), Resource::Work)?,
            512,
            Resource::Work,
        )?;
        let work = checked_add(
            checked_mul(
                checked_mul(self.source().len(), 4, Resource::Work)?,
                self.label_count,
                Resource::Work,
            )?,
            checked_add(
                checked_mul(
                    checked_add(self.text_bytes, names, Resource::Work)?,
                    32,
                    Resource::Work,
                )?,
                shape,
                Resource::Work,
            )?,
            Resource::Work,
        )?;
        bound(work, cap.max_work, Resource::Work)?;
        let mut bytes = 2usize;
        let mut labels = 0usize;
        let mut text = 0usize;
        for (at, d) in self.data().iter().enumerate() {
            if at > 0 {
                bytes = checked_add(bytes, 1, Resource::Output)?;
            }
            let primary = self.index.range(d.labels[0].span).map_err(Error::Span)?;
            let mut n = checked_add(b"{\"range\":".len(), range_size(primary), Resource::Output)?;
            n = checked_add(
                n,
                b",\"severity\":1,\"code\":".len() + quoted(&d.code)?,
                Resource::Output,
            )?;
            n = checked_add(
                n,
                b",\"source\":\"vesper\",\"message\":".len() + quoted(&d.message)?,
                Resource::Output,
            )?;
            text = checked_add(text, d.code.len() + d.message.len(), Resource::Metadata)?;
            if d.labels.len() > 1 {
                n = checked_add(n, b",\"relatedInformation\":[".len(), Resource::Output)?;
                for (i, label) in d.labels[1..].iter().enumerate() {
                    let r = self.index.range(label.span).map_err(Error::Span)?;
                    if i > 0 {
                        n = checked_add(n, 1, Resource::Output)?;
                    }
                    n = checked_add(
                        n,
                        b"{\"location\":{\"uri\":".len()
                            + quoted(uri)?
                            + b",\"range\":".len()
                            + range_size(r)
                            + b"},\"message\":".len()
                            + quoted(&label.message)?
                            + 1,
                        Resource::Output,
                    )?;
                    text = checked_add(text, uri.len() + label.message.len(), Resource::Metadata)?;
                }
                n = checked_add(n, 1, Resource::Output)?;
            }
            if let Some(help) = &d.help {
                n = checked_add(
                    n,
                    b",\"data\":{\"help\":".len() + quoted(help)? + 1,
                    Resource::Output,
                )?;
                text = checked_add(text, help.len(), Resource::Metadata)?;
            }
            bytes = checked_add(
                bytes,
                checked_add(n, 1, Resource::Output)?,
                Resource::Output,
            )?;
            labels = checked_add(labels, d.labels.len(), Resource::Labels)?;
        }
        if bytes > cap.max_output_bytes {
            return Err(Error::Output);
        }
        let nodes = checked_add(
            checked_mul(self.data().len(), 24, Resource::Metadata)?,
            checked_mul(labels, 20, Resource::Metadata)?,
            Resource::Metadata,
        )?;
        bound(
            checked_add(
                checked_mul(nodes, size_of::<Value>(), Resource::Metadata)?,
                text,
                Resource::Metadata,
            )?,
            cap.max_metadata_bytes,
            Resource::Metadata,
        )?;
        // Span conversion, escaping, URI copies and DOM shape were charged
        // before traversal; output and metadata are now checked before copies.

        let mut output = Vec::new();
        output
            .try_reserve_exact(self.data().len())
            .map_err(|_| Error::Allocation)?;
        for d in self.data() {
            let primary = self.index.range(d.labels[0].span).map_err(Error::Span)?;
            let mut value = json!({"range":range_value(primary),"severity":1,"code":d.code,"source":"vesper","message":d.message});
            if d.labels.len() > 1 {
                let mut related = Vec::new();
                related
                    .try_reserve_exact(d.labels.len() - 1)
                    .map_err(|_| Error::Allocation)?;
                for label in &d.labels[1..] {
                    let range = self.index.range(label.span).map_err(Error::Span)?;
                    related.push(json!({"location":{"uri":uri,"range":range_value(range)},"message":label.message}));
                }
                value["relatedInformation"] = Value::Array(related);
            }
            if let Some(help) = &d.help {
                value["data"] = json!({"help":help});
            }
            output.push(value);
        }
        Ok(output)
    }
    /// Complete private terminal output, with Ariadne explicitly in byte mode.
    /// No external writer or filesystem cache is used.
    pub fn terminal(&self, file: &str) -> Result<Vec<u8>, Error> {
        self.name(file)?;
        if self.data().is_empty() {
            return Ok(Vec::new());
        }
        let cap = self.limits.span;
        bound(
            checked_mul(self.source().len(), 2, Resource::Work)?,
            cap.max_work,
            Resource::Work,
        )?;
        // Ariadne recognizes additional Unicode separators as new lines. Refuse
        // them rather than publish coordinates inconsistent with LineIndex.
        if self
            .source()
            .chars()
            .any(|c| matches!(c, '\x0b' | '\x0c' | '\u{85}' | '\u{2028}' | '\u{2029}'))
        {
            return Err(Error::UnsupportedLineEnding);
        }
        let mut work = checked_add(
            checked_mul(self.source().len(), 2, Resource::Work)?,
            checked_add(
                checked_mul(self.text_bytes, 4, Resource::Work)?,
                checked_mul(
                    checked_mul(
                        file.len(),
                        self.label_count + self.data().len(),
                        Resource::Work,
                    )?,
                    8,
                    Resource::Work,
                )?,
                Resource::Work,
            )?,
            Resource::Work,
        )?;
        let mut lines = 1usize;
        let mut cr = false;
        for b in self.source().bytes() {
            if b == b'\r' || (b == b'\n' && !cr) {
                lines += 1;
            }
            cr = b == b'\r';
        }
        bound(lines, cap.max_lines, Resource::Lines)?;
        let mut metadata = checked_mul(
            checked_mul(lines, 2, Resource::Metadata)?,
            size_of::<ariadne::Line>(),
            Resource::Metadata,
        )?;
        metadata = checked_add(
            metadata,
            checked_mul(self.source().len() + 1, 4, Resource::Metadata)?,
            Resource::Metadata,
        )?;
        metadata = checked_add(
            metadata,
            checked_mul(self.text_bytes, 4, Resource::Metadata)?,
            Resource::Metadata,
        )?;
        for d in self.data() {
            let k = d.labels.len() + 1;
            let square = checked_mul(k, k, Resource::Work)?;
            work = checked_add(
                work,
                checked_add(
                    checked_mul(self.source().len() + 1, square, Resource::Work)?,
                    checked_mul(square, square, Resource::Work)?,
                    Resource::Work,
                )?,
                Resource::Work,
            )?;
            metadata = checked_add(
                metadata,
                checked_mul(
                    d.labels.len(),
                    size_of::<Label<(&str, std::ops::Range<usize>)>>() * 16,
                    Resource::Metadata,
                )?,
                Resource::Metadata,
            )?;
            work = checked_add(
                work,
                d.code.len() + d.message.len() + d.help.as_ref().map_or(0, String::len),
                Resource::Work,
            )?;
            for label in &d.labels {
                work = checked_add(work, label.message.len(), Resource::Work)?;
            }
        }
        bound(work, cap.max_work, Resource::Work)?;
        bound(metadata, cap.max_metadata_bytes, Resource::Metadata)?;
        // A trailing source terminator creates an actual empty EOF line in the
        // shared index. Ariadne omits it; one display-only blank makes that line
        // addressable without changing any original byte or validated offset.
        let padded = self.source().ends_with(['\r', '\n']);
        let mut display = String::new();
        display
            .try_reserve_exact(self.source().len() + usize::from(padded))
            .map_err(|_| Error::Allocation)?;
        display.push_str(self.source());
        if padded {
            display.push(' ');
        }
        let source = Source::from(display.as_str());
        let mut output = Output {
            bytes: Vec::new(),
            limit: cap.max_output_bytes,
            failure: None,
        };
        for d in self.data() {
            let primary = d.labels[0].span;
            let mut report = Report::build(ReportKind::Error, (file, primary.start..primary.end))
                .with_code(&d.code)
                .with_message(&d.message)
                .with_config(
                    Config::default()
                        .with_color(false)
                        .with_index_type(IndexType::Byte),
                );
            for (at, label) in d.labels.iter().enumerate() {
                let mut label =
                    Label::new((file, label.span.start..label.span.end)).with_order(at as i32);
                if at == 0 {
                    label = label.with_message(&d.message);
                } else {
                    label = label.with_message(&d.labels[at].message);
                }
                report = report.with_label(label);
            }
            if let Some(help) = &d.help {
                report = report.with_help(help);
            }
            if report.finish().write((file, &source), &mut output).is_err() {
                return Err(output.failure.unwrap_or(Error::Output));
            }
        }
        Ok(output.bytes)
    }
}
fn range_value(r: Range) -> Value {
    json!({"start":{"line":r.start.line,"character":r.start.character},"end":{"line":r.end.line,"character":r.end.character}})
}
fn digits(n: u32) -> usize {
    n.checked_ilog10().map_or(1, |n| n as usize + 1)
}
fn range_size(r: Range) -> usize {
    b"{\"start\":{\"line\":".len()
        + digits(r.start.line)
        + b",\"character\":".len()
        + digits(r.start.character)
        + b"},\"end\":{\"line\":".len()
        + digits(r.end.line)
        + b",\"character\":".len()
        + digits(r.end.character)
        + 2
}
fn quoted(s: &str) -> Result<usize, Error> {
    let mut size = 2;
    for b in s.bytes() {
        size = checked_add(
            size,
            match b {
                b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
                0..=0x1f => 6,
                _ => 1,
            },
            Resource::Output,
        )?;
    }
    Ok(size)
}
struct Output {
    bytes: Vec<u8>,
    limit: usize,
    failure: Option<Error>,
}
impl Write for Output {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(b.len())
            .is_none_or(|n| n > self.limit)
        {
            self.failure = Some(Error::Output);
            return Err(io::Error::other("diagnostic output cap"));
        }
        if self.bytes.try_reserve(b.len()).is_err() {
            self.failure = Some(Error::Allocation);
            return Err(io::Error::other("diagnostic allocation"));
        }
        self.bytes.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
