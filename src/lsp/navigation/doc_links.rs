//! Source-only documentation definitions. No Document, layout, graph or proof
//! artifact is constructed; the parent supplies its real current lexer tokens.
use super::{Error, Limits};
use crate::{
    ast::{Function, Item, Span},
    check::World,
    lexer::{Token, TokenKind as K},
    tooling::frontend::CheckedSource,
};
use std::mem::size_of;
use vesper_doc::links::{self, LinkLimits, LinkTarget, TargetKind};

const MAX_METADATA: usize = 1024 * 1024;
const MAX_WORK: usize = 16 * 1024 * 1024;

fn resource() -> Error {
    Error::Resource("doc navigation inventory work/metadata")
}
fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or_else(resource)
}
fn mul(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_mul(b).ok_or_else(resource)
}
struct Budget {
    work: usize,
    metadata: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            work: MAX_WORK,
            metadata: MAX_METADATA,
        }
    }
    fn work(&mut self, n: usize) -> Result<(), Error> {
        self.work = self.work.checked_sub(n).ok_or_else(resource)?;
        Ok(())
    }
    fn metadata(&mut self, n: usize) -> Result<(), Error> {
        self.metadata = self.metadata.checked_sub(n).ok_or_else(resource)?;
        self.work(n)
    }
    fn lookup(&mut self, entries: usize, key_bytes: usize) -> Result<(), Error> {
        // A full-map comparison envelope also bounds its logarithmic lookup.
        self.work(mul(add(entries, 1)?, add(key_bytes, 1)?)?)
    }
    fn reserve<T>(&mut self, values: &mut Vec<T>, limit: usize) -> Result<(), Error> {
        if values.len() >= limit {
            return Err(resource());
        }
        if values.len() == values.capacity() {
            let next = mul(values.capacity().max(2), 2)?.min(limit);
            self.metadata(mul(next, size_of::<T>())?)?;
            self.work(mul(values.len(), size_of::<T>())?)?;
            values
                .try_reserve_exact(next - values.len())
                .map_err(|_| resource())?;
        }
        Ok(())
    }
}

