//! Versioned scalar relative-pointer archive, schema 1 (ordered rooted trees).
//!
//! Wire integers are little-endian. Signed offsets are relative to the address
//! of the offset field, not the object or end of the field. Alignment concerns
//! archive-relative offsets, not the host address: all reads copy bytes safely.
//! See the crate README for the complete canonical format and scope.
use crate::binary::{Error, ErrorKind, Result, add, bound, mul, work};
use core::ops::Range;

pub const MAGIC: [u8; 4] = *b"VSPA";
pub const VERSION: u16 = 1;
pub const TREE_SCHEMA: u16 = 1;
pub const HEADER_LEN: usize = 32;
pub const NODE_LEN: usize = 32;

/// Unvalidated wire displacement. Resolving checks a range, not a schema/type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelPtr32 {
    pub offset: i32,
}
impl RelPtr32 {
    pub fn between(field_offset: usize, target_offset: usize) -> Result<Self> {
        let offset = if target_offset >= field_offset {
            i32::try_from(target_offset - field_offset)
                .map_err(|_| error(ErrorKind::Overflow, field_offset))?
        } else {
            let magnitude = field_offset - target_offset;
            if magnitude == (i32::MAX as usize) + 1 {
                i32::MIN
            } else {
                -i32::try_from(magnitude).map_err(|_| error(ErrorKind::Overflow, field_offset))?
            }
        };
        Ok(Self { offset })
    }
    pub fn resolve(
        self,
        field_offset: usize,
        size: usize,
        alignment: usize,
        archive_len: usize,
    ) -> Result<Range<usize>> {
        field(field_offset, 4, archive_len)?;
        alignment_valid(alignment)?;
        let target = if self.offset < 0 {
            field_offset.checked_sub(self.offset.unsigned_abs() as usize)
        } else {
            field_offset.checked_add(self.offset as usize)
        }
        .ok_or(error(ErrorKind::Offset, field_offset))?;
        if target % alignment != 0 {
            return Err(error(ErrorKind::Alignment, field_offset));
        }
        let end = add(target, size)?;
        if end > archive_len {
            return Err(error(ErrorKind::Offset, field_offset));
        }
        Ok(target..end)
    }
}

/// Unvalidated offset/count pair. Empty slices canonically have offset zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelSlice32 {
    pub offset: i32,
    pub count: u32,
}
impl RelSlice32 {
    pub fn resolve(
        self,
        field_offset: usize,
        item_size: usize,
        alignment: usize,
        archive_len: usize,
    ) -> Result<Range<usize>> {
        field(field_offset, 8, archive_len)?;
        alignment_valid(alignment)?;
        if item_size == 0 {
            return Err(error(ErrorKind::Layout, field_offset));
        }
        if self.count == 0 {
            if self.offset != 0 {
                return Err(error(ErrorKind::Layout, field_offset));
            }
            return Ok(0..0);
        }
        RelPtr32 {
            offset: self.offset,
        }
        .resolve(
            field_offset,
            mul(self.count as usize, item_size)?,
            alignment,
            archive_len,
        )
    }
}
fn alignment_valid(alignment: usize) -> Result<()> {
    if !alignment.is_power_of_two() {
        return Err(error(ErrorKind::Alignment, 0));
    }
    Ok(())
}
fn field(offset: usize, size: usize, len: usize) -> Result<()> {
    if !offset.is_multiple_of(4) {
        return Err(error(ErrorKind::Alignment, offset));
    }
    if add(offset, size)? > len {
        return Err(error(ErrorKind::Offset, offset));
    }
    Ok(())
}
fn error(kind: ErrorKind, offset: usize) -> Error {
    Error::new(kind, offset)
}
fn aligned(n: usize) -> Result<usize> {
    Ok(add(n, 3)? & !3)
}
fn node_offset(index: usize) -> Result<usize> {
    add(HEADER_LEN, mul(index, NODE_LEN)?)
}
fn get_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let word = bytes
        .get(offset..add(offset, 4)?)
        .ok_or(error(ErrorKind::Truncated, offset))?;
    Ok(u32::from_le_bytes(word.try_into().expect("four bytes")))
}
fn get_i32(bytes: &[u8], offset: usize) -> Result<i32> {
    Ok(get_u32(bytes, offset)? as i32)
}
fn get_slice(bytes: &[u8], offset: usize) -> Result<RelSlice32> {
    Ok(RelSlice32 {
        offset: get_i32(bytes, offset)?,
        count: get_u32(bytes, add(offset, 4)?)?,
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_archive_bytes: usize,
    pub max_nodes: usize,
    pub max_children_per_node: usize,
    pub max_edges: usize,
    /// Root depth is one. Validation is iterative, never graph-recursive.
    pub max_depth: usize,
    pub max_label_bytes: usize,
    pub max_total_label_bytes: usize,
    /// Requested payload bytes for the temporary metadata vector, not allocator overhead.
    pub max_validation_bytes: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 4 * 1024 * 1024,
            max_nodes: 4096,
            max_children_per_node: 4095,
            max_edges: 4095,
            max_depth: 128,
            max_label_bytes: 65_536,
            max_total_label_bytes: 1024 * 1024,
            max_validation_bytes: 128 * 1024,
            max_work: 16 * 1024 * 1024,
        }
    }
}

