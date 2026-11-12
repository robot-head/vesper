//! Typed models of the D3D12 enhanced-barrier vocabulary.
//!
//! The bit values mirror `D3D12_BARRIER_SYNC`, `D3D12_BARRIER_ACCESS`, and
//! `D3D12_BARRIER_LAYOUT` from `d3d12.h`, so a later Windows executor can pass
//! [`Sync::bits`], [`Access::bits`], and [`Layout::raw`] straight through. They
//! have not yet been compared against the headers on a Windows host; that is
//! part of the Windows acceptance gate.
//!
//! The compatibility tables follow the "Layout Access Compatibility",
//! "Access Bits Barrier Sync Compatibility", and queue-type sections of the
//! D3D12 Enhanced Barriers specification
//! (<https://microsoft.github.io/DirectX-Specs/d3d/D3D12EnhancedBarriers.html>).
//! Video queues, video layouts/accesses, simultaneous-access textures, and
//! `DIRECT_QUEUE_GENERIC_READ_COMPUTE_QUEUE_ACCESSIBLE` are not modeled.

use std::fmt;
use std::ops::{BitOr, BitOrAssign};

macro_rules! bit_set {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$cmeta:meta])* $konst:ident = $value:expr),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
        pub struct $name(u32);

        impl $name {
            $($(#[$cmeta])* pub const $konst: Self = Self($value);)*

            const NAMED: &'static [(&'static str, u32)] = &[$((stringify!($konst), $value)),*];

            /// Raw D3D12 bit pattern.
            pub const fn bits(self) -> u32 {
                self.0
            }

            /// Builds a set from raw bits. Unknown bits are kept, so the caller
            /// must validate values that did not come from these constants.
            pub const fn from_bits(bits: u32) -> Self {
                Self(bits)
            }

            /// True when every bit of `other` is also set in `self`.
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            /// True when the two sets share at least one bit.
            pub const fn intersects(self, other: Self) -> bool {
                self.0 & other.0 != 0
            }
        }

        impl BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }

        impl BitOrAssign for $name {
            fn bitor_assign(&mut self, rhs: Self) {
                self.0 |= rhs.0;
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, f)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // Exact match first, so zero-valued and aliased names print.
                if let Some((name, _)) = Self::NAMED.iter().find(|(_, v)| *v == self.0) {
                    return f.write_str(name);
                }
                let mut rest = self.0;
                let mut first = true;
                for (name, value) in Self::NAMED {
                    if *value != 0 && value.count_ones() == 1 && rest & value != 0 {
                        if !first {
                            f.write_str("|")?;
                        }
                        f.write_str(name)?;
                        first = false;
                        rest &= !value;
                    }
                }
                if rest != 0 {
                    if !first {
                        f.write_str("|")?;
                    }
                    write!(f, "{rest:#x}")?;
                }
                Ok(())
            }
        }
    };
}

bit_set! {
    /// `D3D12_BARRIER_SYNC`: pipeline scopes a barrier waits for or blocks.
    Sync {
        NONE = 0,
        ALL = 0x1,
        DRAW = 0x2,
        INDEX_INPUT = 0x4,
        VERTEX_SHADING = 0x8,
        PIXEL_SHADING = 0x10,
        DEPTH_STENCIL = 0x20,
        RENDER_TARGET = 0x40,
        COMPUTE_SHADING = 0x80,
        RAYTRACING = 0x100,
        COPY = 0x200,
        RESOLVE = 0x400,
        EXECUTE_INDIRECT = 0x800,
        ALL_SHADING = 0x1000,
        NON_PIXEL_SHADING = 0x2000,
        EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO = 0x4000,
        CLEAR_UNORDERED_ACCESS_VIEW = 0x8000,
        BUILD_RAYTRACING_ACCELERATION_STRUCTURE = 0x80_0000,
        COPY_RAYTRACING_ACCELERATION_STRUCTURE = 0x100_0000,
        /// Marks the begin (`SyncAfter`) or end (`SyncBefore`) of a split barrier.
        SPLIT = 0x8000_0000,
    }
}

