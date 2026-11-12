//! AST-free, transactional recovery CST updates. Body reuse is deliberately
//! narrow; uncertainty invokes the real full recovery parser, never an old AST.
use crate::{
    ast::{Diagnostic, Span},
    parser,
    recovery::{
        self, Declaration, DeclarationKind, DeclarationStatus, Error, Limits, RecoverySite,
        RecoverySyntax, Resource,
    },
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateKind {
    Unchanged,
    BodyReparse,
    FullReparse,
}
impl UpdateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::BodyReparse => "body_reparse",
            Self::FullReparse => "full_reparse",
        }
    }
}
/// Owns one actual current syntax snapshot. An update failure leaves this value
/// unchanged; callers must not publish it as analysis of their newer text.
#[derive(Debug)]
pub struct SyntaxDocument {
    snapshot: Arc<RecoverySyntax>,
    limits: Limits,
    last: UpdateKind,
}
impl SyntaxDocument {
    pub fn open(source: &str, limits: Limits) -> Result<Self, Error> {
        limits.preflight(source)?;
        let snapshot = Arc::new(parser::parse_recovery(source, limits)?);
        Ok(Self {
            snapshot,
            limits,
            last: UpdateKind::FullReparse,
        })
    }
    pub fn source(&self) -> &str {
        self.snapshot.source()
    }
    pub fn snapshot(&self) -> &Arc<RecoverySyntax> {
        &self.snapshot
    }
    pub fn last_update_kind(&self) -> UpdateKind {
        self.last
    }
    pub fn update(&mut self, source: &str) -> Result<UpdateKind, Error> {
        self.limits.preflight(source)?;
        let mut work = Work {
            remaining: self.limits.max_work,
        };
        let changed = difference(self.source(), source, &mut work)?;
        let Some(changed) = changed else {
            self.last = UpdateKind::Unchanged;
            return Ok(self.last);
        };
        let reused = self.try_body(source, &changed, &mut work)?;
        let (snapshot, kind) = if let Some(snapshot) = reused {
            (snapshot, UpdateKind::BodyReparse)
        } else {
            let limits = work.limits(self.limits)?;
            (
                parser::parse_recovery(source, limits)?,
                UpdateKind::FullReparse,
            )
        };
        self.snapshot = Arc::new(snapshot);
        self.last = kind;
        Ok(kind)
    }
    fn try_body(
        &self,
        source: &str,
        changed: &Change,
        work: &mut Work,
    ) -> Result<Option<RecoverySyntax>, Error> {
        let mut eligible = None;
        for (id, d) in self.snapshot.declarations().iter().enumerate() {
            work.charge(1)?;
            if d.kind != DeclarationKind::Function
                || d.status != DeclarationStatus::Complete
                || d.body_context.is_none()
            {
                continue;
            }
            let (Some(body), Some(raw)) = (&d.body_span, &d.body_green_span) else {
                continue;
            };
            if body != raw
                || body.len() < 2
                || changed.old.start <= body.start
                || changed.old.end >= body.end
            {
                continue;
            }
            if eligible.is_some() {
                return Ok(None);
            }
            eligible = Some((id, body.clone()));
        }
        let Some((id, body)) = eligible else {
            return Ok(None);
        };
        // Delimiter edits can change the intended production boundary. This
        // profile intentionally reparses them fully, even when balanced.
        for byte in self.source()[changed.old.clone()]
            .bytes()
            .chain(source[changed.new.clone()].bytes())
        {
            work.charge(1)?;
            if matches!(byte, b'{' | b'}' | b'(' | b')' | b'[' | b']') {
                return Ok(None);
            }
        }
        let new_body = body.start..changed.shift(body.end)?;
        if new_body.len() < 2
            || source.as_bytes().get(new_body.start) != Some(&b'{')
            || source.as_bytes().get(new_body.end - 1) != Some(&b'}')
        {
            return Ok(None);
        }
        // Reserve bounded metadata scanning/cloning before either an early
        // uncertainty fallback or the eventual owned inventory clone.
        charge_inventory(&self.snapshot, work)?;
        if self
            .snapshot
            .diagnostics()
            .iter()
            .any(|d| diagnostic_touches(d, &body))
            || self.snapshot.sites().iter().any(|s| site_touches(s, &body))
        {
            return Ok(None);
        }
        // Helpers enforce their own whole-stage caps. Debit each entire stage
        // BEFORE invoking it, so a failed attempt cannot reset fallback work.
        let capture = work.reserve_fraction(self.limits, 4)?;
        let context = match parser::capture_body_context(&self.snapshot, id, capture) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        let parse = work.reserve_fraction(self.limits, 3)?;
        let replacement =
            match parser::parse_body_fragment(source, new_body.clone(), &context, parse) {
                Ok(g) => g,
                Err(_) => return Ok(None),
            };
        let root = self.snapshot.syntax();
        let mut block = None;
        for node in root.descendants() {
            work.charge(1)?;
            if node.kind() == parser::BLOCK
                && usize::from(node.text_range().start()) == body.start
                && usize::from(node.text_range().end()) == body.end
            {
                if block.is_some() {
                    return Ok(None);
                }
                block = Some(node);
            }
        }
        let Some(block) = block else {
            return Ok(None);
        };
        let mut declarations = self.snapshot.declarations().to_vec();
        let mut diagnostics = self.snapshot.diagnostics().to_vec();
        let mut sites = self.snapshot.sites().to_vec();
        for (index, d) in declarations.iter_mut().enumerate() {
            if index == id {
                let Ok(span) = changed.enclosing(&d.span, &body) else {
                    return Ok(None);
                };
                d.span = span;
                d.body_span = Some(new_body.clone());
                d.body_green_span = Some(new_body.clone());
            } else if shift_declaration(d, changed, &body).is_err() {
                return Ok(None);
            }
        }
        for diagnostic in &mut diagnostics {
            if shift_diagnostic(diagnostic, changed, &body).is_err() {
                return Ok(None);
            }
        }
        for site in &mut sites {
            if shift_site(site, changed, &body).is_err() {
                return Ok(None);
            }
        }
        // Rowan rebuilds only ancestors, cloning each ancestor's child handles.
        // Account for that actual bounded staging before allocating the new path.
        for ancestor in block.ancestors() {
            for _ in ancestor.children_with_tokens() {
                work.charge(1)?;
            }
        }
        let green = block.replace_with(replacement);
        work.charge(source.len())?; // bound the new owned source allocation
        let limits = work.reserve_fraction(self.limits, 2)?;
        match recovery::assemble(
            Arc::from(source),
            green,
            diagnostics,
            declarations,
            sites,
            limits,
        ) {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(_) => Ok(None),
        }
    }
}
struct Work {
    remaining: usize,
}
impl Work {
    fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(n)
            .ok_or(Error::Budget(Resource::Work))?;
        Ok(())
    }
    fn limits(&self, mut limits: Limits) -> Result<Limits, Error> {
        if self.remaining == 0 {
            return Err(Error::Budget(Resource::Work));
        }
        limits.max_work = self.remaining;
        Ok(limits)
    }
    fn reserve_fraction(
        &mut self,
        mut limits: Limits,
        denominator: usize,
    ) -> Result<Limits, Error> {
        let amount = self.remaining / denominator;
        if amount == 0 {
            return Err(Error::Budget(Resource::Work));
        }
        self.charge(amount)?;
        limits.max_work = amount;
        Ok(limits)
    }
}
struct Change {
    old: Span,
    new: Span,
}
impl Change {
    fn shift(&self, offset: usize) -> Result<usize, Error> {
        self.new
            .end
            .checked_add(offset.checked_sub(self.old.end).ok_or(Error::InvalidSpan)?)
            .ok_or(Error::InvalidSpan)
    }
    fn outside(&self, span: &Span, body: &Span) -> Result<Span, Error> {
        if span.end <= body.start {
            Ok(span.clone())
        } else if span.start >= body.end {
            Ok(self.shift(span.start)?..self.shift(span.end)?)
        } else {
            Err(Error::InvalidInventory)
        }
    }
    fn enclosing(&self, span: &Span, body: &Span) -> Result<Span, Error> {
        if span.start <= body.start && span.end >= body.end {
            Ok(span.start..self.shift(span.end)?)
        } else {
            Err(Error::InvalidInventory)
        }
    }
}
fn crlf_interior(source: &str, at: usize) -> bool {
    at > 0
        && source.as_bytes().get(at - 1) == Some(&b'\r')
        && source.as_bytes().get(at) == Some(&b'\n')
}
fn difference(old: &str, new: &str, work: &mut Work) -> Result<Option<Change>, Error> {
    let mut start = 0;
    while start < old.len().min(new.len()) {
        work.charge(1)?;
        if old.as_bytes()[start] != new.as_bytes()[start] {
            break;
        }
        start += 1;
    }
    if start == old.len() && start == new.len() {
        return Ok(None);
    }
    while !old.is_char_boundary(start)
        || !new.is_char_boundary(start)
        || crlf_interior(old, start)
        || crlf_interior(new, start)
    {
        start -= 1;
        work.charge(1)?;
    }
    let (mut a, mut b) = (old.len(), new.len());
    while a > start && b > start {
        work.charge(1)?;
        if old.as_bytes()[a - 1] != new.as_bytes()[b - 1] {
            break;
        }
        a -= 1;
        b -= 1;
    }
    while !old.is_char_boundary(a)
        || !new.is_char_boundary(b)
        || crlf_interior(old, a)
        || crlf_interior(new, b)
    {
        a += 1;
        b += 1;
        work.charge(1)?;
    }
    Ok(Some(Change {
        old: start..a,
        new: start..b,
    }))
}
fn touches(span: &Span, body: &Span) -> bool {
    span.start < body.end && span.end > body.start
        || span.is_empty() && body.start < span.start && span.start < body.end
}
fn diagnostic_touches(d: &Diagnostic, body: &Span) -> bool {
    touches(&d.span, body) || d.related.iter().any(|l| touches(&l.span, body))
}
fn site_touches(s: &RecoverySite, body: &Span) -> bool {
    touches(&s.span, body)
        || s.receiver.as_ref().is_some_and(|r| {
            touches(&r.root, body) || r.projections.iter().any(|p| touches(p, body))
        })
}
fn shift_declaration(d: &mut Declaration, c: &Change, body: &Span) -> Result<(), Error> {
    d.span = c.outside(&d.span, body)?;
    d.header_span = c.outside(&d.header_span, body)?;
    if let Some(span) = &mut d.name_span {
        *span = c.outside(span, body)?;
    }
    if let Some(span) = &mut d.body_span {
        *span = c.outside(span, body)?;
    }
    if let Some(span) = &mut d.body_green_span {
        *span = c.outside(span, body)?;
    }
    Ok(())
}
fn shift_diagnostic(d: &mut Diagnostic, c: &Change, body: &Span) -> Result<(), Error> {
    d.span = c.outside(&d.span, body)?;
    for label in &mut d.related {
        label.span = c.outside(&label.span, body)?;
    }
    Ok(())
}
fn shift_site(s: &mut RecoverySite, c: &Change, body: &Span) -> Result<(), Error> {
    s.span = c.outside(&s.span, body)?;
    if let Some(r) = &mut s.receiver {
        r.root = c.outside(&r.root, body)?;
        for p in &mut r.projections {
            *p = c.outside(p, body)?;
        }
    }
    Ok(())
}
fn charge_inventory(snapshot: &RecoverySyntax, work: &mut Work) -> Result<(), Error> {
    // Existing sealed metadata is bounded, but charge every value before cloning.
    for d in snapshot.declarations() {
        work.charge(std::mem::size_of::<Declaration>())?;
        if let Some(c) = &d.body_context {
            for name in c.row_names.iter().chain(&c.bound_values) {
                work.charge(
                    name.len()
                        .checked_add(std::mem::size_of::<String>())
                        .ok_or(Error::Budget(Resource::Work))?,
                )?;
            }
        }
    }
    for d in snapshot.diagnostics() {
        work.charge(std::mem::size_of::<Diagnostic>())?;
        work.charge(d.code.len())?;
        work.charge(d.message.len())?;
        if let Some(h) = &d.help {
            work.charge(h.len())?;
        }
        for l in &d.related {
            work.charge(std::mem::size_of_val(l))?;
            work.charge(l.message.len())?;
        }
    }
    for s in snapshot.sites() {
        work.charge(std::mem::size_of::<RecoverySite>())?;
        if let Some(r) = &s.receiver {
            work.charge(std::mem::size_of_val(r))?;
            work.charge(
                r.projections
                    .len()
                    .checked_mul(std::mem::size_of::<Span>())
                    .ok_or(Error::Budget(Resource::Work))?,
            )?;
        }
    }
    Ok(())
}
