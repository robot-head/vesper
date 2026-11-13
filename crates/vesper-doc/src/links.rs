//! Finite local intra-doc references. These public DTOs are inspection data,
//! never evidence that a compiler checked a source or a proof was audited.
use crate::{Document, Span};
use serde::{Deserialize, Serialize};
use std::{fmt, mem::size_of};
use vesper_syntax::lexer::{self, TokenKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Function,
    Type,
    InherentMethod,
    Effect,
    EffectOperation,
    Trait,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkTarget {
    pub path: String,
    pub kind: TargetKind,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkReference {
    pub span: Span,
    pub display: String,
    pub target: LinkTarget,
    pub anchor: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkDiagnostic {
    pub code: String,
    pub span: Span,
    pub message: String,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    #[default]
    NotAnalyzed,
    Resolved,
    Rejected,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkReport {
    pub status: LinkStatus,
    pub source_bytes: usize,
    pub targets: Vec<LinkTarget>,
    pub references: Vec<LinkReference>,
    pub diagnostics: Vec<LinkDiagnostic>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkLimits {
    pub max_source_bytes: usize,
    pub max_targets: usize,
    pub max_references: usize,
    pub max_path_bytes: usize,
    pub max_path_segments: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}
impl Default for LinkLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024,
            max_targets: 1024,
            max_references: 1024,
            max_path_bytes: 1024,
            max_path_segments: 8,
            max_metadata_bytes: 1024 * 1024,
            max_work: 16 * 1024 * 1024,
        }
    }
}
impl LinkLimits {
    pub const HARD: Self = Self {
        max_source_bytes: 256 * 1024,
        max_targets: 4096,
        max_references: 4096,
        max_path_bytes: 2048,
        max_path_segments: 16,
        max_metadata_bytes: 4 * 1024 * 1024,
        max_work: 64 * 1024 * 1024,
    };
    pub fn validate(self) -> Result<(), LinkError> {
        let h = Self::HARD;
        for (n, max) in [
            (self.max_source_bytes, h.max_source_bytes),
            (self.max_targets, h.max_targets),
            (self.max_references, h.max_references),
            (self.max_path_bytes, h.max_path_bytes),
            (self.max_path_segments, h.max_path_segments),
            (self.max_metadata_bytes, h.max_metadata_bytes),
            (self.max_work, h.max_work),
        ] {
            if n == 0 || n > max {
                return Err(error("configuration", "invalid intra-doc link limits"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkError {
    pub code: String,
    pub message: String,
}
impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "intra-doc {}: {}", self.code, self.message)
    }
}
impl std::error::Error for LinkError {}
fn error(code: &str, message: &str) -> LinkError {
    LinkError {
        code: code.into(),
        message: message.into(),
    }
}
fn resource() -> LinkError {
    error("resource", "link resource budget exceeded")
}
fn add(a: usize, b: usize) -> Result<usize, LinkError> {
    a.checked_add(b).ok_or_else(resource)
}
fn mul(a: usize, b: usize) -> Result<usize, LinkError> {
    a.checked_mul(b).ok_or_else(resource)
}
struct Budget {
    limits: LinkLimits,
    metadata: usize,
    work: usize,
}
impl Budget {
    fn new(limits: LinkLimits) -> Result<Self, LinkError> {
        limits.validate()?;
        Ok(Self {
            limits,
            metadata: 0,
            work: 0,
        })
    }
    fn work(&mut self, n: usize) -> Result<(), LinkError> {
        self.work = add(self.work, n)?;
        if self.work > self.limits.max_work {
            return Err(resource());
        }
        Ok(())
    }
    fn metadata(&mut self, n: usize) -> Result<(), LinkError> {
        self.metadata = add(self.metadata, n)?;
        if self.metadata > self.limits.max_metadata_bytes {
            return Err(resource());
        }
        self.work(n)
    }
    fn string(&mut self, s: &str) -> Result<String, LinkError> {
        self.metadata(s.len())?;
        let mut value = String::new();
        value.try_reserve_exact(s.len()).map_err(|_| resource())?;
        value.push_str(s);
        Ok(value)
    }
    fn push<T>(&mut self, v: &mut Vec<T>, value: T) -> Result<(), LinkError> {
        self.metadata(size_of::<T>())?;
        if v.len() == v.capacity() {
            self.work(mul(v.len(), size_of::<T>())?)?;
            v.try_reserve_exact(1).map_err(|_| resource())?;
        }
        v.push(value);
        Ok(())
    }
}
fn identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn local_path(s: &str) -> bool {
    !s.is_empty() && s.split("::").all(identifier)
}
fn check_path(s: &str, limits: LinkLimits) -> Result<(), LinkError> {
    if s.len() > limits.max_path_bytes || s.split("::").count() > limits.max_path_segments {
        return Err(resource());
    }
    Ok(())
}
fn target_valid(t: &LinkTarget, limits: LinkLimits) -> Result<(), LinkError> {
    check_path(&t.path, limits)?;
    let segments = t.path.split("::").count();
    let required = match t.kind {
        TargetKind::InherentMethod | TargetKind::EffectOperation => 2,
        _ => 1,
    };
    if !local_path(&t.path) || segments != required || t.path.split("::").any(|s| s == "Self") {
        return Err(error("invalid", "invalid local target descriptor"));
    }
    Ok(())
}
fn copy_target(t: &LinkTarget, budget: &mut Budget) -> Result<LinkTarget, LinkError> {
    Ok(LinkTarget {
        path: budget.string(&t.path)?,
        kind: t.kind,
    })
}
fn inventory(targets: &[LinkTarget], budget: &mut Budget) -> Result<Vec<LinkTarget>, LinkError> {
    if targets.len() > budget.limits.max_targets {
        return Err(resource());
    }
    let mut result = Vec::new();
    for t in targets {
        budget.work(add(t.path.len(), 1)?)?;
        target_valid(t, budget.limits)?;
        for earlier in &result {
            let earlier: &LinkTarget = earlier;
            budget.work(add(add(t.path.len(), earlier.path.len())?, 1)?)?;
            if earlier == t {
                return Err(error("invalid", "duplicate local target descriptor"));
            }
        }
        let target = copy_target(t, budget)?;
        budget.push(&mut result, target)?;
    }
    // A quadratic comparison envelope is finite even for an adversarial sort.
    let longest = targets.iter().map(|t| t.path.len()).max().unwrap_or(0);
    budget.work(mul(mul(targets.len(), targets.len())?, add(longest, 1)?)?)?;
    result.sort_unstable();
    Ok(result)
}

/// Stable ASCII fragment ID; the input is encoded, never interpolated as a URL.
pub fn anchor_id(kind: TargetKind, path: &str) -> String {
    let tag = match kind {
        TargetKind::Function => "function",
        TargetKind::Type => "type",
        TargetKind::InherentMethod => "method",
        TargetKind::Effect => "effect",
        TargetKind::EffectOperation => "operation",
        TargetKind::Trait => "trait",
    };
    let mut result = String::from("vesper-doc-");
    result.push_str(tag);
    result.push('-');
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in path.bytes() {
        result.push(HEX[usize::from(b >> 4)] as char);
        result.push(HEX[usize::from(b & 15)] as char);
    }
    result
}
fn anchor(t: &LinkTarget, budget: &mut Budget) -> Result<String, LinkError> {
    // Includes the longest fixed tag and geometric String growth/copy envelope.
    let bytes = add(32, mul(t.path.len(), 2)?)?;
    budget.metadata(mul(bytes, 4)?)?;
    Ok(anchor_id(t.kind, &t.path))
}

/// The facade constructs Document from actual checked compiler metadata. This
/// root-free projection itself makes no checked-source claim.
fn append_target(
    path: &str,
    kind: TargetKind,
    result: &mut Vec<LinkTarget>,
    budget: &mut Budget,
) -> Result<(), LinkError> {
    if result.len() >= budget.limits.max_targets {
        return Err(resource());
    }
    check_path(path, budget.limits)?;
    let target = LinkTarget {
        path: budget.string(path)?,
        kind,
    };
    target_valid(&target, budget.limits)?;
    budget.push(result, target)
}
pub fn document_targets(
    document: &Document,
    limits: LinkLimits,
) -> Result<Vec<LinkTarget>, LinkError> {
    let mut budget = Budget::new(limits)?;
    let mut result = Vec::new();
    for f in &document.functions {
        let kind = if f.name.contains("::") {
            TargetKind::InherentMethod
        } else {
            TargetKind::Function
        };
        append_target(&f.name, kind, &mut result, &mut budget)?;
    }
    for ty in &document.types {
        append_target(
            &ty.declaration.name,
            TargetKind::Type,
            &mut result,
            &mut budget,
        )?;
    }
    for effect in &document.effects {
        append_target(&effect.name, TargetKind::Effect, &mut result, &mut budget)?;
        for operation in &effect.operations {
            budget.work(add(add(operation.name.len(), effect.name.len())?, 1)?)?;
            let name = operation
                .name
                .strip_prefix(&effect.name)
                .and_then(|s| s.strip_prefix("::"))
                .unwrap_or(&operation.name);
            check_path(name, limits)?;
            let bytes = add(add(effect.name.len(), 2)?, name.len())?;
            if bytes > limits.max_path_bytes {
                return Err(resource());
            }
            budget.metadata(bytes)?;
            let mut path = String::new();
            path.try_reserve_exact(bytes).map_err(|_| resource())?;
            path.push_str(&effect.name);
            path.push_str("::");
            path.push_str(name);
            append_target(&path, TargetKind::EffectOperation, &mut result, &mut budget)?;
        }
    }
    for tr in &document.traits {
        append_target(
            &tr.declaration.name,
            TargetKind::Trait,
            &mut result,
            &mut budget,
        )?;
    }
    inventory(&result, &mut budget)
}

#[derive(Clone, Copy)]
struct Line<'a> {
    text: &'a str,
    start: usize,
}
fn run(bytes: &[u8], start: usize, byte: u8) -> usize {
    bytes[start..].iter().take_while(|&&b| b == byte).count()
}
fn balanced_end(
    lines: &[Line<'_>],
    line: usize,
    start: usize,
    open: u8,
    close: u8,
    budget: &mut Budget,
) -> Result<Option<(usize, usize)>, LinkError> {
    let mut depth = 1usize;
    let mut quote = None;
    let mut angle = false;
    for (index, text) in lines.iter().enumerate().skip(line) {
        let bytes = text.text.as_bytes();
        let mut at = if index == line { start + 1 } else { 0 };
        while at < bytes.len() {
            budget.work(1)?;
            let byte = bytes[at];
            if byte == b'\\' {
                at = (at + 2).min(bytes.len());
                continue;
            }
            if open == b'(' {
                if let Some(delimiter) = quote {
                    if byte == delimiter {
                        quote = None;
                    }
                    at += 1;
                    continue;
                }
                if angle {
                    if byte == b'>' {
                        angle = false;
                    }
                    at += 1;
                    continue;
                }
                if matches!(byte, b'\'' | b'"') {
                    quote = Some(byte);
                    at += 1;
                    continue;
                }
                if byte == b'<' {
                    angle = true;
                    at += 1;
                    continue;
                }
            }
            if byte == open {
                depth = add(depth, 1)?;
            } else if byte == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some((index, at + 1)));
                }
            }
            at += 1;
        }
    }
    Ok(None)
}
fn next_markdown(
    lines: &[Line<'_>],
    line: usize,
    start: usize,
    budget: &mut Budget,
) -> Result<Option<(usize, usize)>, LinkError> {
    for index in line..lines.len().min(line + 2) {
        let bytes = lines[index].text.as_bytes();
        let mut at = if index == line { start } else { 0 };
        while matches!(bytes.get(at), Some(b' ' | b'\t' | b'\r')) {
            budget.work(1)?;
            at += 1;
        }
        if at < bytes.len() {
            return Ok(Some((index, at)));
        }
    }
    Ok(None)
}

fn close_ticks(
    lines: &[Line<'_>],
    line: usize,
    at: usize,
    count: usize,
    budget: &mut Budget,
) -> Result<Option<(usize, usize)>, LinkError> {
    for (index, text) in lines.iter().enumerate().skip(line) {
        let bytes = text.text.as_bytes();
        let mut pos = if index == line { at + count } else { 0 };
        while pos < bytes.len() {
            budget.work(1)?;
            if bytes[pos] == b'`' {
                let n = run(bytes, pos, b'`');
                budget.work(n)?;
                if n == count {
                    return Ok(Some((index, pos + n)));
                }
                pos += n;
            } else {
                pos += 1;
            }
        }
    }
    Ok(None)
}
enum Path<'a> {
    Local(&'a str),
    Unsupported,
    Prose,
}
fn path(display: &str, limits: LinkLimits) -> Result<Path<'_>, LinkError> {
    let p = display
        .strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .unwrap_or(display);
    if local_path(p) {
        check_path(p, limits)?;
        return Ok(if p.split("::").any(|s| s == "Self") {
            Path::Unsupported
        } else {
            Path::Local(p)
        });
    }
    if p.starts_with('@') || p.find('[').is_some_and(|at| local_path(&p[..at])) {
        check_path(p, limits)?;
        return Ok(Path::Unsupported);
    }
    Ok(Path::Prose)
}
fn occurrence(
    display: &str,
    span: Span,
    report: &mut LinkReport,
    budget: &mut Budget,
) -> Result<(), LinkError> {
    let p = path(display, budget.limits)?;
    if matches!(p, Path::Prose) {
        return Ok(());
    }
    if add(report.references.len(), report.diagnostics.len())? >= budget.limits.max_references {
        return Err(resource());
    }
    let mut found = None;
    let mut matches = 0usize;
    if let Path::Local(p) = p {
        for (i, target) in report.targets.iter().enumerate() {
            budget.work(add(add(p.len(), target.path.len())?, 1)?)?;
            if target.path == p {
                found = Some(i);
                matches += 1;
            }
        }
    }
    if matches == 1 {
        let target = copy_target(&report.targets[found.unwrap()], budget)?;
        let anchor = anchor(&target, budget)?;
        let reference = LinkReference {
            span,
            display: budget.string(display)?,
            target,
            anchor,
        };
        budget.push(&mut report.references, reference)?;
    } else {
        let (code, message) = match p {
            Path::Unsupported => ("unsupported", "unsupported local intra-doc reference"),
            _ if matches == 0 => ("unknown", "unknown local intra-doc reference"),
            _ => ("ambiguous", "ambiguous local intra-doc reference"),
        };
        let diagnostic = LinkDiagnostic {
            code: budget.string(code)?,
            span,
            message: budget.string(message)?,
        };
        budget.push(&mut report.diagnostics, diagnostic)?;
    }
    Ok(())
}
fn group(
    lines: &[Line<'_>],
    report: &mut LinkReport,
    budget: &mut Budget,
) -> Result<(), LinkError> {
    let mut line = 0usize;
    let mut at = 0usize;
    let mut fence: Option<(u8, usize)> = None;
    while line < lines.len() {
        let text = lines[line].text;
        let bytes = text.as_bytes();
        if at == 0 {
            let trimmed = text.trim_start_matches([' ', '\t']);
            let indent = text.len() - trimmed.len();
            budget.work(add(indent, 1)?)?;
            let byte = trimmed.as_bytes().first().copied();
            if let Some((delimiter, count)) = fence {
                if byte == Some(delimiter) {
                    let n = run(bytes, indent, delimiter);
                    budget.work(add(n, bytes.len() - indent - n)?)?;
                    if n >= count && text[indent + n..].trim().is_empty() {
                        fence = None;
                    }
                }
                line += 1;
                continue;
            }
            if matches!(byte, Some(b'`' | b'~')) {
                let delimiter = byte.unwrap();
                let n = run(bytes, indent, delimiter);
                budget.work(n)?;
                if n >= 3 {
                    fence = Some((delimiter, n));
                    line += 1;
                    continue;
                }
            }
        }
        if at >= bytes.len() {
            line += 1;
            at = 0;
            continue;
        }
        budget.work(1)?;
        match bytes[at] {
            b'\\' => at = (at + 2).min(bytes.len()),
            b'`' => {
                let n = run(bytes, at, b'`');
                budget.work(n)?;
                if let Some((end_line, end_at)) = close_ticks(lines, line, at, n, budget)? {
                    line = end_line;
                    at = end_at;
                } else {
                    at += n;
                }
            }
            b'[' | b'!' if bytes[at] == b'[' || bytes.get(at + 1) == Some(&b'[') => {
                let image = bytes[at] == b'!';
                let start = at + usize::from(image);
                let Some((end_line, end)) = balanced_end(lines, line, start, b'[', b']', budget)?
                else {
                    line = lines.len();
                    continue;
                };
                let next = next_markdown(lines, end_line, end, budget)?;
                if let Some((next_line, next_at)) =
                    next.filter(|&(i, p)| lines[i].text.as_bytes()[p] == b'(')
                {
                    if let Some((i, p)) =
                        balanced_end(lines, next_line, next_at, b'(', b')', budget)?
                    {
                        line = i;
                        at = p;
                    } else {
                        line = lines.len();
                    }
                } else if let Some((next_line, next_at)) =
                    next.filter(|&(i, p)| lines[i].text.as_bytes()[p] == b'[')
                {
                    if let Some((i, p)) =
                        balanced_end(lines, next_line, next_at, b'[', b']', budget)?
                    {
                        line = i;
                        at = p;
                    } else {
                        line = lines.len();
                    }
                } else if next
                    .is_some_and(|(i, p)| i == end_line && lines[i].text.as_bytes()[p] == b':')
                {
                    line = end_line + 1;
                    at = 0;
                } else {
                    if !image && end_line == line {
                        occurrence(
                            &text[start + 1..end - 1],
                            lines[line].start + start..lines[line].start + end,
                            report,
                            budget,
                        )?;
                    }
                    line = end_line;
                    at = end;
                }
            }
            _ => at += 1,
        }
    }
    Ok(())
}

/// Analyze only real lexer doc-comment tokens, with exact original byte spans.
pub fn analyze(
    source: &str,
    targets: &[LinkTarget],
    limits: LinkLimits,
) -> Result<LinkReport, LinkError> {
    let mut budget = Budget::new(limits)?;
    if source.len() > limits.max_source_bytes {
        return Err(resource());
    }
    budget.work(mul(add(source.len(), 1)?, 8)?)?;
    let targets = inventory(targets, &mut budget)?;
    // Reserve a finite lexer envelope before entering the lexer. Long comments
    // need few tokens; token-dense sources can conservatively exhaust this cap.
    let max_tokens = (limits.max_metadata_bytes / 4 / size_of::<lexer::Token>())
        .min(source.len().saturating_add(1))
        .max(1);
    budget.metadata(add(
        mul(add(max_tokens, 1)?, size_of::<lexer::Token>())?,
        1024,
    )?)?;
    let lexed = lexer::lex_with_limits(
        source,
        lexer::LexLimits {
            max_source_bytes: limits.max_source_bytes,
            max_tokens,
            max_diagnostics: 1,
        },
    )
    .map_err(|_| resource())?;
    if lexed.limit.is_some() {
        return Err(resource());
    }
    if !lexed.diagnostics.is_empty() {
        return Err(error(
            "lexical",
            "source lexing failed before link analysis",
        ));
    }
    let mut report = LinkReport {
        status: LinkStatus::Resolved,
        source_bytes: source.len(),
        targets,
        references: vec![],
        diagnostics: vec![],
    };
    let mut lines = Vec::new();
    for token in &lexed.tokens {
        budget.work(1)?;
        let text = &source[token.span.clone()];
        if token.kind == TokenKind::Comment
            && (text.starts_with("//!") || (text.starts_with("///") && !text.starts_with("////")))
        {
            let content = text[3..].strip_prefix(' ').unwrap_or(&text[3..]);
            let line = Line {
                text: content,
                start: token.span.end - content.len(),
            };
            budget.push(&mut lines, line)?;
        } else if token.kind == TokenKind::Whitespace
            && text.bytes().filter(|&b| b == b'\n').count() <= 1
        {
            budget.work(text.len())?;
        } else {
            group(&lines, &mut report, &mut budget)?;
            lines.clear();
        }
    }
    group(&lines, &mut report, &mut budget)?;
    if !report.diagnostics.is_empty() {
        report.status = LinkStatus::Rejected;
    }
    Ok(report)
}

/// Validate public inspection metadata against the supplied finite declaration
/// inventory. Original lexical association is supplied by the checked facade.
pub fn validate_report(
    report: &LinkReport,
    targets: &[LinkTarget],
    limits: LinkLimits,
) -> Result<(), LinkError> {
    let mut budget = Budget::new(limits)?;
    if report.status == LinkStatus::NotAnalyzed {
        return if report.source_bytes == 0
            && report.targets.is_empty()
            && report.references.is_empty()
            && report.diagnostics.is_empty()
        {
            Ok(())
        } else {
            Err(error("invalid", "nonempty not-analyzed link metadata"))
        };
    }
    if report.source_bytes > limits.max_source_bytes
        || add(report.references.len(), report.diagnostics.len())? > limits.max_references
    {
        return Err(resource());
    }
    let expected = inventory(targets, &mut budget)?;
    let supplied = inventory(&report.targets, &mut budget)?;
    budget.work(mul(
        add(expected.len(), supplied.len())?,
        add(limits.max_path_bytes, 1)?,
    )?)?;
    if supplied != expected || report.targets != supplied {
        return Err(error(
            "invalid",
            "link target inventory does not match document",
        ));
    }
    if (report.status == LinkStatus::Resolved) != report.diagnostics.is_empty() {
        return Err(error("invalid", "link status and diagnostics disagree"));
    }
    let mut end = 0;
    for reference in &report.references {
        if reference.span.start < end
            || reference.span.start >= reference.span.end
            || reference.span.end > report.source_bytes
            || reference.span.end - reference.span.start != add(reference.display.len(), 2)?
        {
            return Err(error("invalid", "invalid link occurrence span"));
        }
        end = reference.span.end;
        budget.work(add(reference.display.len(), 1)?)?;
        let Path::Local(path) = path(&reference.display, limits)? else {
            return Err(error("invalid", "unsupported displayed link path"));
        };
        let mut matches = 0;
        let mut exact = false;
        for target in &expected {
            budget.work(add(add(path.len(), target.path.len())?, 1)?)?;
            if target.path == path {
                matches += 1;
                exact |= target == &reference.target;
            }
        }
        if matches != 1 || !exact || reference.anchor != anchor(&reference.target, &mut budget)? {
            return Err(error(
                "invalid",
                "invalid or ambiguous reference target/anchor",
            ));
        }
    }
    let mut end = 0;
    for diagnostic in &report.diagnostics {
        budget.work(add(diagnostic.message.len(), diagnostic.code.len())?)?;
        if diagnostic.span.start < end
            || diagnostic.span.start >= diagnostic.span.end
            || diagnostic.span.end > report.source_bytes
            || diagnostic.message.len() > limits.max_path_bytes + 128
            || !matches!(
                diagnostic.code.as_str(),
                "unknown" | "ambiguous" | "unsupported"
            )
        {
            return Err(error("invalid", "invalid link diagnostic"));
        }
        end = diagnostic.span.end;
    }
    Ok(())
}

// A separate finite rendering stage prepays new temporary anchor storage and
// every typed-section scan/copy before the renderer allocates those anchors.
// Serialized/output buffers retain the existing independent output byte cap.
pub(crate) fn render_preflight(
    document: &Document,
    targets: &[LinkTarget],
    limits: LinkLimits,
) -> Result<(), LinkError> {
    let mut budget = Budget::new(limits)?;
    if document.doc_comments.len() > 4096 {
        return Err(resource());
    }
    for target in targets {
        target_valid(target, limits)?;
        budget.metadata(mul(add(32, mul(target.path.len(), 2)?)?, 4)?)?;
        budget.work(mul(add(target.path.len(), 1)?, 16)?)?;
    }
    let mut bytes = 0usize;
    for comment in &document.doc_comments {
        bytes = add(bytes, comment.text.len())?;
        if bytes > limits.max_source_bytes {
            return Err(resource());
        }
        budget.work(mul(add(comment.text.len(), 1)?, 16)?)?;
    }
    for reference in &document.intra_doc_links.references {
        budget.work(mul(
            add(add(reference.display.len(), reference.anchor.len())?, 1)?,
            16,
        )?)?;
    }
    Ok(())
}