bit_set! {
    /// `D3D12_BARRIER_ACCESS`: cache/visibility scopes of an access.
    Access {
        COMMON = 0,
        VERTEX_BUFFER = 0x1,
        CONSTANT_BUFFER = 0x2,
        INDEX_BUFFER = 0x4,
        RENDER_TARGET = 0x8,
        UNORDERED_ACCESS = 0x10,
        DEPTH_STENCIL_WRITE = 0x20,
        DEPTH_STENCIL_READ = 0x40,
        SHADER_RESOURCE = 0x80,
        STREAM_OUTPUT = 0x100,
        /// Same bit as `D3D12_BARRIER_ACCESS_PREDICATION`.
        INDIRECT_ARGUMENT = 0x200,
        COPY_DEST = 0x400,
        COPY_SOURCE = 0x800,
        RESOLVE_DEST = 0x1000,
        RESOLVE_SOURCE = 0x2000,
        RAYTRACING_ACCELERATION_STRUCTURE_READ = 0x4000,
        RAYTRACING_ACCELERATION_STRUCTURE_WRITE = 0x8000,
        SHADING_RATE_SOURCE = 0x1_0000,
        NO_ACCESS = 0x8000_0000,
    }
}

const ALL_SYNC_SUPERSEDES: u32 =
    0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400 | 0x800 | 0x8000;

impl Sync {
    /// Expands aggregate scopes (`ALL`, `DRAW`, `ALL_SHADING`,
    /// `NON_PIXEL_SHADING`) to the individual scopes the specification says
    /// they supersede. `ALL` is not treated as covering the raytracing
    /// acceleration-structure build/copy/postbuild scopes, which the
    /// specification's `ALL` table does not list; this is conservative.
    pub fn expanded(self) -> Sync {
        let mut bits = self.0 & !Self::SPLIT.0;
        if bits & Self::ALL.0 != 0 {
            bits |= ALL_SYNC_SUPERSEDES | Self::ALL_SHADING.0 | Self::NON_PIXEL_SHADING.0;
        }
        if bits & Self::DRAW.0 != 0 {
            bits |= Self::INDEX_INPUT.0
                | Self::VERTEX_SHADING.0
                | Self::PIXEL_SHADING.0
                | Self::DEPTH_STENCIL.0
                | Self::RENDER_TARGET.0;
        }
        if bits & Self::ALL_SHADING.0 != 0 {
            bits |= Self::VERTEX_SHADING.0 | Self::PIXEL_SHADING.0 | Self::COMPUTE_SHADING.0;
        }
        if bits & Self::NON_PIXEL_SHADING.0 != 0 {
            bits |= Self::VERTEX_SHADING.0 | Self::COMPUTE_SHADING.0;
        }
        Sync(bits)
    }

    /// True when the scopes in `self` include every scope in `other` after
    /// aggregate expansion. Aggregate bits in `other` are only covered by
    /// the same aggregate (or `ALL`), never by the union of their parts.
    pub fn covers(self, other: Sync) -> bool {
        let have = self.expanded();
        let need = other.expanded();
        have.contains(need)
    }

    fn union_nonempty(self, other: Sync) -> Sync {
        match (self == Sync::NONE, other == Sync::NONE) {
            (true, _) => other,
            (_, true) => self,
            _ => self | other,
        }
    }
}

const WRITE_ACCESS: u32 = 0x8 | 0x10 | 0x20 | 0x100 | 0x400 | 0x1000 | 0x8000;
const BUFFER_ONLY_ACCESS: u32 = 0x1 | 0x2 | 0x4 | 0x100 | 0x200 | 0x4000 | 0x8000;
const TEXTURE_ONLY_ACCESS: u32 = 0x8 | 0x20 | 0x40 | 0x1000 | 0x2000 | 0x1_0000;
const KNOWN_ACCESS: u32 = 0x1_ffff;

impl Access {
    /// True when any bit names a write access. `UNORDERED_ACCESS` is treated
    /// as a write because the API cannot tell a UAV read from a UAV write.
    pub fn is_write(self) -> bool {
        self != Self::NO_ACCESS && self.0 & WRITE_ACCESS != 0
    }

    pub(crate) fn union_nonempty(self, other: Access) -> Access {
        match (self == Access::NO_ACCESS, other == Access::NO_ACCESS) {
            (true, _) => other,
            (_, true) => self,
            _ => self | other,
        }
    }

    pub(crate) fn unknown_bits(self) -> u32 {
        self.0 & !KNOWN_ACCESS
    }

    pub(crate) fn buffer_only(self) -> bool {
        self.0 & BUFFER_ONLY_ACCESS != 0
    }

    pub(crate) fn texture_only(self) -> Access {
        Access(self.0 & TEXTURE_ONLY_ACCESS)
    }

