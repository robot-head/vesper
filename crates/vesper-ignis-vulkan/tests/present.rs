//! Swapchain presentation to an X11 window under Xvfb, read back from the X
//! server and compared with the rendered texture and a committed golden.
//!
//! The test uses `$DISPLAY` when set and otherwise starts its own `Xvfb`.
//! Without an X server or `VK_KHR_xcb_surface` it is skipped, unless
//! `VESPER_IGNIS_VULKAN_REQUIRE_PRESENT` is set (as in CI), when it fails.
#![cfg(target_os = "linux")]

mod common;

use common::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use vesper_ignis::{
    Access, BufferDesc, ByteRange, Layout, PassDesc, QueueKind, RenderGraph, SubresourceRange,
    Sync, TextureDesc,
};
use vesper_ignis_vulkan::golden::{Image, diff};
use vesper_ignis_vulkan::*;

const W: u32 = 64;
const H: u32 = 64;

fn required() -> bool {
    std::env::var_os("VESPER_IGNIS_VULKAN_REQUIRE_PRESENT").is_some()
}

fn skip(why: &str) {
    assert!(
        !required(),
        "VESPER_IGNIS_VULKAN_REQUIRE_PRESENT is set but: {why}"
    );
    eprintln!("skipped: {why}");
}

/// An Xvfb server this test started; killed on drop.
struct Xvfb(Child);

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The display to use, and the server when this test started one.
fn display() -> Option<(String, Option<Xvfb>)> {
    if let Ok(d) = std::env::var("DISPLAY")
        && !d.is_empty()
    {
        return Some((d, None));
    }
    // `-displayfd 1` makes Xvfb pick a free display and print its number
    // once it accepts connections.
    let mut child = Command::new("Xvfb")
        .args([
            "-displayfd",
            "1",
            "-screen",
            "0",
            "128x128x24",
            "-nolisten",
            "tcp",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let server = Xvfb(child);
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).ok()?;
    let n: u32 = line.trim().parse().ok()?;
    Some((format!(":{n}"), Some(server)))
}

#[test]
fn rendered_triangle_is_presented_to_an_x11_window() {
    let Some(device) = device() else { return };
    if !device.info().presentation {
        return skip("VK_KHR_xcb_surface / VK_KHR_swapchain unavailable");
    }
    let Some((display, _server)) = display() else {
        return skip("no X server ($DISPLAY unset and Xvfb did not start)");
    };
    let window = match XcbWindow::open(Some(&display), W, H) {
        Ok(w) => w,
        Err(e) => return skip(&format!("cannot open a window on {display}: {e}")),
    };
    assert_eq!(window.extent(), (W, H));
    let mut swapchain = Swapchain::new(&device, &window, SwapchainDesc::default()).unwrap();
    assert_eq!(
        (swapchain.extent().width, swapchain.extent().height),
        (W, H)
    );
    assert!(swapchain.image_count() >= 2);

    // Render the triangle into a texture and read it back through the graph.
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let color = g
        .add_texture(TextureDesc::simple("color", Layout::Undefined))
        .unwrap();
    let out = g
        .add_buffer(BufferDesc {
            name: "out".into(),
            size: u64::from(W * H * 4),
        })
        .unwrap();
    let draw = g
        .add_pass(PassDesc::new("draw", q).texture(
            color,
            SubresourceRange::ALL,
            Access::RENDER_TARGET,
            Sync::RENDER_TARGET,
            Layout::RenderTarget,
        ))
        .unwrap();
    let copy = g
        .add_pass(
            PassDesc::new("copy", q)
                .texture(
                    color,
                    SubresourceRange::ALL,
                    Access::COPY_SOURCE,
                    Sync::COPY,
                    Layout::CopySource,
                )
                .buffer(out, ByteRange::WHOLE, Access::COPY_DEST, Sync::COPY),
        )
        .unwrap();
    let vs = compile_wgsl(shaders::TRIANGLE, Stage::Vertex, "vs_main").unwrap();
    let fs = compile_wgsl(shaders::TRIANGLE, Stage::Fragment, "fs_main").unwrap();
    let mut r = FrameResources::new();
    r.bind_texture(
        color,
        device.create_texture(TextureInfo::new(W, H)).unwrap(),
    )
    .unwrap();
    r.bind_buffer(out, device.create_buffer(u64::from(W * H * 4)).unwrap())
        .unwrap();
    let pipeline = r
        .add_pipeline(device.create_graphics_pipeline(&vs, &fs).unwrap())
        .unwrap();
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
        PassWork::CopyTextureToBuffer {
            src: color,
            dst: out,
        },
    )
    .unwrap();
    let rt = runtime(&device, &[QueueKind::Direct]);
    let b = batch(&rt, &device, &g, &[(q, QueueKind::Direct)], r);
    let (rt, allocators, r) = run_frame(rt, b);
    let rendered = Image::new(W, H, r.buffer(out).unwrap().read()).unwrap();

    // Present it a few times so the swapchain cycles through its images.
    let tex = r.texture(color).unwrap();
    let mut indices = Vec::new();
    for _ in 0..swapchain.image_count() + 1 {
        indices.push(swapchain.present_texture(tex).unwrap());
    }
    assert_eq!(swapchain.presents(), indices.len() as u64);
    indices.sort_unstable();
    indices.dedup();
    assert!(
        indices.len() >= 2,
        "one swapchain image reused: {indices:?}"
    );

    // What the X server shows is what was rendered. The WSI may still be
    // handing the last image to the server, so poll briefly.
    let start = std::time::Instant::now();
    let (shown, d) = loop {
        let shown = window.read_pixels().unwrap();
        let d = diff(&shown, &rendered, 0).unwrap();
        if d.differing == 0 || start.elapsed() > std::time::Duration::from_secs(5) {
            break (shown, d);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(d.differing, 0, "window differs from the texture: {d:?}");
    check_golden(
        "present_triangle",
        include_bytes!("goldens/present_triangle.pam"),
        &shown,
    );
    assert_eq!(shown.pixel(0, 0), Some([0, 0, 255, 255]));
    assert_ne!(shown.pixel(W / 2, H / 2), Some([0, 0, 255, 255]));

    // An integer texture cannot be blitted into the normalized swapchain.
    let int_tex = device
        .create_texture(TextureInfo::new(W, H).with_format(Format::R32Uint))
        .unwrap();
    assert!(matches!(
        swapchain.present_texture(&int_tex),
        Err(VulkanError::Unsupported(_))
    ));
    int_tex.destroy();

    swapchain.destroy();
    window.destroy();
    finish(rt, allocators, r);
    assert_clean(&device);
    assert_eq!(leaked_owners(), 0);
}

#[test]
fn a_missing_display_is_a_typed_error() {
    match XcbWindow::open(Some(":4093"), W, H) {
        Err(VulkanError::Unavailable(_)) => {}
        Err(e) => panic!("unexpected error: {e}"),
        Ok(w) => {
            w.destroy();
            panic!("display :4093 exists");
        }
    }
    assert!(matches!(
        XcbWindow::open(None, 0, H),
        Err(VulkanError::InvalidDesc(_) | VulkanError::Unavailable(_))
    ));
}
