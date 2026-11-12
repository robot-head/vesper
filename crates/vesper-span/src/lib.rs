//! Safe bounded source coordinates. Labels are supplied data, not proof of causality.
//! No I/O, compiler execution, or clamping is performed.
#![forbid(unsafe_code)]

use std::{fmt, mem::size_of, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Source,
    Lines,
    Metadata,
    Work,
    Edits,
    Output,
    Diagnostics,
    Labels,
    Fixes,
    Text,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    Budget(Resource),
    LineOutOfBounds,
    CharacterOutOfBounds,
    ByteOutOfBounds,
    Utf8Boundary,
    SurrogateInterior,
    LineEndingInterior,
    ReversedRange,
    OverlappingEdits,
    SourceMismatch,
    InvalidDiagnostic,
    Allocation,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Logical budgets, not allocator/RSS or elapsed-time guarantees. Configurations
/// exceeding these same fields in `Limits::HARD` fail before traversal/allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_lines: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
    pub max_edits: usize,
    pub max_output_bytes: usize,
    pub max_diagnostics: usize,
    pub max_labels: usize,
    pub max_fixes: usize,
    pub max_text_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024,
            max_lines: 32769,
            max_metadata_bytes: 1024 * 1024,
            max_work: 2 * 1024 * 1024,
            max_edits: 256,
            max_output_bytes: 256 * 1024,
            max_diagnostics: 256,
            max_labels: 1024,
            max_fixes: 256,
            max_text_bytes: 128 * 1024,
        }
    }
}
impl Limits {
    pub const HARD: Self = Self {
        max_source_bytes: 1024 * 1024,
        max_lines: 262145,
        max_metadata_bytes: 16 * 1024 * 1024,
        max_work: 64 * 1024 * 1024,
        max_edits: 4096,
        max_output_bytes: 2 * 1024 * 1024,
        max_diagnostics: 4096,
        max_labels: 16384,
        max_fixes: 4096,
        max_text_bytes: 2 * 1024 * 1024,
    };
    pub fn validate(self) -> Result<(), Error> {
        let a = [
            self.max_source_bytes,
            self.max_lines,
            self.max_metadata_bytes,
            self.max_work,
            self.max_edits,
            self.max_output_bytes,
            self.max_diagnostics,
            self.max_labels,
            self.max_fixes,
            self.max_text_bytes,
        ];
        let b = [
            Self::HARD.max_source_bytes,
            Self::HARD.max_lines,
            Self::HARD.max_metadata_bytes,
            Self::HARD.max_work,
            Self::HARD.max_edits,
            Self::HARD.max_output_bytes,
            Self::HARD.max_diagnostics,
            Self::HARD.max_labels,
            Self::HARD.max_fixes,
            Self::HARD.max_text_bytes,
        ];
        if a.iter().zip(b).any(|(a, b)| *a > b) {
            return Err(Error::InvalidLimits);
        }
        Ok(())
    }
}
fn bound(n: usize, cap: usize, resource: Resource) -> Result<(), Error> {
    if n > cap {
        Err(Error::Budget(resource))
    } else {
        Ok(())
    }
}
fn add(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
    a.checked_add(b).ok_or(Error::Budget(r))
}
fn mul(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
    a.checked_mul(b).ok_or(Error::Budget(r))
}
struct Work {
    used: usize,
    cap: usize,
}
impl Work {
    fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.used = add(self.used, n, Resource::Work)?;
        bound(self.used, self.cap, Resource::Work)
    }
}
#[derive(Debug)]
struct Line {
    start: usize,
    end: usize,
}