    /// Bits of `self` for which none of the supplied sync scopes is listed as
    /// compatible ("at least one of the listed sync bits must also be
    /// provided").
    pub fn incompatible_with_sync(self, sync: Sync) -> Access {
        let mut bad = 0;
        let mut rest = self.0;
        while rest != 0 {
            let bit = rest.isolate_lowest_one();
            rest &= !bit;
            if sync.0 & compatible_sync(bit) == 0 {
                bad |= bit;
            }
        }
        Access(bad)
    }
}

fn compatible_sync(access_bit: u32) -> u32 {
    let s = |x: Sync| x.0;
    let all = s(Sync::ALL);
    let shading = s(Sync::VERTEX_SHADING)
        | s(Sync::PIXEL_SHADING)
        | s(Sync::COMPUTE_SHADING)
        | s(Sync::DRAW)
        | s(Sync::ALL_SHADING);
    let rtas = s(Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE)
        | s(Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE);
    all | match access_bit {
        0x1 => s(Sync::VERTEX_SHADING) | s(Sync::DRAW) | s(Sync::ALL_SHADING),
        0x2 => shading,
        0x4 => s(Sync::INDEX_INPUT) | s(Sync::DRAW),
        0x8 => s(Sync::DRAW) | s(Sync::RENDER_TARGET),
        0x10 => {
            shading
                | s(Sync::CLEAR_UNORDERED_ACCESS_VIEW)
                | s(Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO)
                | s(Sync::RAYTRACING)
        }
        0x20 | 0x40 => s(Sync::DRAW) | s(Sync::DEPTH_STENCIL),
        0x80 => shading | s(Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE) | s(Sync::RAYTRACING),
        0x100 => s(Sync::VERTEX_SHADING) | s(Sync::DRAW) | s(Sync::ALL_SHADING),
        0x200 => s(Sync::EXECUTE_INDIRECT),
        0x400 | 0x800 => s(Sync::COPY),
        0x1000 | 0x2000 => s(Sync::RESOLVE),
        0x4000 => {
            s(Sync::COMPUTE_SHADING)
                | s(Sync::RAYTRACING)
                | s(Sync::ALL_SHADING)
                | rtas
                | s(Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO)
        }
        0x8000 => s(Sync::COMPUTE_SHADING) | s(Sync::RAYTRACING) | s(Sync::ALL_SHADING) | rtas,
        0x1_0000 => s(Sync::PIXEL_SHADING) | s(Sync::ALL_SHADING),
        _ => 0,
    }
}

/// `D3D12_BARRIER_LAYOUT` for textures. Buffers have no layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layout {
    /// Contents are undefined. Only valid as an initial layout here; the
    /// planner never emits `LayoutAfter = UNDEFINED`.
    Undefined,
    /// `COMMON`, which has the same value as `PRESENT`.
    Common,
    GenericRead,
    RenderTarget,
    UnorderedAccess,
    DepthStencilWrite,
    DepthStencilRead,
    ShaderResource,
    CopySource,
    CopyDest,
    ResolveSource,
    ResolveDest,
    ShadingRateSource,
    DirectQueueCommon,
    DirectQueueGenericRead,
    DirectQueueUnorderedAccess,
    DirectQueueShaderResource,
    DirectQueueCopySource,
    DirectQueueCopyDest,
    ComputeQueueCommon,
    ComputeQueueGenericRead,
    ComputeQueueUnorderedAccess,
    ComputeQueueShaderResource,
    ComputeQueueCopySource,
    ComputeQueueCopyDest,
}

impl Layout {
    /// `D3D12_BARRIER_LAYOUT_PRESENT` is an alias of `COMMON`.
    pub const PRESENT: Layout = Layout::Common;

    /// Raw `D3D12_BARRIER_LAYOUT` value.
    pub const fn raw(self) -> u32 {
        use Layout::*;
        match self {
            Undefined => 0xffff_ffff,
            Common => 0,
            GenericRead => 1,
            RenderTarget => 2,
            UnorderedAccess => 3,
            DepthStencilWrite => 4,
            DepthStencilRead => 5,
            ShaderResource => 6,
            CopySource => 7,
            CopyDest => 8,
            ResolveSource => 9,
            ResolveDest => 10,
            ShadingRateSource => 11,
            DirectQueueCommon => 18,
            DirectQueueGenericRead => 19,
            DirectQueueUnorderedAccess => 20,
            DirectQueueShaderResource => 21,
            DirectQueueCopySource => 22,
            DirectQueueCopyDest => 23,
            ComputeQueueCommon => 24,
            ComputeQueueGenericRead => 25,
            ComputeQueueUnorderedAccess => 26,
            ComputeQueueShaderResource => 27,
            ComputeQueueCopySource => 28,
            ComputeQueueCopyDest => 29,
        }
    }

