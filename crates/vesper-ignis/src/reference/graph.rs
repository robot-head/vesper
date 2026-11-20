//! Rust build of `RenderGraph`: declaration and validation.

use crate::d3d12::{Access, Layout, QueueKind, Sync};
use crate::graph::{
    BufferDesc, BufferHandle, ByteRange, GraphError, PassDesc, PassId, QueueId, QueueRestriction,
    ResourceRef, SubresourceRange, TextureDesc, TextureHandle, UseDesc, UseDescRange,
    next_graph_id,
};

/// Subresources one texture may have.
const MAX_SUBRESOURCES: u32 = 1 << 20;

impl ByteRange {
    pub(crate) fn resolve(self, len: u64) -> Option<(u64, u64)> {
        if self.offset >= len {
            return None;
        }
        let end = if self.size == u64::MAX {
            len
        } else {
            self.offset.checked_add(self.size)?
        };
        (self.size != 0 && end <= len).then_some((self.offset, end))
    }
}

impl SubresourceRange {
    pub(crate) fn resolve(self, dims: TextureDims) -> Option<SubresourceRange> {
        fn axis(first: u32, count: u32, len: u32) -> Option<u32> {
            if first >= len || count == 0 {
                return None;
            }
            if count == u32::MAX {
                return Some(len - first);
            }
            (first.checked_add(count)? <= len).then_some(count)
        }
        Some(SubresourceRange {
            first_mip: self.first_mip,
            mip_count: axis(self.first_mip, self.mip_count, dims.mips)?,
            first_slice: self.first_slice,
            slice_count: axis(self.first_slice, self.slice_count, dims.slices)?,
            first_plane: self.first_plane,
            plane_count: axis(self.first_plane, self.plane_count, dims.planes)?,
        })
    }

