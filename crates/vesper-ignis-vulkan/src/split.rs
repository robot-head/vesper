//! Pairing of D3D12 split-barrier halves within one queue stream (no
//! `unsafe`).
//!
//! The planner emits a split barrier as a *begin* half (`SyncAfter = SPLIT`)
//! after the producer and an *end* half (`SyncBefore = SPLIT`) before the
//! consumer, on the same queue, with every other field equal. Vulkan needs
//! the whole dependency at both ends (`vkCmdSetEvent2` and
//! `vkCmdWaitEvents2` must be given identical `VkDependencyInfo`s), so each
//! stream is scanned first and every half is matched with its partner: the
//! whole barrier is the begin half with the end half's `SyncAfter`.
//! Unmatched or inconsistent halves are typed errors before anything is
//! recorded.

use std::collections::BTreeMap;
use vesper_ignis::lifecycle::QueueCommand;
use vesper_ignis::{Barrier, BufferBarrier, ResourceRef, SubresourceRange, Sync, TextureBarrier};

/// What one barrier of a stream is after pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// An ordinary barrier.
    Plain,
    /// The begin half of pair `n` (`vkCmdSetEvent2`).
    Begin(usize),
    /// The end half of pair `n` (`vkCmdWaitEvents2`).
    End(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitError {
    /// An end half with no earlier begin half on the same stream.
    UnmatchedEnd(ResourceRef),
    /// A begin half that no later end half closes.
    UnmatchedBegin(ResourceRef),
    /// A second begin half for a resource whose split is still open.
    Reopened(ResourceRef),
    /// The halves disagree on accesses, layouts or the producer scope.
    Mismatch(ResourceRef),
    /// A half with `SPLIT` on both sides, or `SPLIT` combined with other bits.
    Malformed(ResourceRef),
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnmatchedEnd(r) => write!(f, "split barrier end on {r:?} has no begin"),
            Self::UnmatchedBegin(r) => write!(f, "split barrier begin on {r:?} is never ended"),
            Self::Reopened(r) => write!(f, "split barrier on {r:?} begun twice"),
            Self::Mismatch(r) => write!(f, "split barrier halves on {r:?} disagree"),
            Self::Malformed(r) => write!(f, "malformed split barrier half on {r:?}"),
        }
    }
}
impl std::error::Error for SplitError {}

/// The pairing of one stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pairing {
    /// Role of `(command index, barrier index)`; absent means [`Role::Plain`].
    pub roles: BTreeMap<(usize, usize), Role>,
    /// The whole barrier of each pair.
    pub pairs: Vec<Barrier>,
    /// Command index of each pair's end half.
    pub end_command: Vec<usize>,
}