    /// Accesses the specification lists as compatible with this layout.
    /// `UNDEFINED` admits only `NO_ACCESS` (the both-undefined case is not
    /// used by this planner).
    pub fn compatible_access(self) -> Access {
        use Layout::*;
        let a = |x: Access| x.0;
        let srv = a(Access::SHADER_RESOURCE);
        let src = a(Access::COPY_SOURCE);
        let dst = a(Access::COPY_DEST);
        let uav = a(Access::UNORDERED_ACCESS);
        Access(match self {
            Undefined => 0,
            Common => srv | src | dst,
            DirectQueueCommon | ComputeQueueCommon => srv | src | dst | uav,
            GenericRead | ComputeQueueGenericRead => srv | src,
            DirectQueueGenericRead => {
                srv | src
                    | a(Access::DEPTH_STENCIL_READ)
                    | a(Access::SHADING_RATE_SOURCE)
                    | a(Access::RESOLVE_SOURCE)
            }
            RenderTarget => a(Access::RENDER_TARGET),
            UnorderedAccess | DirectQueueUnorderedAccess | ComputeQueueUnorderedAccess => uav,
            DepthStencilWrite => a(Access::DEPTH_STENCIL_READ) | a(Access::DEPTH_STENCIL_WRITE),
            DepthStencilRead => a(Access::DEPTH_STENCIL_READ),
            ShaderResource | DirectQueueShaderResource | ComputeQueueShaderResource => srv,
            CopySource | DirectQueueCopySource | ComputeQueueCopySource => src,
            CopyDest | DirectQueueCopyDest | ComputeQueueCopyDest => dst,
            ResolveSource => a(Access::RESOLVE_SOURCE),
            ResolveDest => a(Access::RESOLVE_DEST),
            ShadingRateSource => a(Access::SHADING_RATE_SOURCE),
        })
    }

    /// True when every bit of `access` may be used in this layout.
    pub fn allows(self, access: Access) -> bool {
        access != Access::NO_ACCESS && self.compatible_access().contains(access)
    }

    /// Name used by the textual plan dump.
    pub fn name(self) -> &'static str {
        use Layout::*;
        match self {
            Undefined => "UNDEFINED",
            Common => "COMMON",
            GenericRead => "GENERIC_READ",
            RenderTarget => "RENDER_TARGET",
            UnorderedAccess => "UNORDERED_ACCESS",
            DepthStencilWrite => "DEPTH_STENCIL_WRITE",
            DepthStencilRead => "DEPTH_STENCIL_READ",
            ShaderResource => "SHADER_RESOURCE",
            CopySource => "COPY_SOURCE",
            CopyDest => "COPY_DEST",
            ResolveSource => "RESOLVE_SOURCE",
            ResolveDest => "RESOLVE_DEST",
            ShadingRateSource => "SHADING_RATE_SOURCE",
            DirectQueueCommon => "DIRECT_QUEUE_COMMON",
            DirectQueueGenericRead => "DIRECT_QUEUE_GENERIC_READ",
            DirectQueueUnorderedAccess => "DIRECT_QUEUE_UNORDERED_ACCESS",
            DirectQueueShaderResource => "DIRECT_QUEUE_SHADER_RESOURCE",
            DirectQueueCopySource => "DIRECT_QUEUE_COPY_SOURCE",
            DirectQueueCopyDest => "DIRECT_QUEUE_COPY_DEST",
            ComputeQueueCommon => "COMPUTE_QUEUE_COMMON",
            ComputeQueueGenericRead => "COMPUTE_QUEUE_GENERIC_READ",
            ComputeQueueUnorderedAccess => "COMPUTE_QUEUE_UNORDERED_ACCESS",
            ComputeQueueShaderResource => "COMPUTE_QUEUE_SHADER_RESOURCE",
            ComputeQueueCopySource => "COMPUTE_QUEUE_COPY_SOURCE",
            ComputeQueueCopyDest => "COMPUTE_QUEUE_COPY_DEST",
        }
    }

    /// Layouts tried, in order, when a texture use leaves its layout to the
    /// planner and the current layout does not admit the access.
    pub(crate) const AUTO_CANDIDATES: [Layout; 12] = [
        Layout::ShaderResource,
        Layout::CopySource,
        Layout::CopyDest,
        Layout::UnorderedAccess,
        Layout::RenderTarget,
        Layout::DepthStencilRead,
        Layout::DepthStencilWrite,
        Layout::ResolveSource,
        Layout::ResolveDest,
        Layout::ShadingRateSource,
        Layout::GenericRead,
        Layout::DirectQueueGenericRead,
    ];
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Command queue type. Each queue owns one timeline fence in the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum QueueKind {
    Direct,
    Compute,
    Copy,
}

