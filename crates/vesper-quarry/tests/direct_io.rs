//! `O_DIRECT` data-file I/O for `FsDevice`, with its stated fallback.
use vesper_quarry::device::{Device, DirectIo, DirectMode, FileId, FsDevice, DIRECT_ALIGN};
use vesper_quarry::store::{Store, StoreLimits};
use vesper_quarry::Value;

fn dir(tag: &str) -> std::path::PathBuf {
    // Prefer a directory on the build disk (often not tmpfs) so the direct
    // path is exercised where the filesystem supports it.
    let base = std::env::var_os("VESPER_DIRECT_IO_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!(
        "vesper-quarry-direct-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn prefer_reports_what_it_does_and_round_trips() {
    let d = dir("prefer");
    let mut dev = FsDevice::open_with(&d, DirectMode::Prefer).unwrap();
    let status = dev.direct_io().clone();
    eprintln!("direct I/O status at {}: {status:?}", d.display());
    match &status {
        DirectIo::Active => {}
        DirectIo::Unavailable(why) => assert!(!why.is_empty()),
        DirectIo::Off => panic!("Prefer never reports Off"),
    }
    assert!(!d.join("quarry.direct-probe").exists(), "probe removed");
    // Aligned (direct when active), unaligned (always buffered) and
    // cross-handle reads agree.
    let page: Vec<u8> = (0..2 * DIRECT_ALIGN).map(|i| (i % 251) as u8).collect();
    dev.write_at(FileId::Data, DIRECT_ALIGN as u64, &page)
        .unwrap();
    dev.write_at(FileId::Data, 10, b"head").unwrap();
    let mut back = vec![0u8; page.len()];
    dev.read_at(FileId::Data, DIRECT_ALIGN as u64, &mut back)
        .unwrap();
    assert_eq!(back, page);
    let mut odd = [0u8; 7];
    dev.read_at(FileId::Data, DIRECT_ALIGN as u64 + 3, &mut odd)
        .unwrap();
    assert_eq!(&odd[..], &page[3..10]);
    let mut head = [0u8; DIRECT_ALIGN];
    dev.read_at(FileId::Data, 0, &mut head).unwrap();
    assert_eq!(&head[10..14], b"head");
    let mut past = [0u8; DIRECT_ALIGN];
    assert_eq!(
        dev.read_at(FileId::Data, 64 * DIRECT_ALIGN as u64, &mut past)
            .unwrap_err(),
        vesper_quarry::device::DeviceError::ShortRead
    );
    dev.sync(FileId::Data).unwrap();
    drop(dev);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn store_runs_and_recovers_over_direct_io() {
    let d = dir("store");
    {
        let dev = FsDevice::open_with(&d, DirectMode::Prefer).unwrap();
        let (mut s, _) = Store::open(dev, StoreLimits::default()).unwrap();
        s.execute_ddl("CREATE TABLE t (k I64 NOT NULL, v TEXT)")
            .unwrap();
        let mut t = s.begin().unwrap();
        for k in 0..300 {
            s.insert(&mut t, "t", &[Value::I64(k), Value::Text(format!("v{k}"))])
                .unwrap();
        }
        s.commit(t).unwrap();
        s.checkpoint().unwrap();
        // Crash: no close.
        drop(s.into_device());
    }
    let dev = FsDevice::open_with(&d, DirectMode::Prefer).unwrap();
    let (mut s, _) = Store::open(dev, StoreLimits::default()).unwrap();
    let t = s.begin().unwrap();
    assert_eq!(s.scan(&t, "t").unwrap().len(), 300);
    s.commit(t).unwrap();
    s.check().unwrap();
    drop(s);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn require_fails_with_a_reason_or_is_active() {
    let d = dir("require");
    match FsDevice::open_with(&d, DirectMode::Require) {
        Ok(dev) => assert_eq!(dev.direct_io(), &DirectIo::Active),
        Err(e) => assert!(e.to_string().contains("O_DIRECT"), "{e}"),
    }
    let off = FsDevice::open_with(&d.join("off"), DirectMode::Off).unwrap();
    assert_eq!(off.direct_io(), &DirectIo::Off);
    drop(off);
    let _ = std::fs::remove_dir_all(&d);
}