    /// Flat subresource indices (`mip + mips * (slice + slices * plane)`,
    /// the D3D12 subresource numbering) covered by a resolved range.
    pub(crate) fn indices(self, dims: TextureDims) -> impl Iterator<Item = u32> {
        let planes = self.first_plane..self.first_plane + self.plane_count;
        planes.flat_map(move |p| {
            (self.first_slice..self.first_slice + self.slice_count).flat_map(move |s| {
                (self.first_mip..self.first_mip + self.mip_count).map(move |m| dims.flat(m, s, p))
            })
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextureDims {
    pub mips: u32,
    pub slices: u32,
    pub planes: u32,
}

impl TextureDims {
    pub fn count(self) -> u32 {
        self.mips * self.slices * self.planes
    }

    pub fn flat(self, mip: u32, slice: u32, plane: u32) -> u32 {
        mip + self.mips * (slice + self.slices * plane)
    }

    pub fn unflat(self, index: u32) -> (u32, u32, u32) {
        (
            index % self.mips,
            (index / self.mips) % self.slices,
            index / (self.mips * self.slices),
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ResourceKind {
    Buffer {
        size: u64,
    },
    Texture {
        dims: TextureDims,
        initial_layout: Layout,
        final_layout: Option<Layout>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ResourceInfo {
    pub name: String,
    pub kind: ResourceKind,
}

/// Range of one use, after resolution against the resource's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UseRange {
    Bytes { start: u64, end: u64 },
    Subresources(SubresourceRange),
}

#[derive(Clone, Debug)]
pub(crate) struct Use {
    pub resource: u32,
    pub range: UseRange,
    pub access: Access,
    pub sync: Sync,
    pub layout: Option<Layout>,
}

impl Use {
    pub fn is_write(&self) -> bool {
        self.access.is_write()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PassInfo {
    pub name: String,
    pub queue: u16,
    pub uses: Vec<Use>,
}

/// A typed, GPU-independent render graph: queues, resources, passes, and
/// optional explicit ordering edges.
#[derive(Debug)]
pub struct RenderGraph {
    pub(crate) id: u32,
    pub(crate) queues: Vec<QueueKind>,
    pub(crate) resources: Vec<ResourceInfo>,
    pub(crate) passes: Vec<PassInfo>,
    pub(crate) explicit: Vec<(u32, u32)>,
}

impl Default for RenderGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderGraph {
    pub fn new() -> Self {
        RenderGraph {
            id: next_graph_id(),
            queues: Vec::new(),
            resources: Vec::new(),
            passes: Vec::new(),
            explicit: Vec::new(),
        }
    }

    pub fn add_queue(&mut self, kind: QueueKind) -> QueueId {
        let index = u16::try_from(self.queues.len()).expect("more than 65535 queues");
        self.queues.push(kind);
        QueueId {
            graph: self.id,
            index,
        }
    }

    pub fn queue_kind(&self, queue: QueueId) -> Option<QueueKind> {
        (queue.graph == self.id)
            .then(|| self.queues.get(queue.index()).copied())
            .flatten()
    }

    pub fn add_buffer(&mut self, desc: BufferDesc) -> Result<BufferHandle, GraphError> {
        if desc.size == 0 {
            return Err(GraphError::InvalidResource {
                name: desc.name,
                reason: "buffer size is zero",
            });
        }
        let index = self.next_resource_index();
        self.resources.push(ResourceInfo {
            name: desc.name,
            kind: ResourceKind::Buffer { size: desc.size },
        });
        Ok(BufferHandle {
            graph: self.id,
            index,
        })
    }

    pub fn add_texture(&mut self, desc: TextureDesc) -> Result<TextureHandle, GraphError> {
        let invalid = |reason| {
            Err(GraphError::InvalidResource {
                name: desc.name.clone(),
                reason,
            })
        };
        if desc.mip_levels == 0 || desc.array_size == 0 || desc.plane_count == 0 {
            return invalid("texture has a zero dimension");
        }
        let Some(count) = desc
            .mip_levels
            .checked_mul(desc.array_size)
            .and_then(|n| n.checked_mul(desc.plane_count))
        else {
            return invalid("subresource count overflows u32");
        };
        if count > MAX_SUBRESOURCES {
            return invalid("more than 2^20 subresources");
        }
        if desc.final_layout == Some(Layout::Undefined) {
            return invalid("final layout cannot be UNDEFINED");
        }
        let index = self.next_resource_index();
        self.resources.push(ResourceInfo {
            name: desc.name,
            kind: ResourceKind::Texture {
                dims: TextureDims {
                    mips: desc.mip_levels,
                    slices: desc.array_size,
                    planes: desc.plane_count,
                },
                initial_layout: desc.initial_layout,
                final_layout: desc.final_layout,
            },
        });
        Ok(TextureHandle {
            graph: self.id,
            index,
        })
    }

    fn next_resource_index(&self) -> u32 {
        u32::try_from(self.resources.len()).expect("more than u32::MAX resources")
    }

    pub fn resource_name(&self, resource: impl Into<ResourceRef>) -> Option<&str> {
        let r = resource.into();
        (r.graph() == self.id)
            .then(|| self.resources.get(r.index()).map(|i| i.name.as_str()))
            .flatten()
    }

    pub fn pass_name(&self, pass: PassId) -> Option<&str> {
        (pass.graph == self.id)
            .then(|| self.passes.get(pass.index()).map(|p| p.name.as_str()))
            .flatten()
    }

    pub fn pass_count(&self) -> usize {
        self.passes.len()
    }

    /// Validates and appends a pass. Nothing is added on error.
    pub fn add_pass(&mut self, desc: PassDesc) -> Result<PassId, GraphError> {
        let queue_kind = self
            .queue_kind(desc.queue)
            .ok_or(GraphError::ForeignHandle)?;
        let mut uses = Vec::with_capacity(desc.uses.len());
        for (use_index, u) in desc.uses.iter().enumerate() {
            uses.push(self.validate_use(&desc.name, queue_kind, use_index, u)?);
        }
        for i in 0..uses.len() {
            for j in i + 1..uses.len() {
                let (a, b) = (&uses[i], &uses[j]);
                if a.resource == b.resource
                    && overlaps(a.range, b.range)
                    && (a.is_write()
                        || b.is_write()
                        || (a.layout.is_some() && b.layout.is_some() && a.layout != b.layout))
                {
                    return Err(GraphError::ConflictingUses {
                        pass: desc.name,
                        first_use: i,
                        second_use: j,
                    });
                }
                // Overlapping reads are merged per subresource; an explicit
                // layout must also admit the other use's access.
                if a.resource == b.resource && overlaps(a.range, b.range) {
                    for (explicit, other, other_index) in [(a, b, j), (b, a, i)] {
                        if let Some(layout) = explicit.layout
                            && !layout.allows(other.access)
                        {
                            return Err(GraphError::AccessLayoutMismatch {
                                pass: desc.name,
                                use_index: other_index,
                                access: other.access,
                                layout,
                            });
                        }
                    }
                }
            }
        }
        let index = u32::try_from(self.passes.len()).expect("more than u32::MAX passes");
        self.passes.push(PassInfo {
            name: desc.name,
            queue: desc.queue.index,
            uses,
        });
        Ok(PassId {
            graph: self.id,
            index,
        })
    }

    /// Requires `after` to execute after `before` even without a resource
    /// hazard. On different queues this produces a fence wait; on the same
    /// queue it only constrains order (no memory barrier is implied).
    pub fn add_dependency(&mut self, before: PassId, after: PassId) -> Result<(), GraphError> {
        for p in [before, after] {
            if p.graph != self.id || p.index() >= self.passes.len() {
                return Err(GraphError::ForeignHandle);
            }
        }
        if before == after {
            return Err(GraphError::SelfDependency { pass: before });
        }
        if !self.explicit.contains(&(before.index, after.index)) {
            self.explicit.push((before.index, after.index));
        }
        Ok(())
    }

    fn validate_use(
        &self,
        pass: &str,
        queue: QueueKind,
        use_index: usize,
        u: &UseDesc,
    ) -> Result<Use, GraphError> {
        if u.resource.graph() != self.id {
            return Err(GraphError::ForeignHandle);
        }
        let info = self
            .resources
            .get(u.resource.index())
            .ok_or(GraphError::ForeignHandle)?;
        let pass = || pass.to_owned();
        let access = u.access;
        if access == Access::COMMON || access == Access::NO_ACCESS || access.unknown_bits() != 0 {
            return Err(GraphError::InvalidAccess {
                pass: pass(),
                use_index,
                access,
            });
        }
        let sync = u.sync;
        if sync == Sync::NONE || sync.intersects(Sync::SPLIT) {
            return Err(GraphError::InvalidSync {
                pass: pass(),
                use_index,
                sync,
            });
        }
        let bad_access = access.incompatible_with_sync(sync);
        if bad_access != Access::COMMON {
            return Err(GraphError::AccessSyncMismatch {
                pass: pass(),
                use_index,
                access: bad_access,
                sync,
            });
        }
        let restriction = |restriction| GraphError::QueueUnsupported {
            pass: pass(),
            use_index,
            queue,
            restriction,
        };
        if !queue.supported_access().contains(access) {
            return Err(restriction(QueueRestriction::Access(access)));
        }
        if !queue.supported_sync().contains(sync) {
            return Err(restriction(QueueRestriction::Sync(sync)));
        }
        let range = match (&info.kind, &u.range) {
            (ResourceKind::Buffer { size }, UseDescRange::Bytes(r)) => {
                if access.texture_only() != Access::COMMON {
                    return Err(GraphError::AccessResourceKindMismatch {
                        pass: pass(),
                        use_index,
                        access,
                    });
                }
                let (start, end) = r.resolve(*size).ok_or_else(|| GraphError::InvalidRange {
                    pass: pass(),
                    use_index,
                })?;
                UseRange::Bytes { start, end }
            }
            (ResourceKind::Texture { dims, .. }, UseDescRange::Subresources(r)) => {
                if access.buffer_only() {
                    return Err(GraphError::AccessResourceKindMismatch {
                        pass: pass(),
                        use_index,
                        access,
                    });
                }
                if let Some(layout) = u.layout {
                    if layout == Layout::Undefined || !layout.allows(access) {
                        return Err(GraphError::AccessLayoutMismatch {
                            pass: pass(),
                            use_index,
                            access,
                            layout,
                        });
                    }
                    if !queue.supports_layout(layout) {
                        return Err(restriction(QueueRestriction::Layout(layout)));
                    }
                }
                let r = r.resolve(*dims).ok_or_else(|| GraphError::InvalidRange {
                    pass: pass(),
                    use_index,
                })?;
                UseRange::Subresources(r)
            }
            // The typed builder makes these unreachable.
            _ => return Err(GraphError::ForeignHandle),
        };
        Ok(Use {
            resource: u.resource.index() as u32,
            range,
            access,
            sync,
            layout: u.layout,
        })
    }
}

pub(crate) fn overlaps(a: UseRange, b: UseRange) -> bool {
    fn axis(a0: u32, an: u32, b0: u32, bn: u32) -> bool {
        a0 < b0 + bn && b0 < a0 + an
    }
    match (a, b) {
        (UseRange::Bytes { start: s0, end: e0 }, UseRange::Bytes { start: s1, end: e1 }) => {
            s0 < e1 && s1 < e0
        }
        (UseRange::Subresources(x), UseRange::Subresources(y)) => {
            axis(x.first_mip, x.mip_count, y.first_mip, y.mip_count)
                && axis(x.first_slice, x.slice_count, y.first_slice, y.slice_count)
                && axis(x.first_plane, x.plane_count, y.first_plane, y.plane_count)
        }
        _ => false,
    }
}
