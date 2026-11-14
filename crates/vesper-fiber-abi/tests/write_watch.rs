//! Kernel-assisted write tracking: raw-pointer stores into a tracked region
//! are recorded per page by the MMU (mprotect faults on Linux and macOS,
//! write watches on Windows).
#![cfg(any(target_os = "linux", target_os = "macos", windows))]
use vesper_fiber_abi::write_watch::{Mechanism, TrackedRegion, page_size};

#[test]
fn raw_pointer_writes_are_tracked_per_page() {
    let page = page_size();
    let mut region = TrackedRegion::new(8 * page - 1).unwrap();
    assert_eq!(region.len(), 8 * page);
    assert_eq!(region.pages(), 8);
    assert_eq!(region.mechanism(), Mechanism::HOST);
    region.arm().unwrap();
    let base = region.as_mut_ptr();
    // SAFETY: in bounds of the live region, nothing else borrows it.
    unsafe {
        base.add(page + 17).write_volatile(0xa5);
        base.add(5 * page)
            .cast::<u64>()
            .write_volatile(0x1122_3344_5566_7788);
        // A second store to the same page is recorded once.
        base.add(5 * page + 100).write_volatile(1);
    }
    assert_eq!(region.take_dirty().unwrap(), vec![1, 5]);
    // The stores really happened.
    let mut b = [0u8; 8];
    region.read(5 * page, &mut b).unwrap();
    assert_eq!(u64::from_le_bytes(b), 0x1122_3344_5566_7788);
    region.read(page + 17, &mut b[..1]).unwrap();
    assert_eq!(b[0], 0xa5);
    if region.mechanism() == Mechanism::MprotectFaults {
        assert_eq!(region.serviced_faults(), 2, "one fault per page per epoch");
    }

    // The next epoch starts empty; reads are not writes.
    region.read(0, &mut b).unwrap();
    assert!(region.take_dirty().unwrap().is_empty());
    // A page written in the previous epoch is tracked again.
    // SAFETY: as above.
    unsafe {
        base.add(5 * page + 8).write_volatile(2);
        base.add(7 * page + page - 1).write_volatile(3);
    }
    region.write(0, &[9; 3]).unwrap();
    assert_eq!(region.take_dirty().unwrap(), vec![0, 5, 7]);
    let stats = region.stats();
    assert_eq!(stats.dirty_pages, 5);

    // Disarmed: writes are allowed and not recorded.
    assert!(region.disarm().unwrap().is_empty());
    assert!(!region.is_armed());
    // SAFETY: as above.
    unsafe { base.add(3 * page).write_volatile(4) };
    region.arm().unwrap();
    assert!(region.take_dirty().unwrap().is_empty());
}

#[test]
fn a_write_spanning_pages_dirties_each_and_bounds_are_checked() {
    let page = page_size();
    let mut region = TrackedRegion::new(4 * page).unwrap();
    region.arm().unwrap();
    let data = vec![7u8; page + 2];
    region.write(page - 1, &data).unwrap();
    assert_eq!(region.take_dirty().unwrap(), vec![0, 1, 2]);
    assert!(region.write(4 * page - 1, &[1, 2]).is_err());
    assert!(region.read(usize::MAX, &mut [0]).is_err());
    assert!(TrackedRegion::new(0).is_err());
    assert!(region.take_dirty().unwrap().is_empty());
}

#[test]
fn several_regions_track_independently_and_slots_are_reused() {
    let page = page_size();
    let mut a = TrackedRegion::new(2 * page).unwrap();
    let mut b = TrackedRegion::new(2 * page).unwrap();
    a.arm().unwrap();
    b.arm().unwrap();
    // SAFETY: in bounds of live regions.
    unsafe {
        a.as_mut_ptr().add(page).write_volatile(1);
        b.as_mut_ptr().write_volatile(1);
    }
    assert_eq!(a.take_dirty().unwrap(), vec![1]);
    assert_eq!(b.take_dirty().unwrap(), vec![0]);
    drop(a);
    drop(b);
    // Far more regions than handler slots over time, never at once.
    for i in 0..200 {
        let mut r = TrackedRegion::new(page).unwrap();
        r.arm().unwrap();
        // SAFETY: in bounds.
        unsafe { r.as_mut_ptr().write_volatile(i as u8) };
        assert_eq!(r.take_dirty().unwrap(), vec![0]);
    }
}

/// Faults outside every tracked region still reach the previous handler: a
/// write to read-only memory that is not tracked kills the process with the
/// default action (checked in a child process).
#[cfg(unix)]
#[test]
fn foreign_faults_keep_their_default_action() {
    if std::env::var_os("VESPER_WRITE_WATCH_CHILD").is_some() {
        let page = page_size();
        let mut tracked = TrackedRegion::new(page).unwrap();
        tracked.arm().unwrap();
        // SAFETY: deliberately faulting on a read-only, untracked mapping.
        unsafe {
            let p = libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            assert_ne!(p, libc::MAP_FAILED);
            p.cast::<u8>().write_volatile(1);
        }
        std::process::exit(0);
    }
    let exe = std::env::current_exe().unwrap();
    let mut cmd = match std::env::var("VESPER_PROBE_RUNNER") {
        Ok(runner) if !runner.is_empty() => {
            let mut parts = runner.split_whitespace();
            let mut c = std::process::Command::new(parts.next().unwrap());
            c.args(parts).arg(exe);
            c
        }
        _ => std::process::Command::new(exe),
    };
    let status = cmd
        .args([
            "--exact",
            "foreign_faults_keep_their_default_action",
            "--test-threads=1",
        ])
        .env("VESPER_WRITE_WATCH_CHILD", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    use std::os::unix::process::ExitStatusExt;
    let signal = status.signal();
    assert!(
        matches!(signal, Some(libc::SIGSEGV) | Some(libc::SIGBUS)),
        "child should die of the fault, got {status:?}"
    );
}
