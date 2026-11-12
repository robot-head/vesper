//! Typed resource and pass declarations.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::d3d12::{Access, Layout, QueueKind, Sync};

static NEXT_GRAPH_ID: AtomicU32 = AtomicU32::new(1);

/// A queue registered with a [`RenderGraph`]. Each queue has one timeline
/// fence in the produced plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueueId {
    pub(crate) graph: u32,
    pub(crate) index: u16,
}

impl QueueId {
    pub fn index(self) -> usize {
        self.index as usize
    }
}

/// A buffer resource. Buffers have no layout and are tracked by byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferHandle {
    pub(crate) graph: u32,
    pub(crate) index: u32,
}

/// A texture resource. Textures are tracked per subresource
/// (mip level, array slice, plane), each with its own layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureHandle {
    pub(crate) graph: u32,
    pub(crate) index: u32,
}

/// Either kind of resource, as reported in plans and errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceRef {
    Buffer(BufferHandle),
    Texture(TextureHandle),
}

impl ResourceRef {
    /// Index into the graph's resource table, shared by buffers and textures.
    pub fn index(self) -> usize {
        match self {
            ResourceRef::Buffer(b) => b.index as usize,
            ResourceRef::Texture(t) => t.index as usize,
        }
    }

    fn graph(self) -> u32 {
        match self {
            ResourceRef::Buffer(b) => b.graph,
            ResourceRef::Texture(t) => t.graph,
        }
    }
}

impl From<BufferHandle> for ResourceRef {
    fn from(value: BufferHandle) -> Self {
        ResourceRef::Buffer(value)
    }
}

impl From<TextureHandle> for ResourceRef {
    fn from(value: TextureHandle) -> Self {
        ResourceRef::Texture(value)
    }
}

/// A declared pass. Declaration order is the graph's sequential meaning:
/// every read observes the latest earlier overlapping write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PassId {
    pub(crate) graph: u32,
    pub(crate) index: u32,
}

impl PassId {
    pub fn index(self) -> usize {
        self.index as usize
    }
}

/// A byte range of a buffer. `size == u64::MAX` means "to the end".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub offset: u64,
    pub size: u64,
}

impl ByteRange {
    pub const WHOLE: ByteRange = ByteRange {
        offset: 0,
        size: u64::MAX,
    };

    pub const fn new(offset: u64, size: u64) -> Self {
        ByteRange { offset, size }
    }

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

/// A box of texture subresources, mirroring
/// `D3D12_BARRIER_SUBRESOURCE_RANGE`. A count of `u32::MAX` means
/// "all remaining".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubresourceRange {
    pub first_mip: u32,
    pub mip_count: u32,
    pub first_slice: u32,
    pub slice_count: u32,
    pub first_plane: u32,
    pub plane_count: u32,
}

impl SubresourceRange {
    pub const ALL: SubresourceRange = SubresourceRange {
        first_mip: 0,
        mip_count: u32::MAX,
        first_slice: 0,
        slice_count: u32::MAX,
        first_plane: 0,
        plane_count: u32::MAX,
    };

    /// `count` mips starting at `first`, all slices and planes.
    pub const fn mips(first: u32, count: u32) -> Self {
        SubresourceRange {
            first_mip: first,
            mip_count: count,
            ..Self::ALL
        }
    }

    /// `count` array slices starting at `first`, all mips and planes.
    pub const fn slices(first: u32, count: u32) -> Self {
        SubresourceRange {
            first_slice: first,
            slice_count: count,
            ..Self::ALL
        }
    }

    /// Exactly one subresource.
    pub const fn single(mip: u32, slice: u32, plane: u32) -> Self {
        SubresourceRange {
            first_mip: mip,
            mip_count: 1,
            first_slice: slice,
            slice_count: 1,
            first_plane: plane,
            plane_count: 1,
        }
    }

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

impl fmt::Display for SubresourceRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mips {}+{} slices {}+{} planes {}+{}",
            self.first_mip,
            self.mip_count,
            self.first_slice,
            self.slice_count,
            self.first_plane,
            self.plane_count
        )
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

/// Buffer declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferDesc {
    pub name: String,
    pub size: u64,
}

/// Texture declaration. `initial_layout` is the layout the subresources are
/// in when the graph starts (`Undefined` for transient textures whose
/// contents are discarded). If `final_layout` is set, the plan ends with
/// every subresource in that layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureDesc {
    pub name: String,
    pub mip_levels: u32,
    pub array_size: u32,
    pub plane_count: u32,
    pub initial_layout: Layout,
    pub final_layout: Option<Layout>,
}

impl TextureDesc {
    /// One mip, one slice, one plane.
    pub fn simple(name: impl Into<String>, initial_layout: Layout) -> Self {
        TextureDesc {
            name: name.into(),
            mip_levels: 1,
            array_size: 1,
            plane_count: 1,
            initial_layout,
            final_layout: None,
        }
    }

    pub fn with_mips(mut self, mip_levels: u32) -> Self {
        self.mip_levels = mip_levels;
        self
    }

    pub fn with_array_size(mut self, array_size: u32) -> Self {
        self.array_size = array_size;
        self
    }