impl Pairing {
    pub fn role(&self, command: usize, barrier: usize) -> Role {
        self.roles
            .get(&(command, barrier))
            .copied()
            .unwrap_or(Role::Plain)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Buffer(usize),
    Texture(usize, [u32; 6]),
}

fn range_key(r: SubresourceRange) -> [u32; 6] {
    [
        r.first_mip,
        r.mip_count,
        r.first_slice,
        r.slice_count,
        r.first_plane,
        r.plane_count,
    ]
}

fn key(b: &Barrier) -> (Key, ResourceRef) {
    match b {
        Barrier::Buffer(b) => (
            Key::Buffer(ResourceRef::from(b.resource).index()),
            b.resource.into(),
        ),
        Barrier::Texture(t) => (
            Key::Texture(
                ResourceRef::from(t.resource).index(),
                range_key(t.subresources),
            ),
            t.resource.into(),
        ),
    }
}

fn syncs(b: &Barrier) -> (Sync, Sync) {
    match b {
        Barrier::Buffer(b) => (b.sync_before, b.sync_after),
        Barrier::Texture(t) => (t.sync_before, t.sync_after),
    }
}

/// The whole barrier of a begin/end pair, or `None` when they disagree.
fn join(begin: &Barrier, end: &Barrier) -> Option<Barrier> {
    match (begin, end) {
        (Barrier::Buffer(b), Barrier::Buffer(e)) => {
            let whole = BufferBarrier {
                sync_after: e.sync_after,
                ..*b
            };
            (BufferBarrier {
                sync_before: b.sync_before,
                ..*e
            } == whole)
                .then_some(Barrier::Buffer(whole))
        }
        (Barrier::Texture(b), Barrier::Texture(e)) => {
            let whole = TextureBarrier {
                sync_after: e.sync_after,
                ..*b
            };
            (TextureBarrier {
                sync_before: b.sync_before,
                ..*e
            } == whole)
                .then_some(Barrier::Texture(whole))
        }
        _ => None,
    }
}

/// Pairs the split halves of one stream's commands.
pub fn pair(commands: &[QueueCommand]) -> Result<Pairing, SplitError> {
    let mut open: BTreeMap<Key, (usize, usize, Barrier, ResourceRef)> = BTreeMap::new();
    let mut out = Pairing::default();
    for (ci, c) in commands.iter().enumerate() {
        let QueueCommand::Barriers(list) = c else {
            continue;
        };
        for (bi, b) in list.iter().enumerate() {
            let (before, after) = syncs(b);
            let (k, r) = key(b);
            let begin = after.contains(Sync::SPLIT);
            let end = before.contains(Sync::SPLIT);
            if (begin && (end || after != Sync::SPLIT)) || (end && before != Sync::SPLIT) {
                return Err(SplitError::Malformed(r));
            }
            if begin {
                if open.contains_key(&k) {
                    return Err(SplitError::Reopened(r));
                }
                open.insert(k, (ci, bi, *b, r));
            } else if end {
                let Some((bci, bbi, first, _)) = open.remove(&k) else {
                    return Err(SplitError::UnmatchedEnd(r));
                };
                let whole = join(&first, b).ok_or(SplitError::Mismatch(r))?;
                let n = out.pairs.len();
                out.pairs.push(whole);
                out.end_command.push(ci);
                out.roles.insert((bci, bbi), Role::Begin(n));
                out.roles.insert((ci, bi), Role::End(n));
            }
        }
    }
    if let Some((_, _, _, r)) = open.into_values().next() {
        return Err(SplitError::UnmatchedBegin(r));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesper_ignis::{Access, Layout, RenderGraph, TextureDesc};

    fn tex() -> vesper_ignis::TextureHandle {
        RenderGraph::new()
            .add_texture(TextureDesc::simple("t", Layout::Undefined))
            .unwrap()
    }

    fn half(t: vesper_ignis::TextureHandle, before: Sync, after: Sync) -> Barrier {
        Barrier::Texture(TextureBarrier {
            resource: t,
            sync_before: before,
            sync_after: after,
            access_before: Access::RENDER_TARGET,
            access_after: Access::SHADER_RESOURCE,
            layout_before: Layout::RenderTarget,
            layout_after: Layout::ShaderResource,
            subresources: SubresourceRange::ALL,
        })
    }

    #[test]
    fn halves_pair_into_the_whole_barrier() {
        let t = tex();
        let cmds = vec![
            QueueCommand::Barriers(vec![half(t, Sync::RENDER_TARGET, Sync::SPLIT)]),
            QueueCommand::Barriers(vec![half(t, Sync::SPLIT, Sync::COMPUTE_SHADING)]),
        ];
        let p = pair(&cmds).unwrap();
        assert_eq!(p.role(0, 0), Role::Begin(0));
        assert_eq!(p.role(1, 0), Role::End(0));
        assert_eq!(
            p.pairs[0],
            half(t, Sync::RENDER_TARGET, Sync::COMPUTE_SHADING)
        );
    }

    #[test]
    fn inconsistent_halves_are_typed_errors() {
        let t = tex();
        let r: ResourceRef = t.into();
        let begin = QueueCommand::Barriers(vec![half(t, Sync::RENDER_TARGET, Sync::SPLIT)]);
        let end = QueueCommand::Barriers(vec![half(t, Sync::SPLIT, Sync::COMPUTE_SHADING)]);
        assert_eq!(
            pair(std::slice::from_ref(&end)),
            Err(SplitError::UnmatchedEnd(r))
        );
        assert_eq!(
            pair(std::slice::from_ref(&begin)),
            Err(SplitError::UnmatchedBegin(r))
        );
        assert_eq!(
            pair(&[begin.clone(), begin.clone()]),
            Err(SplitError::Reopened(r))
        );
        let Barrier::Texture(mut other) = half(t, Sync::SPLIT, Sync::COMPUTE_SHADING) else {
            unreachable!()
        };
        other.layout_after = Layout::CopySource;
        assert_eq!(
            pair(&[begin, QueueCommand::Barriers(vec![Barrier::Texture(other)])]),
            Err(SplitError::Mismatch(r))
        );
        assert_eq!(
            pair(&[QueueCommand::Barriers(vec![half(
                t,
                Sync::SPLIT,
                Sync::SPLIT
            )])]),
            Err(SplitError::Malformed(r))
        );
    }
}
