//! Bounded projection of an actual formatter arena. This is document-algebra
//! evidence, not checked source or a proof of the Rust renderer's refinement.
use crate::{Doc, DocId, Document, Error};
use std::{fmt, mem::size_of};

#[derive(Clone, Copy, Debug)]
pub struct ProofLimits {
    pub max_nodes: usize,
    pub max_text_bytes: usize,
    pub max_depth: usize,
    pub max_metadata_bytes: usize,
    pub max_lean_bytes: usize,
    pub max_work: usize,
}
impl Default for ProofLimits {
    fn default() -> Self {
        Self {
            max_nodes: 4096,
            max_text_bytes: 128 * 1024,
            max_depth: 64,
            max_metadata_bytes: 1024 * 1024,
            max_lean_bytes: 1024 * 1024,
            max_work: 64 * 1024 * 1024,
        }
    }
}
impl ProofLimits {
    pub const HARD: Self = Self {
        max_nodes: 16384,
        max_text_bytes: 256 * 1024,
        max_depth: 128,
        max_metadata_bytes: 4 * 1024 * 1024,
        max_lean_bytes: 2 * 1024 * 1024,
        max_work: 256 * 1024 * 1024,
    };
    pub fn validate(self) -> Result<(), ProofError> {
        let hard = Self::HARD;
        for (value, cap) in [
            (self.max_nodes, hard.max_nodes),
            (self.max_text_bytes, hard.max_text_bytes),
            (self.max_depth, hard.max_depth),
            (self.max_metadata_bytes, hard.max_metadata_bytes),
            (self.max_lean_bytes, hard.max_lean_bytes),
            (self.max_work, hard.max_work),
        ] {
            if value == 0 || value > cap {
                return Err(ProofError::InvalidLimits);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProofError {
    InvalidLimits,
    Resource(&'static str),
    InvalidDocument,
    Allocation,
    Formatter(Error),
    Conservation,
    LeanSyntax(String),
}
impl fmt::Display for ProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("invalid formatter proof limits"),
            Self::Resource(resource) => write!(f, "formatter proof {resource} limit exceeded"),
            Self::InvalidDocument => f.write_str("invalid formatter document reference"),
            Self::Allocation => f.write_str("formatter proof allocation failed"),
            Self::Formatter(error) => write!(f, "formatter unavailable: {error}"),
            Self::Conservation => f.write_str("formatter proof conservation check failed"),
            Self::LeanSyntax(error) => write!(f, "formatter proof Lean syntax: {error}"),
        }
    }
}
impl std::error::Error for ProofError {}
impl From<Error> for ProofError {
    fn from(error: Error) -> Self {
        Self::Formatter(error)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ModelDoc {
    Nil,
    Text(String),
    Comment(String),
    HardLine,
    SoftLineOrSpace,
    SoftLineOrEmpty,
    Nest(usize, Box<ModelDoc>),
    Concat(Vec<ModelDoc>),
    Group(Box<ModelDoc>),
}

/// An owned projection from a validated current-owner formatter document.
/// Fields cannot be supplied by a caller or deserialized from cached claims.
///
/// ```compile_fail
/// use vesper_fmt::proof_model::DocumentModel;
/// let forged = DocumentModel {};
/// ```
#[derive(Debug)]
pub struct DocumentModel {
    doc: ModelDoc,
    node_count: usize,
    text_bytes: usize,
    depth: usize,
    comments: Vec<String>,
}
impl DocumentModel {
    pub fn node_count(&self) -> usize {
        self.node_count
    }
    pub fn text_bytes(&self) -> usize {
        self.text_bytes
    }
    pub fn depth(&self) -> usize {
        self.depth
    }
    pub fn comments(&self) -> &[String] {
        &self.comments
    }
    pub(crate) fn doc(&self) -> &ModelDoc {
        &self.doc
    }
}

fn add(a: usize, b: usize) -> Result<usize, ProofError> {
    a.checked_add(b).ok_or(ProofError::Resource("arithmetic"))
}
fn mul(a: usize, b: usize) -> Result<usize, ProofError> {
    a.checked_mul(b).ok_or(ProofError::Resource("arithmetic"))
}
struct Budget {
    remaining: usize,
    metadata: usize,
    limits: ProofLimits,
}
impl Budget {
    fn work(&mut self, units: usize) -> Result<(), ProofError> {
        self.remaining = self
            .remaining
            .checked_sub(units)
            .ok_or(ProofError::Resource("work"))?;
        Ok(())
    }
    fn metadata(&mut self, bytes: usize) -> Result<(), ProofError> {
        self.metadata = add(self.metadata, bytes)?;
        if self.metadata > self.limits.max_metadata_bytes {
            return Err(ProofError::Resource("metadata"));
        }
        self.work(bytes)
    }
}
struct Frame {
    id: DocId,
    depth: usize,
    entered: bool,
    next_child: usize,
}
impl Frame {
    fn new(id: DocId, depth: usize) -> Self {
        Self {
            id,
            depth,
            entered: false,
            next_child: 0,
        }
    }
}
#[derive(Default)]
struct Stats {
    nodes: usize,
    text_bytes: usize,
    depth: usize,
    comments: usize,
    build_work: usize,
}
fn preflight(
    document: &Document<'_>,
    root: DocId,
    budget: &mut Budget,
) -> Result<Stats, ProofError> {
    document.proof_node(root)?;
    budget.metadata(size_of::<DocumentModel>())?;
    budget.metadata(mul(budget.limits.max_depth, size_of::<Frame>())?)?;
    let mut stack = Vec::new();
    stack
        .try_reserve_exact(budget.limits.max_depth)
        .map_err(|_| ProofError::Allocation)?;
    stack.push(Frame::new(root, 1));
    let mut stats = Stats::default();
    while let Some(frame) = stack.last_mut() {
        budget.work(1)?;
        let node = document.proof_node(frame.id)?;
        if !frame.entered {
            stats.nodes = add(stats.nodes, 1)?;
            if stats.nodes > budget.limits.max_nodes {
                return Err(ProofError::Resource("nodes"));
            }
            stats.depth = stats.depth.max(frame.depth);
            if stats.depth > budget.limits.max_depth {
                return Err(ProofError::Resource("depth"));
            }
            budget.metadata(size_of::<ModelDoc>())?;
            stats.build_work = add(stats.build_work, add(size_of::<ModelDoc>(), 4)?)?;
            if let Doc::Text(text) | Doc::Comment(text) = node {
                stats.text_bytes = add(stats.text_bytes, text.len())?;
                if stats.text_bytes > budget.limits.max_text_bytes {
                    return Err(ProofError::Resource("text"));
                }
                budget.metadata(text.len())?;
                stats.build_work = add(stats.build_work, text.len())?;
                if matches!(node, Doc::Comment(_)) {
                    stats.comments = add(stats.comments, 1)?;
                    budget.metadata(add(size_of::<String>(), text.len())?)?;
                    stats.build_work =
                        add(stats.build_work, add(size_of::<String>(), text.len())?)?;
                }
            }
            frame.entered = true;
        }
        let child = match node {
            Doc::Nest(_, child) | Doc::Group(child) if frame.next_child == 0 => Some(*child),
            Doc::Concat(children) => children.get(frame.next_child).copied(),
            _ => None,
        };
        if let Some(child) = child {
            frame.next_child = add(frame.next_child, 1)?;
            let depth = add(frame.depth, 1)?;
            if depth > budget.limits.max_depth {
                return Err(ProofError::Resource("depth"));
            }
            stack.push(Frame::new(child, depth));
        } else {
            stack.pop();
        }
    }
    // All payload/tree/retained-comment construction work is reserved before
    // the first payload clone. The scan stack was paid before its allocation.
    budget.work(stats.build_work)?;
    Ok(stats)
}
fn copy(text: &str) -> Result<String, ProofError> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|_| ProofError::Allocation)?;
    owned.push_str(text);
    Ok(owned)
}
fn build(
    document: &Document<'_>,
    id: DocId,
    comments: &mut Vec<String>,
) -> Result<ModelDoc, ProofError> {
    Ok(match document.proof_node(id)? {
        Doc::Nil => ModelDoc::Nil,
        Doc::Text(text) => ModelDoc::Text(copy(text)?),
        Doc::Comment(text) => {
            comments.push(copy(text)?);
            ModelDoc::Comment(copy(text)?)
        }
        Doc::HardLine => ModelDoc::HardLine,
        Doc::SoftLineOrSpace => ModelDoc::SoftLineOrSpace,
        Doc::SoftLineOrEmpty => ModelDoc::SoftLineOrEmpty,
        Doc::Nest(indent, inner) => {
            ModelDoc::Nest(*indent, Box::new(build(document, *inner, comments)?))
        }
        Doc::Group(inner) => ModelDoc::Group(Box::new(build(document, *inner, comments)?)),
        Doc::Concat(children) => {
            let mut projected = Vec::new();
            projected
                .try_reserve_exact(children.len())
                .map_err(|_| ProofError::Allocation)?;
            for child in children {
                projected.push(build(document, *child, comments)?);
            }
            ModelDoc::Concat(projected)
        }
    })
}
pub(crate) fn project(
    document: &Document<'_>,
    root: DocId,
    limits: ProofLimits,
) -> Result<DocumentModel, ProofError> {
    limits.validate()?;
    let mut budget = Budget {
        remaining: limits.max_work,
        metadata: 0,
        limits,
    };
    let stats = preflight(document, root, &mut budget)?;
    let mut comments = Vec::new();
    comments
        .try_reserve_exact(stats.comments)
        .map_err(|_| ProofError::Allocation)?;
    let doc = build(document, root, &mut comments)?;
    Ok(DocumentModel {
        doc,
        node_count: stats.nodes,
        text_bytes: stats.text_bytes,
        depth: stats.depth,
        comments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;

    #[test]
    fn all_actual_arena_variants_and_wrapper_payloads_are_preserved() {
        let mut document = Document::new(Limits::default()).unwrap();
        let nil = document.push(Doc::Nil).unwrap();
        let text = document.push(Doc::Text("x")).unwrap();
        let comment = document.push(Doc::Comment("/* 🦀 */")).unwrap();
        let hard = document.push(Doc::HardLine).unwrap();
        let space = document.push(Doc::SoftLineOrSpace).unwrap();
        let empty = document.push(Doc::SoftLineOrEmpty).unwrap();
        let nested = document.push(Doc::Nest(7, comment)).unwrap();
        let grouped = document.push(Doc::Group(nested)).unwrap();
        let root = document
            .push(Doc::Concat(vec![nil, text, grouped, hard, space, empty]))
            .unwrap();
        let projected = document.proof_model(root, ProofLimits::default()).unwrap();
        assert_eq!(
            projected.doc(),
            &ModelDoc::Concat(vec![
                ModelDoc::Nil,
                ModelDoc::Text("x".into()),
                ModelDoc::Group(Box::new(ModelDoc::Nest(
                    7,
                    Box::new(ModelDoc::Comment("/* 🦀 */".into()))
                ))),
                ModelDoc::HardLine,
                ModelDoc::SoftLineOrSpace,
                ModelDoc::SoftLineOrEmpty,
            ])
        );
    }
}