struct Inventory<'a> {
    source: &'a str,
    tokens: &'a [Token],
    names: Vec<LinkTarget>,
    spans: Vec<Span>,
    limits: LinkLimits,
    budget: Budget,
}
impl Inventory<'_> {
    fn path(&mut self, owner: Option<&str>, name: &str) -> Result<String, Error> {
        self.budget.work(add(name.len(), 1)?)?;
        let bytes = if let Some(owner) = owner {
            self.budget.work(add(owner.len(), 1)?)?;
            add(add(owner.len(), 2)?, name.len())?
        } else {
            name.len()
        };
        if bytes > self.limits.max_path_bytes {
            return Err(resource());
        }
        self.budget.metadata(bytes)?;
        let mut result = String::new();
        result.try_reserve_exact(bytes).map_err(|_| resource())?;
        if let Some(owner) = owner {
            result.push_str(owner);
            result.push_str("::");
        }
        result.push_str(name);
        Ok(result)
    }
    fn selection(&mut self, declaration: &Span, name: &str) -> Result<Span, Error> {
        if self.source.get(declaration.clone()).is_none() {
            return Err(Error::InvalidSpan);
        }
        self.budget.work(mul(self.tokens.len(), 2)?)?;
        let begin = self
            .tokens
            .partition_point(|t| t.span.start < declaration.start);
        let end = self
            .tokens
            .partition_point(|t| t.span.end <= declaration.end);
        for token in &self.tokens[begin..end] {
            self.budget.work(1)?;
            if matches!(token.kind, K::Lower | K::Upper | K::SelfKw) {
                self.budget.work(add(token.span.len(), name.len())?)?;
                if self.source.get(token.span.clone()) == Some(name) {
                    return Ok(token.span.clone());
                }
            }
        }
        Err(Error::InvalidSpan)
    }
    fn push(
        &mut self,
        path: String,
        kind: TargetKind,
        declaration: &Span,
        leaf: &str,
    ) -> Result<(), Error> {
        let selection = self.selection(declaration, leaf)?;
        self.budget
            .reserve(&mut self.names, self.limits.max_targets)?;
        self.budget
            .reserve(&mut self.spans, self.limits.max_targets)?;
        self.names.push(LinkTarget { path, kind });
        self.spans.push(selection);
        Ok(())
    }
    fn function(
        &mut self,
        world: &World,
        source: &Function,
        owner: Option<&str>,
    ) -> Result<(), Error> {
        let path = self.path(owner, &source.name)?;
        self.budget.lookup(world.functions.len(), path.len())?;
        let actual = world.functions.get(&path).ok_or(Error::InvalidSpan)?;
        if actual.span != source.span {
            return Err(Error::InvalidSpan);
        }
        self.budget
            .lookup(world.elaboration.generated.len(), path.len())?;
        if world.elaboration.generated.contains_key(&path) {
            return Ok(());
        }
        self.push(
            path,
            if owner.is_some() {
                TargetKind::InherentMethod
            } else {
                TargetKind::Function
            },
            &source.span,
            &source.name,
        )
    }
    fn collect(&mut self, snapshot: &CheckedSource) -> Result<(), Error> {
        let world = snapshot.world();
        for item in &snapshot.parsed().module.items {
            self.budget.work(1)?;
            match item {
                Item::Function(f) => self.function(world, f, None)?,
                Item::Adt(a) => {
                    self.budget.lookup(world.adts.len(), a.name.len())?;
                    if world.adts.get(&a.name).map(|d| &d.span) != Some(&a.span) {
                        return Err(Error::InvalidSpan);
                    }
                    let path = self.path(None, &a.name)?;
                    self.push(path, TargetKind::Type, &a.span, &a.name)?;
                }
                Item::Effect(e) => {
                    self.budget.lookup(world.effects.len(), e.name.len())?;
                    let actual = world.effects.get(&e.name).ok_or(Error::InvalidSpan)?;
                    if actual.span != e.span {
                        return Err(Error::InvalidSpan);
                    }
                    let path = self.path(None, &e.name)?;
                    self.push(path, TargetKind::Effect, &e.span, &e.name)?;
                    for operation in &e.ops {
                        self.budget.work(1)?;
                        for candidate in &actual.ops {
                            self.budget
                                .work(add(add(candidate.name.len(), operation.name.len())?, 1)?)?;
                        }
                        if !actual
                            .ops
                            .iter()
                            .any(|f| f.name == operation.name && f.span == operation.span)
                        {
                            return Err(Error::InvalidSpan);
                        }
                        let path = self.path(Some(&e.name), &operation.name)?;
                        self.push(
                            path,
                            TargetKind::EffectOperation,
                            &operation.span,
                            &operation.name,
                        )?;
                    }
                }
                Item::Impl { ty, methods, .. } => {
                    // The local path grammar has no tuple/function-type owner.
                    if !ty.name().is_empty() {
                        for method in methods {
                            self.budget.work(1)?;
                            self.function(world, method, Some(ty.name()))?;
                        }
                    }
                }
                Item::Trait(t) => {
                    for name in &world.elaboration.traits {
                        self.budget.work(add(add(name.len(), t.name.len())?, 1)?)?;
                    }
                    if !world.elaboration.traits.contains(&t.name) {
                        return Err(Error::InvalidSpan);
                    }
                    let path = self.path(None, &t.name)?;
                    self.push(path, TargetKind::Trait, &t.span, &t.name)?;
                }
                // Conformance methods elaborate into private helpers. Model
                // names and trait method signatures are not this link profile.
                Item::Conformance(_) => (),
            }
        }
        Ok(())
    }
}

pub(super) fn target(
    snapshot: &CheckedSource,
    tokens: &[Token],
    offset: usize,
    limits: Limits,
) -> Result<Option<Span>, Error> {
    let mut budget = Budget::new();
    budget.work(tokens.len())?;
    let at = tokens.partition_point(|t| t.span.end <= offset);
    let Some(token) = tokens.get(at) else {
        return Ok(None);
    };
    if token.kind != K::Comment || !super::contains(&token.span, offset) {
        return Ok(None);
    }
    let source = snapshot.source();
    let comment = source.get(token.span.clone()).ok_or(Error::InvalidSpan)?;
    if !(comment.starts_with("//!") || comment.starts_with("///") && !comment.starts_with("////")) {
        return Ok(None);
    }
    let link_limits = LinkLimits {
        max_source_bytes: limits
            .max_source_bytes
            .min(LinkLimits::HARD.max_source_bytes),
        max_targets: limits.max_symbols,
        ..Default::default()
    };
    let mut inventory = Inventory {
        source,
        tokens,
        names: vec![],
        spans: vec![],
        limits: link_limits,
        budget,
    };
    inventory.collect(snapshot)?;
    let report = links::analyze(source, &inventory.names, link_limits).map_err(|e| {
        match e.code.as_str() {
            "resource" => Error::Resource("doc link parser budget"),
            "configuration" => Error::InvalidLimits,
            _ => Error::InvalidSpan,
        }
    })?;
    inventory.budget.work(report.references.len())?;
    let Some(reference) = report
        .references
        .iter()
        .find(|r| super::contains(&r.span, offset))
    else {
        return Ok(None);
    };
    let mut selected = None;
    for (name, span) in inventory.names.iter().zip(&inventory.spans) {
        inventory
            .budget
            .work(add(add(name.path.len(), reference.target.path.len())?, 1)?)?;
        if name == &reference.target {
            if selected.is_some() {
                return Err(Error::InvalidSpan);
            }
            selected = Some(span.clone());
        }
    }
    selected.map(Some).ok_or(Error::InvalidSpan)
}