/// Encoding input: child indices address this same node slice. Root is index
/// zero; every other node must have exactly one parent with a smaller index.
/// This canonical topological tree schema forbids cycles, sharing and orphans.
#[derive(Clone, Copy, Debug)]
pub struct TreeNode<'a> {
    pub value: u32,
    pub label: &'a str,
    pub children: &'a [u32],
}
/// Requested metadata payload for encoding preflight or validation. This does
/// not include allocator bookkeeping. No memory is allocated by this calculator.
pub fn validation_bytes(count: usize) -> Result<usize> {
    mul(count, size_of::<Meta>())
}

#[derive(Clone, Copy, Default)]
struct Meta {
    parents: u32,
    depth: usize,
}
struct Check {
    limits: Limits,
    remaining: usize,
    edges: usize,
    labels: usize,
    meta: Vec<Meta>,
}
impl Check {
    fn new(count: usize, limits: Limits) -> Result<Self> {
        if count == 0 {
            return Err(error(ErrorKind::Tree, 12));
        }
        bound(count, limits.max_nodes, ErrorKind::CountLimit)?;
        bound(count, u32::MAX as usize, ErrorKind::Overflow)?;
        bound(1, limits.max_depth, ErrorKind::DepthLimit)?;
        let bytes = validation_bytes(count)?;
        bound(
            bytes,
            limits.max_validation_bytes,
            ErrorKind::AllocationLimit,
        )?;
        let mut remaining = limits.max_work;
        work(&mut remaining, count)?;
        let mut meta = Vec::new();
        meta.try_reserve_exact(count)
            .map_err(|_| error(ErrorKind::AllocationFailed, 0))?;
        meta.resize(count, Meta::default());
        meta[0].depth = 1;
        Ok(Self {
            limits,
            remaining,
            edges: 0,
            labels: 0,
            meta,
        })
    }
    fn label(&mut self, len: usize) -> Result<()> {
        bound(len, self.limits.max_label_bytes, ErrorKind::StringLimit)?;
        self.labels = add(self.labels, len)?;
        bound(
            self.labels,
            self.limits.max_total_label_bytes,
            ErrorKind::StringLimit,
        )?;
        work(&mut self.remaining, len)
    }
    fn node(&mut self, index: usize, children: usize) -> Result<()> {
        work(&mut self.remaining, NODE_LEN)?;
        if self.meta[index].depth == 0 {
            return Err(error(ErrorKind::Tree, node_offset(index)?));
        }
        bound(
            children,
            self.limits.max_children_per_node,
            ErrorKind::CountLimit,
        )?;
        self.edges = add(self.edges, children)?;
        bound(self.edges, self.limits.max_edges, ErrorKind::CountLimit)
    }
    fn edge(&mut self, parent: usize, child: usize) -> Result<()> {
        work(&mut self.remaining, 8)?;
        if child <= parent || child >= self.meta.len() {
            return Err(error(ErrorKind::Tree, node_offset(parent)?));
        }
        if self.meta[child].parents != 0 {
            return Err(error(ErrorKind::Tree, node_offset(child)?));
        }
        let depth = add(self.meta[parent].depth, 1)?;
        bound(depth, self.limits.max_depth, ErrorKind::DepthLimit)?;
        self.meta[child] = Meta { parents: 1, depth };
        Ok(())
    }
    fn size(&self, bytes: usize) -> Result<()> {
        bound(bytes, self.limits.max_archive_bytes, ErrorKind::ByteLimit)?;
        // This V1 subset keeps the entire archive within a signed displacement span.
        bound(bytes, i32::MAX as usize, ErrorKind::Overflow)
    }
}

