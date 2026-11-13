use crate::{Error, Limits, Work};

pub const DEFAULT_LINE_WIDTH: usize = 100;
/// IDs can refer only to previously constructed nodes in their Document.
/// IDs from another document are rejected (including numerically equal indices).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocId {
    index: usize,
    owner: u64,
}
#[derive(Debug)]
pub enum Doc<'a> {
    Nil,
    Text(&'a str),
    Comment(&'a str),
    HardLine,
    SoftLineOrSpace,
    SoftLineOrEmpty,
    Nest(usize, DocId),
    Concat(Vec<DocId>),
    Group(DocId),
}
struct Node<'a> {
    doc: Doc<'a>,
    width: Option<usize>,
    depth: usize,
}
pub struct Document<'a> {
    nodes: Vec<Node<'a>>,
    owner: u64,
    limits: Limits,
    work: Work,
}
impl<'a> Document<'a> {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let mut current = NEXT.load(Ordering::Relaxed);
        let owner = loop {
            let next = current.checked_add(1).ok_or(Error::Overflow)?;
            match NEXT.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break current,
                Err(observed) => current = observed,
            }
        };
        Ok(Self {
            nodes: Vec::new(),
            owner,
            limits,
            work: Work::new(limits.max_work),
        })
    }
    fn node(&self, id: DocId) -> Result<&Node<'a>, Error> {
        if id.owner != self.owner {
            return Err(Error::InvalidDoc);
        }
        self.nodes.get(id.index).ok_or(Error::InvalidDoc)
    }
    /// Project this exact arena, preserving expanded shared-node occurrences.
    /// This proves a document model only; it does not establish source checking.
    pub fn proof_model(
        &self,
        root: DocId,
        limits: crate::proof_model::ProofLimits,
    ) -> Result<crate::proof_model::DocumentModel, crate::proof_model::ProofError> {
        crate::proof_model::project(self, root, limits)
    }
    pub(crate) fn proof_node(&self, id: DocId) -> Result<&Doc<'a>, crate::proof_model::ProofError> {
        self.node(id)
            .map(|node| &node.doc)
            .map_err(|_| crate::proof_model::ProofError::InvalidDocument)
    }
    pub fn push(&mut self, doc: Doc<'a>) -> Result<DocId, Error> {
        self.work.charge(1)?;
        if self.nodes.len() >= self.limits.max_doc_nodes {
            return Err(Error::NodeLimit);
        }
        let mut depth = 1;
        let width = match &doc {
            Doc::Nil | Doc::SoftLineOrEmpty => Some(0),
            Doc::SoftLineOrSpace => Some(1),
            Doc::HardLine => None,
            Doc::Text(s) | Doc::Comment(s) => {
                self.work.charge(s.len())?;
                if s.contains(['\n', '\r']) || matches!(&doc,Doc::Comment(s) if s.starts_with("//"))
                {
                    None
                } else {
                    Some(s.chars().count())
                }
            }
            Doc::Nest(spaces, id) => {
                if *spaces > self.limits.max_output_bytes {
                    return Err(Error::OutputLimit);
                }
                let node = self.node(*id)?;
                depth = node.depth.checked_add(1).ok_or(Error::Overflow)?;
                node.width
            }
            Doc::Group(id) => {
                let node = self.node(*id)?;
                depth = node.depth.checked_add(1).ok_or(Error::Overflow)?;
                node.width
            }
            Doc::Concat(ids) => {
                if ids.len() > self.limits.max_doc_nodes {
                    return Err(Error::NodeLimit);
                }
                self.work.charge(ids.len())?;
                let mut width = Some(0usize);
                for id in ids {
                    let node = self.node(*id)?;
                    depth = depth.max(node.depth.checked_add(1).ok_or(Error::Overflow)?);
                    width = match (width, node.width) {
                        (Some(a), Some(b)) => Some(a.checked_add(b).ok_or(Error::Overflow)?),
                        _ => None,
                    };
                }
                width
            }
        };
        if depth > 256 {
            return Err(Error::DepthLimit);
        }
        self.nodes.try_reserve(1).map_err(|_| Error::Allocation)?;
        let id = DocId {
            index: self.nodes.len(),
            owner: self.owner,
        };
        self.nodes.push(Node { doc, width, depth });
        Ok(id)
    }
    // Reserve text following a group on the same line, including delimiters and
    // a block's opening brace. Otherwise a 100-column header plus " {" spills.
    fn following_width(
        &mut self,
        pending: &[(usize, bool, DocId)],
        remaining: usize,
    ) -> Result<usize, Error> {
        self.work.charge(pending.len())?;
        let mut scan = Vec::new();
        scan.try_reserve(pending.len())
            .map_err(|_| Error::Allocation)?;
        scan.extend_from_slice(pending);
        let mut width = 0usize;
        while let Some((indent, flat, id)) = scan.pop() {
            self.work.charge(1)?;
            let text_bytes = match &self.node(id)?.doc {
                Doc::Text(text) | Doc::Comment(text) => text.len(),
                _ => 0,
            };
            self.work.charge(text_bytes)?;
            match &self.node(id)?.doc {
                Doc::Nil => {}
                Doc::Text(text) | Doc::Comment(text) => {
                    let first = text.split(['\n', '\r']).next().unwrap_or("");
                    width = width
                        .checked_add(first.chars().count())
                        .ok_or(Error::Overflow)?;
                    if first.len() != text.len() {
                        return Ok(width);
                    }
                }
                Doc::HardLine => return Ok(width),
                Doc::SoftLineOrSpace | Doc::SoftLineOrEmpty => {
                    if !flat {
                        return Ok(width);
                    }
                    if matches!(self.node(id)?.doc, Doc::SoftLineOrSpace) {
                        width = width.checked_add(1).ok_or(Error::Overflow)?;
                    }
                }
                Doc::Nest(_, inner) => scan.push((indent, flat, *inner)),
                Doc::Group(inner) => scan.push((indent, false, *inner)),
                Doc::Concat(ids) => {
                    if scan.len().checked_add(ids.len()).ok_or(Error::Overflow)?
                        > self.limits.max_doc_nodes
                    {
                        return Err(Error::NodeLimit);
                    }
                    scan.try_reserve(ids.len()).map_err(|_| Error::Allocation)?;
                    for id in ids.iter().rev() {
                        scan.push((indent, flat, *id));
                    }
                }
            }
            if width > remaining {
                return Ok(width);
            }
        }
        Ok(width)
    }
    pub(crate) fn work_used(&self) -> usize {
        self.work.used
    }
    pub fn render(&mut self, root: DocId) -> Result<String, Error> {
        self.node(root)?;
        let mut output = String::new();
        let mut stack = vec![(0usize, false, root)];
        let mut column = 0usize;
        while let Some((indent, flat, id)) = stack.pop() {
            self.work.charge(1)?;
            let emit_bytes = match &self.node(id)?.doc {
                Doc::Text(text) | Doc::Comment(text) => text.len(),
                _ => 0,
            };
            self.work.charge(emit_bytes)?;
            let node = self.node(id)?;
            match &node.doc {
                Doc::Nil => {}
                Doc::Text(text) | Doc::Comment(text) => {
                    let text = *text;
                    append(&mut output, text, self.limits.max_output_bytes)?;
                    column = if text.contains('\n') {
                        text.rsplit('\n').next().unwrap_or("").chars().count()
                    } else {
                        column
                            .checked_add(text.chars().count())
                            .ok_or(Error::Overflow)?
                    };
                }
                Doc::HardLine | Doc::SoftLineOrSpace | Doc::SoftLineOrEmpty => {
                    if matches!(node.doc, Doc::HardLine) || !flat {
                        append(&mut output, "\n", self.limits.max_output_bytes)?;
                        self.work.charge(indent)?;
                        for _ in 0..indent {
                            append(&mut output, " ", self.limits.max_output_bytes)?;
                        }
                        column = indent;
                    } else if matches!(node.doc, Doc::SoftLineOrSpace) {
                        append(&mut output, " ", self.limits.max_output_bytes)?;
                        column = column.checked_add(1).ok_or(Error::Overflow)?;
                    }
                }
                Doc::Nest(spaces, inner) => {
                    stack.push((
                        indent.checked_add(*spaces).ok_or(Error::Overflow)?,
                        flat,
                        *inner,
                    ));
                }
                Doc::Group(inner) => {
                    let inner = *inner;
                    let available = DEFAULT_LINE_WIDTH.saturating_sub(column);
                    let width = self.node(inner)?.width;
                    let fits = if let Some(width) = width.filter(|w| *w <= available) {
                        let suffix = self.following_width(&stack, available - width)?;
                        suffix <= available - width
                    } else {
                        false
                    };
                    stack.push((indent, fits, inner));
                }
                Doc::Concat(ids) => {
                    if stack.len().checked_add(ids.len()).ok_or(Error::Overflow)?
                        > self.limits.max_doc_nodes
                    {
                        return Err(Error::NodeLimit);
                    }
                    stack
                        .try_reserve(ids.len())
                        .map_err(|_| Error::Allocation)?;
                    for id in ids.iter().rev() {
                        stack.push((indent, flat, *id));
                    }
                }
            }
            self.work.charge(1)?;
        }
        Ok(output)
    }
}
fn append(output: &mut String, text: &str, max: usize) -> Result<(), Error> {
    let len = output
        .len()
        .checked_add(text.len())
        .ok_or(Error::Overflow)?;
    if len > max {
        return Err(Error::OutputLimit);
    }
    output
        .try_reserve(text.len())
        .map_err(|_| Error::Allocation)?;
    output.push_str(text);
    Ok(())
}
