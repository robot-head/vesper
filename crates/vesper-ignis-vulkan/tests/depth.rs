//! Depth/stencil textures and color formats beyond RGBA8, on a real Vulkan
//! implementation (Mesa lavapipe in CI), with validation required there.
//! Rendered images are compared with committed goldens.

mod common;

use ash::vk;
use common::*;
use vesper_ignis::{
    Access, BufferDesc, ByteRange, Layout, PassDesc, QueueKind, RenderGraph, SubresourceRange,
    Sync, TextureDesc,
};
use vesper_ignis_vulkan::golden::Image;
use vesper_ignis_vulkan::*;

const W: u32 = 64;
const H: u32 = 64;

fn buffer(g: &mut RenderGraph, name: &str, size: u64) -> vesper_ignis::BufferHandle {
    g.add_buffer(BufferDesc {
        name: name.into(),
        size,
    })
    .unwrap()
}

fn copy_out(
    g: &mut RenderGraph,
    q: vesper_ignis::QueueId,
    name: &str,
    tex: vesper_ignis::TextureHandle,
    range: SubresourceRange,
    buf: vesper_ignis::BufferHandle,
) -> vesper_ignis::PassId {
    g.add_pass(
        PassDesc::new(name, q)
            .texture(
                tex,
                range,
                Access::COPY_SOURCE,
                Sync::COPY,
                Layout::CopySource,
            )
            .buffer(buf, ByteRange::WHOLE, Access::COPY_DEST, Sync::COPY),
    )
    .unwrap()
}