    pub fn with_final_layout(mut self, layout: Layout) -> Self {
        self.final_layout = Some(layout);
        self
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

#[derive(Clone, Debug)]
enum UseDescRange {
    Bytes(ByteRange),
    Subresources(SubresourceRange),
}

#[derive(Clone, Debug)]
struct UseDesc {
    resource: ResourceRef,
    range: UseDescRange,
    access: Access,
    sync: Sync,
    layout: Option<Layout>,
}

/// A pass to add to a graph: a name, the queue it executes on, and the
/// resource ranges it accesses.
#[derive(Clone, Debug)]
pub struct PassDesc {
    name: String,
    queue: QueueId,
    uses: Vec<UseDesc>,
}

impl PassDesc {
    pub fn new(name: impl Into<String>, queue: QueueId) -> Self {
        PassDesc {
            name: name.into(),
            queue,
            uses: Vec::new(),
        }
    }

    /// Declares an access to a byte range of a buffer.
    pub fn buffer(
        mut self,
        buffer: BufferHandle,
        range: ByteRange,
        access: Access,
        sync: Sync,
    ) -> Self {
        self.uses.push(UseDesc {
            resource: buffer.into(),
            range: UseDescRange::Bytes(range),
            access,
            sync,
            layout: None,
        });
        self
    }

    /// Declares an access to texture subresources. With `layout == None` the
    /// planner keeps the current layout when it admits the access and
    /// otherwise chooses a layout for it.
    pub fn texture(
        mut self,
        texture: TextureHandle,
        range: SubresourceRange,
        access: Access,
        sync: Sync,
        layout: impl Into<Option<Layout>>,
    ) -> Self {
        self.uses.push(UseDesc {
            resource: texture.into(),
            range: UseDescRange::Subresources(range),
            access,
            sync,
            layout: layout.into(),
        });
        self
    }
}

/// Why a queue rejected a use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueRestriction {
    Access(Access),
    Sync(Sync),
    Layout(Layout),
}

/// Structured errors raised while declaring a graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// A handle from a different graph was used.
    ForeignHandle,
    InvalidResource {
        name: String,
        reason: &'static str,
    },
    /// `access` is empty (`COMMON`), `NO_ACCESS`, or has unknown bits.
    InvalidAccess {
        pass: String,
        use_index: usize,
        access: Access,
    },
    /// `sync` is `NONE`, `SPLIT`, or has unknown bits for the queue.
    InvalidSync {
        pass: String,
        use_index: usize,
        sync: Sync,
    },
    /// No supplied sync scope is compatible with these access bits.
    AccessSyncMismatch {
        pass: String,
        use_index: usize,
        access: Access,
        sync: Sync,
    },
    /// The explicit layout does not admit the access.
    AccessLayoutMismatch {
        pass: String,
        use_index: usize,
        access: Access,
        layout: Layout,
    },
    /// Access only meaningful for the other resource kind (e.g. a
    /// render-target access on a buffer).
    AccessResourceKindMismatch {
        pass: String,
        use_index: usize,
        access: Access,
    },
    /// The pass's queue type cannot perform this use.
    QueueUnsupported {
        pass: String,
        use_index: usize,
        queue: QueueKind,
        restriction: QueueRestriction,
    },
    /// Empty or out-of-bounds range.
    InvalidRange {
        pass: String,
        use_index: usize,
    },
    /// Two uses in the same pass overlap and either one writes or their
    /// explicit layouts differ.
    ConflictingUses {
        pass: String,
        first_use: usize,
        second_use: usize,
    },
    /// A pass was ordered after itself.
    SelfDependency {
        pass: PassId,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::ForeignHandle => write!(f, "handle belongs to a different render graph"),
            GraphError::InvalidResource { name, reason } => {
                write!(f, "resource `{name}` is invalid: {reason}")
            }
            GraphError::InvalidAccess {
                pass,
                use_index,
                access,
            } => {
                write!(f, "pass `{pass}` use {use_index}: invalid access {access}")
            }
            GraphError::InvalidSync {
                pass,
                use_index,
                sync,
            } => {
                write!(f, "pass `{pass}` use {use_index}: invalid sync {sync}")
            }
            GraphError::AccessSyncMismatch {
                pass,
                use_index,
                access,
                sync,
            } => write!(
                f,
                "pass `{pass}` use {use_index}: access {access} is not compatible with sync {sync}"
            ),
            GraphError::AccessLayoutMismatch {
                pass,
                use_index,
                access,
                layout,
            } => write!(
                f,
                "pass `{pass}` use {use_index}: access {access} is not compatible with layout {layout}"
            ),
            GraphError::AccessResourceKindMismatch {
                pass,
                use_index,
                access,
            } => write!(
                f,
                "pass `{pass}` use {use_index}: access {access} does not apply to this resource kind"
            ),
            GraphError::QueueUnsupported {
                pass,
                use_index,
                queue,
                restriction,
            } => write!(
                f,
                "pass `{pass}` use {use_index}: {queue} queue does not support {restriction:?}"
            ),
            GraphError::InvalidRange { pass, use_index } => {
                write!(
                    f,
                    "pass `{pass}` use {use_index}: empty or out-of-bounds range"
                )
            }
            GraphError::ConflictingUses {
                pass,
                first_use,
                second_use,
            } => write!(
                f,
                "pass `{pass}`: uses {first_use} and {second_use} overlap with a write or differing layouts"
            ),
            GraphError::SelfDependency { pass } => {
                write!(f, "pass {} cannot depend on itself", pass.index)
            }
        }
    }
}

impl std::error::Error for GraphError {}

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
            id: NEXT_GRAPH_ID.fetch_add(1, Ordering::Relaxed),
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
        if count > 1 << 20 {
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