/// CRLF is one terminator. Its CR offset is the preceding content end; the
/// interior LF offset is unrepresentable. Lone CR/LF also terminate a line.
/// A trailing terminator creates an empty final line; EOF is always addressable.
#[derive(Debug)]
pub struct LineIndex {
    text: Arc<str>,
    lines: Vec<Line>,
    limits: Limits,
}
impl LineIndex {
    pub fn new(text: Arc<str>, limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        bound(text.len(), limits.max_source_bytes, Resource::Source)?;
        bound(
            mul(text.len(), 2, Resource::Work)?,
            limits.max_work,
            Resource::Work,
        )?;
        let mut work = Work {
            used: 0,
            cap: limits.max_work,
        };
        // Count and validate all storage before allocating the line table.
        let mut count = 1usize;
        scan_lines(&text, &mut work, |_, _| {
            count = add(count, 1, Resource::Lines)?;
            Ok(())
        })?;
        bound(count, limits.max_lines, Resource::Lines)?;
        bound(
            mul(count, size_of::<Line>(), Resource::Metadata)?,
            limits.max_metadata_bytes,
            Resource::Metadata,
        )?;
        let mut lines = Vec::new();
        lines
            .try_reserve_exact(count)
            .map_err(|_| Error::Allocation)?;
        let mut start = 0;
        scan_lines(&text, &mut work, |end, next| {
            lines.push(Line { start, end });
            start = next;
            Ok(())
        })?;
        lines.push(Line {
            start,
            end: text.len(),
        });
        Ok(Self {
            text,
            lines,
            limits,
        })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn offset(&self, p: Position) -> Result<usize, Error> {
        let line = self
            .lines
            .get(usize::try_from(p.line).map_err(|_| Error::LineOutOfBounds)?)
            .ok_or(Error::LineOutOfBounds)?;
        let mut utf16 = 0u32;
        let mut work = Work {
            used: 0,
            cap: self.limits.max_work,
        };
        for (offset, c) in self
            .text
            .get(line.start..line.end)
            .ok_or(Error::Utf8Boundary)?
            .char_indices()
        {
            work.charge(c.len_utf8())?;
            if utf16 == p.character {
                return Ok(line.start + offset);
            }
            let next = utf16
                .checked_add(u32::try_from(c.len_utf16()).map_err(|_| Error::CharacterOutOfBounds)?)
                .ok_or(Error::CharacterOutOfBounds)?;
            if p.character < next {
                return Err(Error::SurrogateInterior);
            }
            utf16 = next;
        }
        if utf16 == p.character {
            Ok(line.end)
        } else {
            Err(Error::CharacterOutOfBounds)
        }
    }
    pub fn position(&self, offset: usize) -> Result<Position, Error> {
        if offset > self.text.len() {
            return Err(Error::ByteOutOfBounds);
        }
        if !self.text.is_char_boundary(offset) {
            return Err(Error::Utf8Boundary);
        }
        let i = self
            .lines
            .partition_point(|l| l.start <= offset)
            .checked_sub(1)
            .ok_or(Error::ByteOutOfBounds)?;
        let line = &self.lines[i];
        if offset > line.end {
            return Err(Error::LineEndingInterior);
        }
        let prefix = self
            .text
            .get(line.start..offset)
            .ok_or(Error::Utf8Boundary)?;
        bound(prefix.len(), self.limits.max_work, Resource::Work)?;
        let mut character = 0u32;
        for c in prefix.chars() {
            character = character
                .checked_add(u32::try_from(c.len_utf16()).map_err(|_| Error::CharacterOutOfBounds)?)
                .ok_or(Error::CharacterOutOfBounds)?;
        }
        Ok(Position {
            line: u32::try_from(i).map_err(|_| Error::LineOutOfBounds)?,
            character,
        })
    }
    pub fn range(&self, span: ByteSpan) -> Result<Range, Error> {
        if span.start > span.end {
            return Err(Error::ReversedRange);
        }
        Ok(Range {
            start: self.position(span.start)?,
            end: self.position(span.end)?,
        })
    }
    pub fn span(&self, range: Range) -> Result<ByteSpan, Error> {
        if (range.start.line, range.start.character) > (range.end.line, range.end.character) {
            return Err(Error::ReversedRange);
        }
        Ok(ByteSpan {
            start: self.offset(range.start)?,
            end: self.offset(range.end)?,
        })
    }
}
fn scan_lines(
    text: &str,
    work: &mut Work,
    mut f: impl FnMut(usize, usize) -> Result<(), Error>,
) -> Result<(), Error> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        work.charge(1)?;
        match bytes[i] {
            b'\r' => {
                let end = i;
                i += 1;
                if bytes.get(i) == Some(&b'\n') {
                    work.charge(1)?;
                    i += 1;
                }
                f(end, i)?
            }
            b'\n' => {
                let end = i;
                i += 1;
                f(end, i)?
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// Expected bytes must match the replaced span. An empty insertion has no old
/// bytes to compare; the validated batch always retains its constructor snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEdit {
    pub span: ByteSpan,
    pub expected: String,
    pub replacement: String,
}
/// Validated immutable edits tied to an exact immutable source snapshot.
#[derive(Debug)]
pub struct SourceEdits {
    source: Arc<str>,
    edits: Vec<SourceEdit>,
    output_bytes: usize,
}
impl SourceEdits {
    pub fn new(index: &LineIndex, edits: &[SourceEdit], limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        let mut work = Work {
            used: 0,
            cap: limits.max_work,
        };
        let (output_bytes, _) = validate_edits(index, edits, limits, &mut work)?;
        let owned = copy_edits(edits)?;
        Ok(Self {
            source: Arc::clone(&index.text),
            edits: owned,
            output_bytes,
        })
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn edits(&self) -> &[SourceEdit] {
        &self.edits
    }
    pub fn apply(&self) -> Result<String, Error> {
        let mut out = String::new();
        out.try_reserve_exact(self.output_bytes)
            .map_err(|_| Error::Allocation)?;
        let mut cursor = 0;
        for edit in &self.edits {
            out.push_str(
                self.source
                    .get(cursor..edit.span.start)
                    .ok_or(Error::Utf8Boundary)?,
            );
            out.push_str(&edit.replacement);
            cursor = edit.span.end;
        }
        out.push_str(self.source.get(cursor..).ok_or(Error::Utf8Boundary)?);
        Ok(out)
    }
}
fn copy_string(s: &str) -> Result<String, Error> {
    let mut out = String::new();
    out.try_reserve_exact(s.len())
        .map_err(|_| Error::Allocation)?;
    out.push_str(s);
    Ok(out)
}
fn copy_edits(edits: &[SourceEdit]) -> Result<Vec<SourceEdit>, Error> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(edits.len())
        .map_err(|_| Error::Allocation)?;
    for e in edits {
        owned.push(SourceEdit {
            span: e.span,
            expected: copy_string(&e.expected)?,
            replacement: copy_string(&e.replacement)?,
        });
    }
    owned.sort_unstable_by_key(|e| (e.span.start, e.span.end));
    Ok(owned)
}
fn validate_edits(
    index: &LineIndex,
    edits: &[SourceEdit],
    limits: Limits,
    work: &mut Work,
) -> Result<(usize, usize), Error> {
    bound(index.text.len(), limits.max_source_bytes, Resource::Source)?;
    bound(edits.len(), limits.max_edits, Resource::Edits)?;
    let mut text = 0;
    let mut removed = 0;
    let mut inserted = 0;
    for (i, e) in edits.iter().enumerate() {
        work.charge(add(e.expected.len(), e.replacement.len(), Resource::Work)?)?;
        work.charge(mul(index.text.len(), 2, Resource::Work)?)?; // both endpoint scans
        index.range(e.span)?;
        if index.text.get(e.span.start..e.span.end) != Some(e.expected.as_str()) {
            return Err(Error::SourceMismatch);
        }
        text = add(
            text,
            add(e.expected.len(), e.replacement.len(), Resource::Text)?,
            Resource::Text,
        )?;
        removed = add(removed, e.span.end - e.span.start, Resource::Output)?;
        inserted = add(inserted, e.replacement.len(), Resource::Output)?;
        for other in &edits[..i] {
            work.charge(1)?;
            // Adjacent nonempty edits are allowed. Insertions at any shared
            // boundary are rejected, avoiding caller-order-dependent results.
            let a = e.span;
            let b = other.span;
            if a.start == b.start
                || (a.start < b.end && b.start < a.end)
                || (a.start == a.end && a.start == b.end)
                || (b.start == b.end && b.start == a.end)
            {
                return Err(Error::OverlappingEdits);
            }
        }
    }
    bound(text, limits.max_text_bytes, Resource::Text)?;
    let metadata = add(
        mul(edits.len(), size_of::<SourceEdit>(), Resource::Metadata)?,
        text,
        Resource::Metadata,
    )?;
    bound(metadata, limits.max_metadata_bytes, Resource::Metadata)?;
    let output = add(
        index
            .text
            .len()
            .checked_sub(removed)
            .ok_or(Error::OverlappingEdits)?,
        inserted,
        Resource::Output,
    )?;
    bound(output, limits.max_output_bytes, Resource::Output)?;
    work.charge(add(index.text.len(), output, Resource::Work)?)?;
    work.charge(text)?;
    work.charge(mul(edits.len(), edits.len(), Resource::Work)?)?;
    Ok((output, metadata))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub span: ByteSpan,
    pub message: String,
    pub primary: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub id: String,
    pub title: String,
    pub edits: Vec<SourceEdit>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticData {
    pub code: String,
    pub message: String,
    pub severity: Severity,
    pub labels: Vec<Label>,
    pub help: Option<String>,
    pub fixes: Vec<Fix>,
}
/// Checked transport data for one exact source snapshot. It does not establish
/// compiler provenance, causality, or the semantic correctness of fixes.
#[derive(Debug)]
pub struct Diagnostics {
    source: Arc<str>,
    data: Vec<DiagnosticData>,
}
impl Diagnostics {
    pub fn new(index: &LineIndex, data: &[DiagnosticData], limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        bound(index.text.len(), limits.max_source_bytes, Resource::Source)?;
        bound(data.len(), limits.max_diagnostics, Resource::Diagnostics)?;
        let mut labels = 0;
        let mut fixes = 0;
        let mut edit_count = 0;
        let mut text = 0;
        let mut metadata = mul(data.len(), size_of::<DiagnosticData>(), Resource::Metadata)?;
        let mut work = Work {
            used: 0,
            cap: limits.max_work,
        };
        for d in data {
            if d.code.is_empty() || d.message.is_empty() || d.labels.is_empty() {
                return Err(Error::InvalidDiagnostic);
            }
            labels = add(labels, d.labels.len(), Resource::Labels)?;
            bound(labels, limits.max_labels, Resource::Labels)?;
            if !d.labels.iter().any(|l| l.primary) {
                return Err(Error::InvalidDiagnostic);
            }
            fixes = add(fixes, d.fixes.len(), Resource::Fixes)?;
            bound(fixes, limits.max_fixes, Resource::Fixes)?;
            text = add(
                text,
                add(d.code.len(), d.message.len(), Resource::Text)?,
                Resource::Text,
            )?;
            text = add(text, d.help.as_ref().map_or(0, String::len), Resource::Text)?;
            metadata = add(
                metadata,
                mul(d.labels.len(), size_of::<Label>(), Resource::Metadata)?,
                Resource::Metadata,
            )?;
            metadata = add(
                metadata,
                mul(d.fixes.len(), size_of::<Fix>(), Resource::Metadata)?,
                Resource::Metadata,
            )?;
            for label in &d.labels {
                work.charge(mul(index.text.len(), 2, Resource::Work)?)?;
                index.range(label.span)?;
                text = add(text, label.message.len(), Resource::Text)?;
            }
            for fix in &d.fixes {
                if fix.id.is_empty() || fix.title.is_empty() || fix.edits.is_empty() {
                    return Err(Error::InvalidDiagnostic);
                }
                edit_count = add(edit_count, fix.edits.len(), Resource::Edits)?;
                bound(edit_count, limits.max_edits, Resource::Edits)?;
                let (_, m) = validate_edits(index, &fix.edits, limits, &mut work)?;
                metadata = add(metadata, m, Resource::Metadata)?;
                text = add(
                    text,
                    add(fix.id.len(), fix.title.len(), Resource::Text)?,
                    Resource::Text,
                )?;
                for e in &fix.edits {
                    text = add(
                        text,
                        add(e.expected.len(), e.replacement.len(), Resource::Text)?,
                        Resource::Text,
                    )?;
                }
            }
        }
        bound(text, limits.max_text_bytes, Resource::Text)?;
        work.charge(text)?;
        bound(
            add(metadata, text, Resource::Metadata)?,
            limits.max_metadata_bytes,
            Resource::Metadata,
        )?;
        // All checks precede clones; fallible bounded copies preserve the input.
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(data.len())
            .map_err(|_| Error::Allocation)?;
        for d in data {
            let mut ls = Vec::new();
            ls.try_reserve_exact(d.labels.len())
                .map_err(|_| Error::Allocation)?;
            for l in &d.labels {
                ls.push(Label {
                    span: l.span,
                    message: copy_string(&l.message)?,
                    primary: l.primary,
                });
            }
            let mut fs = Vec::new();
            fs.try_reserve_exact(d.fixes.len())
                .map_err(|_| Error::Allocation)?;
            for f in &d.fixes {
                let edits = copy_edits(&f.edits)?;
                fs.push(Fix {
                    id: copy_string(&f.id)?,
                    title: copy_string(&f.title)?,
                    edits,
                });
            }
            owned.push(DiagnosticData {
                code: copy_string(&d.code)?,
                message: copy_string(&d.message)?,
                severity: d.severity,
                labels: ls,
                help: d.help.as_deref().map(copy_string).transpose()?,
                fixes: fs,
            });
        }
        Ok(Self {
            source: Arc::clone(&index.text),
            data: owned,
        })
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn data(&self) -> &[DiagnosticData] {
        &self.data
    }
}