impl QueueKind {
    pub fn name(self) -> &'static str {
        match self {
            QueueKind::Direct => "direct",
            QueueKind::Compute => "compute",
            QueueKind::Copy => "copy",
        }
    }

    /// Layouts a subresource may be in while used on this queue type.
    pub fn supports_layout(self, layout: Layout) -> bool {
        use Layout::*;
        match self {
            QueueKind::Direct => !matches!(
                layout,
                Undefined
                    | ComputeQueueCommon
                    | ComputeQueueGenericRead
                    | ComputeQueueUnorderedAccess
                    | ComputeQueueShaderResource
                    | ComputeQueueCopySource
                    | ComputeQueueCopyDest
            ),
            QueueKind::Compute => matches!(
                layout,
                Common
                    | GenericRead
                    | UnorderedAccess
                    | ShaderResource
                    | CopySource
                    | CopyDest
                    | ComputeQueueCommon
                    | ComputeQueueGenericRead
                    | ComputeQueueUnorderedAccess
                    | ComputeQueueShaderResource
                    | ComputeQueueCopySource
                    | ComputeQueueCopyDest
            ),
            QueueKind::Copy => layout == Common,
        }
    }

    /// Whether a barrier on this queue type may transition `before -> after`.
    /// "Both LayoutBefore and LayoutAfter must be compatible with the type of
    /// queue performing the Barrier"; `UNDEFINED` is accepted as a before
    /// layout. Copy queues do not support layout transitions at all.
    pub fn can_transition(self, before: Layout, after: Layout) -> bool {
        self != QueueKind::Copy
            && after != Layout::Undefined
            && (before == Layout::Undefined || self.supports_layout(before))
            && self.supports_layout(after)
    }

    /// Access bits this planner accepts on the queue type. This is a
    /// conservative model of which operations each command-list type records.
    pub fn supported_access(self) -> Access {
        match self {
            QueueKind::Direct => Access(KNOWN_ACCESS),
            QueueKind::Compute => {
                Access::CONSTANT_BUFFER
                    | Access::UNORDERED_ACCESS
                    | Access::SHADER_RESOURCE
                    | Access::INDIRECT_ARGUMENT
                    | Access::COPY_DEST
                    | Access::COPY_SOURCE
                    | Access::RAYTRACING_ACCELERATION_STRUCTURE_READ
                    | Access::RAYTRACING_ACCELERATION_STRUCTURE_WRITE
            }
            QueueKind::Copy => Access::COPY_DEST | Access::COPY_SOURCE,
        }
    }

    /// Sync scopes this planner accepts on the queue type.
    pub fn supported_sync(self) -> Sync {
        match self {
            QueueKind::Direct => Sync(0xffff | 0x0180_0000),
            QueueKind::Compute => {
                Sync::ALL
                    | Sync::COMPUTE_SHADING
                    | Sync::ALL_SHADING
                    | Sync::NON_PIXEL_SHADING
                    | Sync::RAYTRACING
                    | Sync::COPY
                    | Sync::EXECUTE_INDIRECT
                    | Sync::CLEAR_UNORDERED_ACCESS_VIEW
                    | Sync::EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO
                    | Sync::BUILD_RAYTRACING_ACCELERATION_STRUCTURE
                    | Sync::COPY_RAYTRACING_ACCELERATION_STRUCTURE
            }
            QueueKind::Copy => Sync::ALL | Sync::COPY,
        }
    }
}

impl fmt::Display for QueueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

pub(crate) fn sync_union(a: Sync, b: Sync) -> Sync {
    a.union_nonempty(b)
}
