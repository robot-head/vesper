//! Sync fds exported for timeline points and polled from a Pulse epoll
//! driver (§19.9), natively (`SYNC_FD`) and through the bridged fallback.
//!
//! The native path needs `VK_KHR_external_semaphore_fd` with `SYNC_FD`
//! export, which lavapipe offers only when `/dev/udmabuf` exists. Without it
//! the native test is skipped, unless `VESPER_IGNIS_VULKAN_REQUIRE_SYNC_FD`
//! is set (as in CI), when it fails.
#![cfg(target_os = "linux")]

mod common;

use common::*;
use std::time::{Duration, Instant};
use vesper_ignis::lifecycle::{Limits, QueueConfig, Runtime, SubmitResult};
use vesper_ignis::{Access, BufferDesc, ByteRange, PassDesc, QueueKind, RenderGraph, Sync};
use vesper_ignis_vulkan::pulse::SyncFdWaiter;
use vesper_ignis_vulkan::*;

/// Polls without blocking until `n` tokens arrive or `limit` passes.
fn collect(waiter: &mut SyncFdWaiter, n: usize, limit: Duration) -> Vec<u64> {
    let start = Instant::now();
    let mut got = Vec::new();
    while got.len() < n && start.elapsed() < limit {
        got.extend(waiter.poll(false).unwrap());
        std::thread::sleep(Duration::from_millis(2));
    }
    got
}

fn fill_frame(
    rt: &Rt,
    device: &Device,
    word: u32,
) -> (
    vesper_ignis::lifecycle::BatchInputs<CommandAllocator, FrameResources>,
    RenderGraph,
) {
    let mut g = RenderGraph::new();
    let q = g.add_queue(QueueKind::Direct);
    let buf = g
        .add_buffer(BufferDesc {
            name: "b".into(),
            size: 256,
        })
        .unwrap();
    let pass = g
        .add_pass(PassDesc::new("fill", q).buffer(
            buf,
            ByteRange::WHOLE,
            Access::COPY_DEST,
            Sync::COPY,
        ))
        .unwrap();
    let mut r = FrameResources::new();
    r.bind_buffer(buf, device.create_buffer(256).unwrap())
        .unwrap();
    r.set_pass(
        pass,
        PassWork::FillBuffer {
            dst: buf,
            value: word,
        },
    )
    .unwrap();
    let b = batch(rt, device, &g, &[(q, QueueKind::Direct)], r);
    (b, g)
}

