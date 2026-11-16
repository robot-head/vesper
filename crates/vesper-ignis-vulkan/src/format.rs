//! Texture formats supported by the backend (no `unsafe`).
//!
//! Every format names its Vulkan format, its D3D12-style plane layout (color,
//! depth, or depth + stencil, where D3D12 exposes depth as plane 0 and
//! stencil as plane 1) and the tightly packed byte size of one texel of each
//! plane when copied to a buffer. Whether the device can use a format for a
//! given purpose is checked at texture creation
//! ([`crate::Device::create_texture`]); unsupported combinations are typed
//! errors, never silently replaced by another format.

use crate::translate::Planes;
use ash::vk;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Format {
    Rgba8Unorm,
    Bgra8Unorm,
    Rgba8Srgb,
    Rgba16Float,
    Rgba32Float,
    R32Float,
    R32Uint,
    Depth16Unorm,
    Depth32Float,
    Depth24UnormStencil8,
    Depth32FloatStencil8,
}

impl Format {
    pub const ALL: [Format; 11] = [
        Format::Rgba8Unorm,
        Format::Bgra8Unorm,
        Format::Rgba8Srgb,
        Format::Rgba16Float,
        Format::Rgba32Float,
        Format::R32Float,
        Format::R32Uint,
        Format::Depth16Unorm,
        Format::Depth32Float,
        Format::Depth24UnormStencil8,
        Format::Depth32FloatStencil8,
    ];

    pub fn vk(self) -> vk::Format {
        match self {
            Format::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
            Format::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
            Format::Rgba8Srgb => vk::Format::R8G8B8A8_SRGB,
            Format::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
            Format::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
            Format::R32Float => vk::Format::R32_SFLOAT,
            Format::R32Uint => vk::Format::R32_UINT,
            Format::Depth16Unorm => vk::Format::D16_UNORM,
            Format::Depth32Float => vk::Format::D32_SFLOAT,
            Format::Depth24UnormStencil8 => vk::Format::D24_UNORM_S8_UINT,
            Format::Depth32FloatStencil8 => vk::Format::D32_SFLOAT_S8_UINT,
        }
    }

    /// The format with this Vulkan value, if it is one of ours.
    pub fn from_vk(format: vk::Format) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.vk() == format)
    }

    pub fn planes(self) -> Planes {
        match self {
            Format::Depth16Unorm | Format::Depth32Float => Planes::Depth,
            Format::Depth24UnormStencil8 | Format::Depth32FloatStencil8 => Planes::DepthStencil,
            _ => Planes::Color,
        }
    }

    pub fn is_color(self) -> bool {
        self.planes() == Planes::Color
    }
    pub fn has_depth(self) -> bool {
        !self.is_color()
    }
    pub fn has_stencil(self) -> bool {
        self.planes() == Planes::DepthStencil
    }

    /// Number of D3D12 planes (1 for color and depth-only, 2 for depth +
    /// stencil).
    pub fn plane_count(self) -> u32 {
        self.planes().count()
    }

    /// The Vulkan aspect of `plane`, or `None` when the format has no such
    /// plane.
    pub fn aspect(self, plane: u32) -> Option<vk::ImageAspectFlags> {
        (plane < self.plane_count()).then(|| self.planes().aspect(plane))
    }

    /// Every aspect of the format.
    pub fn aspects(self) -> vk::ImageAspectFlags {
        match self.planes() {
            Planes::Color => vk::ImageAspectFlags::COLOR,
            Planes::Depth => vk::ImageAspectFlags::DEPTH,
            Planes::DepthStencil => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
        }
    }

    /// Bytes per texel of `plane` in a tightly packed buffer copy
    /// (`vkCmdCopyImageToBuffer` rules: the depth plane of D24S8 copies as
    /// 4 bytes, the stencil plane as 1). `None` for a plane the format lacks.
    pub fn copy_texel_bytes(self, plane: u32) -> Option<u32> {
        if plane >= self.plane_count() {
            return None;
        }
        Some(match (self, plane) {
            (Format::Rgba8Unorm | Format::Bgra8Unorm | Format::Rgba8Srgb, _) => 4,
            (Format::Rgba16Float, _) => 8,
            (Format::Rgba32Float, _) => 16,
            (Format::R32Float | Format::R32Uint, _) => 4,
            (Format::Depth16Unorm, _) => 2,
            (Format::Depth32Float, _) => 4,
            (Format::Depth24UnormStencil8 | Format::Depth32FloatStencil8, 0) => 4,
            (_, _) => 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_aspects_and_copy_sizes() {
        for f in Format::ALL {
            assert_eq!(Format::from_vk(f.vk()), Some(f));
            assert_eq!(f.copy_texel_bytes(f.plane_count()), None);
            assert_eq!(f.aspect(f.plane_count()), None);
        }
        assert_eq!(
            Format::Depth32FloatStencil8.aspect(1),
            Some(vk::ImageAspectFlags::STENCIL)
        );
        assert_eq!(Format::Depth24UnormStencil8.copy_texel_bytes(0), Some(4));
        assert_eq!(Format::Depth24UnormStencil8.copy_texel_bytes(1), Some(1));
        assert_eq!(Format::Rgba16Float.copy_texel_bytes(0), Some(8));
        assert!(Format::Bgra8Unorm.is_color());
        assert!(!Format::Depth32Float.has_stencil());
        assert_eq!(
            Format::Depth32FloatStencil8.aspects(),
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        );
    }
}