/// Full preflight, including schema/tree/limits; no caller buffer is modified.
/// One bounded, fallible temporary metadata allocation is made and then dropped.
pub fn encoded_size(nodes: &[TreeNode<'_>], limits: Limits) -> Result<usize> {
    let mut check = Check::new(nodes.len(), limits)?;
    let mut cursor = node_offset(nodes.len())?;
    check.size(cursor)?;
    for (i, node) in nodes.iter().enumerate() {
        check.node(i, node.children.len())?;
        check.label(node.label.len())?;
        cursor = aligned(add(cursor, node.label.len())?)?;
        cursor = add(cursor, mul(node.children.len(), 4)?)?;
        check.size(cursor)?;
        for &child in node.children {
            check.edge(i, child as usize)?;
        }
    }
    // Both initialization and actual encoding touch at most two complete prefixes.
    work(&mut check.remaining, mul(cursor, 2)?)?;
    Ok(cursor)
}

/// Writes the canonical V1/tree archive only after complete preflight and one
/// output capacity check. All Result failures preserve every output byte.
pub fn encode_into(nodes: &[TreeNode<'_>], output: &mut [u8], limits: Limits) -> Result<usize> {
    let size = encoded_size(nodes, limits)?;
    if output.len() < size {
        return Err(error(ErrorKind::OutputTooSmall, 0));
    }
    output[..size].fill(0);
    output[..4].copy_from_slice(&MAGIC);
    output[4..6].copy_from_slice(&VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&TREE_SCHEMA.to_le_bytes());
    put_u32(output, 8, size as u32);
    put_u32(output, 12, nodes.len() as u32);
    put_i32(output, 16, (HEADER_LEN - 16) as i32);
    put_i32(output, 20, (HEADER_LEN - 20) as i32);
    put_u32(output, 24, nodes.len() as u32);
    let mut cursor = HEADER_LEN + nodes.len() * NODE_LEN;
    for (i, node) in nodes.iter().enumerate() {
        let offset = HEADER_LEN + i * NODE_LEN;
        put_u32(output, offset, node.value);
        if !node.label.is_empty() {
            put_relative(output, offset + 4, cursor);
            put_u32(output, offset + 8, node.label.len() as u32);
            output[cursor..cursor + node.label.len()].copy_from_slice(node.label.as_bytes());
            cursor += node.label.len();
        }
        cursor = (cursor + 3) & !3;
        if !node.children.is_empty() {
            put_relative(output, offset + 12, cursor);
            put_u32(output, offset + 16, node.children.len() as u32);
            for &child in node.children {
                put_relative(output, cursor, HEADER_LEN + child as usize * NODE_LEN);
                cursor += 4;
            }
        }
    }
    debug_assert_eq!(cursor, size);
    Ok(size)
}
fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_i32(output: &mut [u8], offset: usize, value: i32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_relative(output: &mut [u8], field: usize, target: usize) {
    put_i32(
        output,
        field,
        RelPtr32::between(field, target)
            .expect("preflight signed span")
            .offset,
    );
}

/// Fully validated borrowed archive. No pointer/view can be published by a raw
/// byte slice without successful validation. The temporary index is not retained.
#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> {
    bytes: &'a [u8],
    count: usize,
}
impl<'a> Archive<'a> {
    pub fn validate(bytes: &'a [u8], limits: Limits) -> Result<Self> {
        bound(bytes.len(), limits.max_archive_bytes, ErrorKind::ByteLimit)?;
        bound(bytes.len(), i32::MAX as usize, ErrorKind::Overflow)?;
        if bytes.len() < HEADER_LEN {
            return Err(error(ErrorKind::Truncated, bytes.len()));
        }
        if bytes[..4] != MAGIC {
            return Err(error(ErrorKind::Magic, 0));
        }
        if bytes[4..6] != VERSION.to_le_bytes() {
            return Err(error(ErrorKind::Version, 4));
        }
        if bytes[6..8] != TREE_SCHEMA.to_le_bytes() {
            return Err(error(ErrorKind::Schema, 6));
        }
        let total = get_u32(bytes, 8)? as usize;
        if total > bytes.len() {
            return Err(error(ErrorKind::Truncated, bytes.len()));
        }
        if total < bytes.len() {
            return Err(error(ErrorKind::TrailingBytes, total));
        }
        if get_u32(bytes, 28)? != 0 {
            return Err(error(ErrorKind::Layout, 28));
        }
        let count = get_u32(bytes, 12)? as usize;
        bound(count, limits.max_nodes, ErrorKind::CountLimit)?;
        let nodes = get_slice(bytes, 20)?;
        if nodes.count as usize != count {
            return Err(error(ErrorKind::Layout, 24));
        }
        let range = nodes.resolve(20, NODE_LEN, 4, bytes.len())?;
        if range.start != HEADER_LEN {
            return Err(error(ErrorKind::Layout, 20));
        }
        let root = RelPtr32 {
            offset: get_i32(bytes, 16)?,
        }
        .resolve(16, NODE_LEN, 4, bytes.len())?;
        if root.start != HEADER_LEN {
            return Err(error(ErrorKind::Tree, 16));
        }
        // A hostile count must fit actual node bytes before any metadata
        // allocation/initialization, even when caller-supplied limits are large.
        let mut check = Check::new(count, limits)?;
        check.size(bytes.len())?;
        work(&mut check.remaining, bytes.len())?;
        let mut cursor = range.end;
        for i in 0..count {
            let offset = node_offset(i)?;
            for reserved in [20, 24, 28] {
                if get_u32(bytes, offset + reserved)? != 0 {
                    return Err(error(ErrorKind::Layout, offset + reserved));
                }
            }
            let label = get_slice(bytes, offset + 4)?;
            let children = get_slice(bytes, offset + 12)?;
            check.node(i, children.count as usize)?;
            check.label(label.count as usize)?;
            let label_range = label.resolve(offset + 4, 1, 1, bytes.len())?;
            if label.count != 0 {
                if label_range.start != cursor {
                    return Err(error(ErrorKind::Layout, offset + 4));
                }
                core::str::from_utf8(&bytes[label_range.clone()])
                    .map_err(|e| error(ErrorKind::Utf8, label_range.start + e.valid_up_to()))?;
                cursor = label_range.end;
            }
            let next = aligned(cursor)?;
            let padding = bytes
                .get(cursor..next)
                .ok_or(error(ErrorKind::Truncated, cursor))?;
            if padding.iter().any(|b| *b != 0) {
                return Err(error(ErrorKind::Layout, cursor));
            }
            cursor = next;
            let child_range = children.resolve(offset + 12, 4, 4, bytes.len())?;
            if children.count != 0 {
                if child_range.start != cursor {
                    return Err(error(ErrorKind::Layout, offset + 12));
                }
                for entry in (child_range.start..child_range.end).step_by(4) {
                    let target = RelPtr32 {
                        offset: get_i32(bytes, entry)?,
                    }
                    .resolve(entry, NODE_LEN, 4, bytes.len())?;
                    if target.start < HEADER_LEN
                        || target.start >= range.end
                        || !(target.start - HEADER_LEN).is_multiple_of(NODE_LEN)
                    {
                        return Err(error(ErrorKind::Tree, entry));
                    }
                    check.edge(i, (target.start - HEADER_LEN) / NODE_LEN)?;
                }
                cursor = child_range.end;
            }
        }
        if cursor != bytes.len() {
            return Err(error(ErrorKind::TrailingBytes, cursor));
        }
        Ok(Self { bytes, count })
    }
    pub fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }
    pub fn len(self) -> usize {
        self.count
    }
    /// Valid V1 archives always have a root, so this is always false.
    pub fn is_empty(self) -> bool {
        self.count == 0
    }
    pub fn root(self) -> NodeView<'a> {
        NodeView {
            archive: self,
            index: 0,
        }
    }
    pub fn node(self, index: usize) -> Option<NodeView<'a>> {
        (index < self.count).then_some(NodeView {
            archive: self,
            index,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct NodeView<'a> {
    archive: Archive<'a>,
    index: usize,
}
impl<'a> NodeView<'a> {
    fn offset(self) -> usize {
        HEADER_LEN + self.index * NODE_LEN
    }
    pub fn index(self) -> usize {
        self.index
    }
    pub fn value(self) -> u32 {
        get_u32(self.archive.bytes, self.offset()).expect("validated node")
    }
    /// Constant-size checked range computation, borrowing the original bytes.
    pub fn label_bytes(self) -> &'a [u8] {
        let slice = get_slice(self.archive.bytes, self.offset() + 4).expect("validated label");
        let range = slice
            .resolve(self.offset() + 4, 1, 1, self.archive.bytes.len())
            .expect("validated label range");
        &self.archive.bytes[range]
    }
    /// Rechecks UTF-8 safely when creating a str; this is O(label bytes), with
    /// no allocation or copying. label_bytes() avoids that repeated scan.
    pub fn label(self) -> &'a str {
        core::str::from_utf8(self.label_bytes()).expect("validated UTF-8 label")
    }
    pub fn children(self) -> Children<'a> {
        let slice = get_slice(self.archive.bytes, self.offset() + 12).expect("validated children");
        let range = slice
            .resolve(self.offset() + 12, 4, 4, self.archive.bytes.len())
            .expect("validated children range");
        Children {
            archive: self.archive,
            pos: range.start,
            end: range.end,
        }
    }
}
pub struct Children<'a> {
    archive: Archive<'a>,
    pos: usize,
    end: usize,
}
impl<'a> Iterator for Children<'a> {
    type Item = NodeView<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.end {
            return None;
        }
        let ptr = RelPtr32 {
            offset: get_i32(self.archive.bytes, self.pos).expect("validated child"),
        };
        let target = ptr
            .resolve(self.pos, NODE_LEN, 4, self.archive.bytes.len())
            .expect("validated child target");
        self.pos += 4;
        Some(NodeView {
            archive: self.archive,
            index: (target.start - HEADER_LEN) / NODE_LEN,
        })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = (self.end - self.pos) / 4;
        (n, Some(n))
    }
}
impl ExactSizeIterator for Children<'_> {}