#[test]
fn depth_and_stencil_render_match_golden_and_read_back() {
    let Some(device) = device() else { return };
    let format = Format::Depth32FloatStencil8;
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let color = g
        .add_texture(TextureDesc::simple("color", Layout::Undefined))
        .unwrap();
    let mut ds_desc = TextureDesc::simple("depth", Layout::Undefined);
    ds_desc.plane_count = 2;
    let ds = g.add_texture(ds_desc).unwrap();
    let color_out = buffer(&mut g, "color-out", u64::from(W * H * 4));
    let depth_out = buffer(&mut g, "depth-out", u64::from(W * H * 4));
    let stencil_out = buffer(&mut g, "stencil-out", u64::from(W * H));
    let draw = g
        .add_pass(
            PassDesc::new("draw", q)
                .texture(
                    color,
                    SubresourceRange::ALL,
                    Access::RENDER_TARGET,
                    Sync::RENDER_TARGET,
                    Layout::RenderTarget,
                )
                .texture(
                    ds,
                    SubresourceRange::ALL,
                    Access::DEPTH_STENCIL_WRITE,
                    Sync::DEPTH_STENCIL,
                    Layout::DepthStencilWrite,
                ),
        )
        .unwrap();
    let copy_color = copy_out(
        &mut g,
        q,
        "copy-color",
        color,
        SubresourceRange::ALL,
        color_out,
    );
    let copy_depth = copy_out(
        &mut g,
        q,
        "copy-depth",
        ds,
        SubresourceRange::single(0, 0, 0),
        depth_out,
    );
    let copy_stencil = copy_out(
        &mut g,
        q,
        "copy-stencil",
        ds,
        SubresourceRange::single(0, 0, 1),
        stencil_out,
    );

    let vs = compile_wgsl(shaders::DEPTH, Stage::Vertex, "vs_main").unwrap();
    let fs = compile_wgsl(shaders::DEPTH, Stage::Fragment, "fs_main").unwrap();
    let pipeline = device
        .create_graphics_pipeline_with(
            &vs,
            &fs,
            &GraphicsDesc {
                color: Some(Format::Rgba8Unorm),
                depth: Some(format),
                depth_test: Some(vk::CompareOp::LESS),
                depth_write: true,
                stencil: Some(StencilDesc {
                    compare: vk::CompareOp::ALWAYS,
                    pass: vk::StencilOp::REPLACE,
                }),
            },
        )
        .unwrap();
    let ds_tex = match device.create_texture(TextureInfo::new(W, H).with_format(format)) {
        Ok(t) => t,
        Err(VulkanError::UnsupportedFormat(f)) => {
            eprintln!("skipped: {f:?} unsupported on this device");
            pipeline.destroy();
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let mut r = FrameResources::new();
    r.bind_texture(
        color,
        device.create_texture(TextureInfo::new(W, H)).unwrap(),
    )
    .unwrap();
    r.bind_texture(ds, ds_tex).unwrap();
    for b in [color_out, depth_out, stencil_out] {
        let size = if b == stencil_out { W * H } else { W * H * 4 };
        r.bind_buffer(b, device.create_buffer(u64::from(size)).unwrap())
            .unwrap();
    }
    let pipeline = r.add_pipeline(pipeline).unwrap();
    r.set_pass(
        draw,
        PassWork::Render {
            pipeline,
            color: Some(ColorTarget {
                texture: color,
                clear: Some([0.0, 0.0, 0.0, 1.0]),
            }),
            depth: Some(DepthTarget {
                texture: ds,
                clear_depth: Some(1.0),
                clear_stencil: Some(0),
            }),
            vertices: 3,
            instances: 2,
            stencil_reference: 7,
        },
    )
    .unwrap();
    r.set_pass(
        copy_color,
        PassWork::CopyTextureToBuffer {
            src: color,
            dst: color_out,
        },
    )
    .unwrap();
    for (pass, plane, dst) in [(copy_depth, 0, depth_out), (copy_stencil, 1, stencil_out)] {
        r.set_pass(
            pass,
            PassWork::CopyPlaneToBuffer {
                src: ds,
                plane,
                mip: 0,
                dst,
                offset: 0,
            },
        )
        .unwrap();
    }
    let rt = runtime(&device, &[QueueKind::Direct]);
    let b = batch(&rt, &device, &g, &[(q, QueueKind::Direct)], r);
    let (rt, allocators, r) = run_frame(rt, b);

    let rgba = r.buffer(color_out).unwrap().read();
    let image = Image::new(W, H, rgba).unwrap();
    check_golden(
        "depth_stencil",
        include_bytes!("goldens/depth_stencil.pam"),
        &image,
    );
    let depth: Vec<f32> = r
        .buffer(depth_out)
        .unwrap()
        .read()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    let stencil = r.buffer(stencil_out).unwrap().read();
    let at = |x: u32, y: u32| (y * W + x) as usize;
    // Overlap (center): the near triangle wins the depth test.
    assert_eq!(image.pixel(W / 2, H / 2), Some([0, 255, 0, 255]));
    assert_eq!(depth[at(W / 2, H / 2)], 0.25);
    // Right side: only the far triangle.
    assert_eq!(image.pixel(W / 2 + 22, H / 2 - 18), Some([255, 0, 0, 255]));
    assert_eq!(depth[at(W / 2 + 22, H / 2 - 18)], 0.75);
    // Corner: cleared depth and stencil.
    assert_eq!(depth[at(0, H - 1)], 1.0);
    assert_eq!(stencil[at(0, H - 1)], 0);
    // Every covered pixel has stencil 7 and every uncovered one 0.
    for y in 0..H {
        for x in 0..W {
            let covered = depth[at(x, y)] < 1.0;
            assert_eq!(stencil[at(x, y)] == 7, covered, "stencil at ({x}, {y})");
        }
    }
    let tex = r.texture(ds).unwrap();
    assert_eq!(
        tex.plane_layout(1, 0, 0),
        Some(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
    );
    finish(rt, allocators, r);
    assert_clean(&device);
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1f;
    let frac = f32::from(h & 0x3ff);
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        e => (1.0 + frac / 1024.0) * 2f32.powi(i32::from(e) - 15),
    }
}

#[test]
fn triangle_renders_into_float_and_bgra_targets() {
    let Some(device) = device() else { return };
    let vs = compile_wgsl(shaders::TRIANGLE, Stage::Vertex, "vs_main").unwrap();
    let fs = compile_wgsl(shaders::TRIANGLE, Stage::Fragment, "fs_main").unwrap();
    for format in [Format::Rgba16Float, Format::Bgra8Unorm, Format::Rgba32Float] {
        let texel = format.copy_texel_bytes(0).unwrap();
        let mut g = RenderGraph::new();
        let q = g.add_queue(QueueKind::Direct);
        let color = g
            .add_texture(TextureDesc::simple("color", Layout::Undefined))
            .unwrap();
        let out = buffer(&mut g, "out", u64::from(W * H * texel));
        let draw = g
            .add_pass(PassDesc::new("draw", q).texture(
                color,
                SubresourceRange::ALL,
                Access::RENDER_TARGET,
                Sync::RENDER_TARGET,
                Layout::RenderTarget,
            ))
            .unwrap();
        let copy = copy_out(&mut g, q, "copy", color, SubresourceRange::ALL, out);
        let pipeline = device
            .create_graphics_pipeline_with(
                &vs,
                &fs,
                &GraphicsDesc {
                    color: Some(format),
                    ..GraphicsDesc::default()
                },
            )
            .unwrap();
        let mut r = FrameResources::new();
        r.bind_texture(
            color,
            device
                .create_texture(TextureInfo::new(W, H).with_format(format))
                .unwrap(),
        )
        .unwrap();
        r.bind_buffer(out, device.create_buffer(u64::from(W * H * texel)).unwrap())
            .unwrap();
        let pipeline = r.add_pipeline(pipeline).unwrap();
        r.set_pass(
            draw,
            PassWork::Render {
                pipeline,
                color: Some(ColorTarget {
                    texture: color,
                    clear: Some([0.0, 0.0, 1.0, 1.0]),
                }),
                depth: None,
                vertices: 3,
                instances: 1,
                stencil_reference: 0,
            },
        )
        .unwrap();
        r.set_pass(
            copy,
            PassWork::CopyPlaneToBuffer {
                src: color,
                plane: 0,
                mip: 0,
                dst: out,
                offset: 0,
            },
        )
        .unwrap();
        let rt = runtime(&device, &[QueueKind::Direct]);
        let b = batch(&rt, &device, &g, &[(q, QueueKind::Direct)], r);
        let (rt, allocators, r) = run_frame(rt, b);
        let bytes = r.buffer(out).unwrap().read();
        let center = ((H / 2 * W + W / 2) * texel) as usize;
        let corner = ((W + 1) * texel) as usize;
        let rgba = |at: usize| -> [f32; 4] {
            let px = &bytes[at..at + texel as usize];
            match format {
                Format::Rgba16Float => std::array::from_fn(|i| {
                    f16_to_f32(u16::from_le_bytes([px[2 * i], px[2 * i + 1]]))
                }),
                Format::Rgba32Float => std::array::from_fn(|i| {
                    f32::from_le_bytes(px[4 * i..4 * i + 4].try_into().unwrap())
                }),
                Format::Bgra8Unorm => [px[2], px[1], px[0], px[3]].map(|c| f32::from(c) / 255.0),
                _ => unreachable!(),
            }
        };
        let tolerance = if format == Format::Bgra8Unorm {
            0.003
        } else {
            1e-6
        };
        for (got, want) in rgba(center).iter().zip([1.0, 0.25, 0.0, 1.0]) {
            assert!(
                (got - want).abs() <= tolerance,
                "{format:?} center {got} vs {want}"
            );
        }
        assert_eq!(rgba(corner), [0.0, 0.0, 1.0, 1.0], "{format:?} clear");
        finish(rt, allocators, r);
    }
    assert_clean(&device);
}

#[test]
fn every_format_is_supported_or_a_typed_error() {
    let Some(device) = device() else { return };
    let mut supported = Vec::new();
    for format in Format::ALL {
        match device.create_texture(TextureInfo::new(16, 16).with_format(format)) {
            Ok(t) => {
                let usage = t.usage();
                assert!(usage.contains(vk::ImageUsageFlags::TRANSFER_SRC));
                if format.is_color() {
                    assert!(usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT));
                } else {
                    assert!(usage.contains(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT));
                }
                supported.push(format);
                t.destroy();
            }
            Err(VulkanError::UnsupportedFormat(f)) => assert_eq!(f, format),
            Err(e) => panic!("{format:?}: {e}"),
        }
    }
    eprintln!("supported formats: {supported:?}");
    assert!(supported.contains(&Format::Rgba8Unorm));
    assert!(supported.contains(&Format::Depth32Float));
    // Mismatched pipeline state is rejected before any Vulkan call.
    let vs = compile_wgsl(shaders::DEPTH, Stage::Vertex, "vs_main").unwrap();
    let fs = compile_wgsl(shaders::DEPTH, Stage::Fragment, "fs_main").unwrap();
    let bad = device.create_graphics_pipeline_with(
        &vs,
        &fs,
        &GraphicsDesc {
            depth: Some(Format::Depth32Float),
            stencil: Some(StencilDesc {
                compare: vk::CompareOp::ALWAYS,
                pass: vk::StencilOp::REPLACE,
            }),
            ..GraphicsDesc::default()
        },
    );
    assert!(matches!(bad, Err(VulkanError::InvalidDesc(_))), "{bad:?}");
    assert_clean(&device);
}