fn run(mode: SyncFdMode) {
    let Some(device) = device() else { return };
    let backend = VulkanBackend::new(&device, &[QueueKind::Direct]).unwrap();
    let exporter = backend.sync_fd_exporter();
    if mode == SyncFdMode::Native && !exporter.native_available() {
        assert!(
            std::env::var_os("VESPER_IGNIS_VULKAN_REQUIRE_SYNC_FD").is_none(),
            "VESPER_IGNIS_VULKAN_REQUIRE_SYNC_FD is set but SYNC_FD export is unavailable"
        );
        eprintln!("skipped: SYNC_FD export unavailable (no /dev/udmabuf for lavapipe?)");
        backend.destroy().map_err(|(_, e)| e).unwrap();
        return;
    }
    let mut rt: Rt = Runtime::new(
        backend,
        vec![QueueConfig::new(QueueKind::Direct)],
        Limits::default(),
    )
    .unwrap_or_else(|e| panic!("{:?}", e.error));
    let mut waiter = SyncFdWaiter::new(8).unwrap();
    let mut owners = Vec::new();

    // Frame 1: export after submission and wait for it through Pulse.
    let (b, _g) = fill_frame(&rt, &device, 0x0505_0505);
    let recording = rt.record(b).unwrap_or_else(|e| panic!("{:?}", e.error));
    let SubmitResult::Submitted(t1) = rt.submit(recording.close()) else {
        panic!("frame 1 not submitted")
    };
    let v1 = rt.submission(t1).unwrap().terminal[0].value;
    let fd1 = exporter.export(0, v1, mode).unwrap();
    // Frame 2's point, exported before frame 2 exists (wait-before-signal).
    // Natively that would wait on the queue ahead of its own signal, so
    // Native refuses and Auto bridges it.
    if mode == SyncFdMode::Native {
        assert!(matches!(
            exporter.export(0, v1 + 1, mode),
            Err(VulkanError::Unsupported(_))
        ));
    }
    let fd2 = exporter
        .export(
            0,
            v1 + 1,
            if mode == SyncFdMode::Native {
                SyncFdMode::Auto
            } else {
                mode
            },
        )
        .unwrap();
    eprintln!("kinds: {:?} {:?}", fd1.kind(), fd2.kind());
    assert_eq!(fd2.kind(), SyncFdKind::Bridged);
    if mode == SyncFdMode::Native {
        assert_ne!(fd1.kind(), SyncFdKind::Bridged);
    }
    waiter.watch(&fd1, 1).unwrap();
    waiter.watch(&fd2, 2).unwrap();
    assert_eq!(collect(&mut waiter, 1, Duration::from_secs(30)), vec![1]);
    // Frame 1 is done but frame 2 was never submitted: fd 2 stays quiet.
    assert!(collect(&mut waiter, 1, Duration::from_millis(200)).is_empty());
    assert_eq!(waiter.pending(), 1);

    loop {
        let mut report = rt.poll(16);
        assert!(report.error.is_none());
        if let Some(f) = report.retired.pop() {
            owners.push(f);
            break;
        }
    }
    let (b, _g2) = fill_frame(&rt, &device, 0x0606_0606);
    let recording = rt.record(b).unwrap_or_else(|e| panic!("{:?}", e.error));
    let SubmitResult::Submitted(t2) = rt.submit(recording.close()) else {
        panic!("frame 2 not submitted")
    };
    assert!(rt.submission(t2).unwrap().terminal[0].value > v1);
    // Blocking poll: Pulse returns once the GPU reaches frame 2's point.
    assert_eq!(waiter.poll(true).unwrap(), vec![2]);
    assert_eq!(waiter.pending(), 0);

    // An open export keeps the backend alive; closing both releases it.
    fd1.close(5_000_000_000).unwrap();
    fd2.close(5_000_000_000).unwrap();
    assert!(matches!(
        exporter.export(0, 0, mode),
        Err(VulkanError::InvalidDesc(_))
    ));
    assert!(matches!(
        exporter.export(9, 1, mode),
        Err(VulkanError::Limit(_))
    ));
    loop {
        let mut report = rt.poll(16);
        assert!(report.error.is_none());
        if let Some(f) = report.retired.pop() {
            owners.push(f);
            break;
        }
    }
    finish(rt, Vec::new(), FrameResources::new());
    // The backend is gone: exports are refused.
    assert!(matches!(
        exporter.export(0, 1, mode),
        Err(VulkanError::Unavailable(_))
    ));
    for f in owners {
        for a in f.allocators {
            a.into_inner().destroy();
        }
        f.resources.destroy();
    }
    drop(exporter);
    assert_clean(&device);
}

#[test]
fn native_sync_fds_are_polled_from_pulse() {
    run(SyncFdMode::Native);
}

#[test]
fn bridged_sync_fds_are_polled_from_pulse() {
    run(SyncFdMode::Bridged);
}

#[test]
fn an_open_sync_fd_keeps_the_backend_from_being_destroyed() {
    let Some(device) = device() else { return };
    let backend = VulkanBackend::new(&device, &[QueueKind::Direct]).unwrap();
    let exporter = backend.sync_fd_exporter();
    let fd = exporter.export(0, 1, SyncFdMode::Bridged).unwrap();
    let (backend, e) = backend.destroy().unwrap_err();
    assert!(matches!(e, VulkanError::Pending));
    fd.close(1_000_000_000).unwrap();
    backend.destroy().map_err(|(_, e)| e).unwrap();
    assert_clean(&device);
}
