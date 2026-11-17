//! Buffer sub-range barriers (no `unsafe`).
//!
//! D3D12 enhanced buffer barriers must cover the whole buffer, so the
//! planner emits whole-buffer barriers even though it tracks hazards per byte
//! range. Vulkan buffer memory barriers take an `offset`/`size`, so the
//! backend narrows each one to the bytes the GPU work it protects can touch:
//! the hull of the ranges that the passes after the barrier, on the same
//! stream and up to the next barrier batch naming the same buffer, access.
//! Every hazard the barrier resolves lies in that range (a hazard needs an
//! access on the barrier's second side), so the narrowing never drops a
//! needed dependency; work on other queues is ordered by semaphores, which
//! carry full memory dependencies. When no later pass on the stream touches
//! the buffer (a handoff before a signal), or one touches it as a whole, the
//! barrier stays whole.

use vesper_ignis::lifecycle::QueueCommand;
use vesper_ignis::{Barrier, BufferHandle, PassId};

/// A byte range `offset..offset + size`; `size == u64::MAX` is the rest of
/// the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bytes {
    pub offset: u64,
    pub size: u64,
}

impl Bytes {
    pub const WHOLE: Bytes = Bytes {
        offset: 0,
        size: u64::MAX,
    };
    pub fn is_whole(self) -> bool {
        self.offset == 0 && self.size == u64::MAX
    }
    fn end(self) -> Option<u64> {
        (self.size != u64::MAX).then(|| self.offset.saturating_add(self.size))
    }
}

fn names(list: &[Barrier], buffer: BufferHandle) -> bool {
    list.iter()
        .any(|b| matches!(b, Barrier::Buffer(bb) if bb.resource == buffer))
}

/// The range a buffer barrier at command `at` of a stream needs to cover.
/// `touches(pass)` lists the ranges a pass's recorded work accesses on each
/// buffer. Returns [`Bytes::WHOLE`] when the barrier cannot be narrowed.
pub fn barrier_range(
    commands: &[QueueCommand],
    at: usize,
    buffer: BufferHandle,
    touches: impl Fn(PassId) -> Vec<(BufferHandle, Bytes)>,
) -> Bytes {
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    let mut any = false;
    for c in commands.iter().skip(at + 1) {
        match c {
            QueueCommand::Barriers(list) if names(list, buffer) => break,
            QueueCommand::Execute(pass) => {
                for (b, r) in touches(*pass) {
                    if b != buffer {
                        continue;
                    }
                    let Some(end) = r.end() else {
                        return Bytes::WHOLE;
                    };
                    any = true;
                    lo = lo.min(r.offset);
                    hi = hi.max(end);
                }
            }
            _ => {}
        }
    }
    if !any || hi <= lo {
        return Bytes::WHOLE;
    }
    Bytes {
        offset: lo,
        size: hi - lo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesper_ignis::{
        Access, BufferBarrier, BufferDesc, ByteRange, PassDesc, QueueKind, RenderGraph, Sync,
    };

    #[test]
    fn narrows_to_the_hull_of_later_accesses_until_the_next_barrier() {
        let mut g = RenderGraph::new();
        let q = g.add_queue(QueueKind::Direct);
        let buf = g
            .add_buffer(BufferDesc {
                name: "b".into(),
                size: 256,
            })
            .unwrap();
        let mut passes = Vec::new();
        for i in 0..4 {
            passes.push(
                g.add_pass(PassDesc::new(format!("p{i}"), q).buffer(
                    buf,
                    ByteRange::WHOLE,
                    Access::COPY_DEST,
                    Sync::COPY,
                ))
                .unwrap(),
            );
        }
        let barrier = || {
            QueueCommand::Barriers(vec![Barrier::Buffer(BufferBarrier {
                resource: buf,
                sync_before: Sync::COPY,
                sync_after: Sync::COPY,
                access_before: Access::COPY_DEST,
                access_after: Access::COPY_DEST,
            })])
        };
        let cmds = vec![
            QueueCommand::Execute(passes[0]),
            barrier(),
            QueueCommand::Execute(passes[1]),
            QueueCommand::Execute(passes[2]),
            barrier(),
            QueueCommand::Execute(passes[3]),
        ];
        let touches = |p: PassId| {
            let r = match p {
                p if p == passes[1] => Bytes {
                    offset: 64,
                    size: 32,
                },
                p if p == passes[2] => Bytes {
                    offset: 128,
                    size: 16,
                },
                p if p == passes[3] => Bytes::WHOLE,
                _ => Bytes { offset: 0, size: 8 },
            };
            vec![(buf, r)]
        };
        assert_eq!(
            barrier_range(&cmds, 1, buf, touches),
            Bytes {
                offset: 64,
                size: 80
            }
        );
        assert!(barrier_range(&cmds, 4, buf, touches).is_whole());
        // Nothing after the barrier touches the buffer: stay whole.
        assert!(barrier_range(&cmds[..2], 1, buf, touches).is_whole());
    }
}
