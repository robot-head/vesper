//! §13.4 isolation domains. On hosts with PKU (x86_64) or POE (AArch64) these
//! tests exercise real hardware faults in contained child processes; on other
//! hosts they assert that every constructor reports `Unavailable`.
use std::time::Duration;
use vesper_ffi::isolation::*;

const T: Duration = Duration::from_secs(60);

/// Forked children copy only the forking thread; serialize the tests so no
/// other test thread is mid-panic (holding std's panic locks) at a fork.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn available() -> Option<Mechanism> {
    match probe() {
        Ok(m) => {
            note(&format!("isolation exercised: {m:?}"));
            Some(m)
        }
        Err(u) => {
            note(&format!("isolation unavailable on this host: {u}"));
            None
        }
    }
}

#[test]
fn unavailable_hosts_report_a_reason_and_never_pretend() {
    let _serial = serial();
    if available().is_some() {
        return;
    }
    let u = IsolationDomain::new().unwrap_err();
    assert!(!u.detail.is_empty());
    assert!(matches!(
        contain(T, |_| ()),
        Err(DomainError::Unavailable(_))
    ));
    if !cfg!(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )) {
        assert_eq!(u.reason, UnavailableReason::UnsupportedTarget);
    }
}

#[test]
fn rights_are_revoked_inside_and_restored_after_every_exit() {
    let _serial = serial();
    let Some(mechanism) = available() else { return };
    let mut d = IsolationDomain::new().unwrap();
    assert_eq!(d.mechanism(), mechanism);
    assert!(d.key() > 0, "key 0 is the default key");
    let r = d.add_region(100).unwrap();
    assert_eq!(d.region_len(r).unwrap(), 100);
    assert_eq!(d.region_address(r).unwrap() % 4096, 0);
    let full = Rights {
        read: true,
        write: true,
    };
    assert_eq!(d.rights(), full);
    d.write(r, 10, b"vesper").unwrap();
    let inside = d.with_revoked(Revoke::Write, || {
        // Reads stay allowed while writes are revoked.
        let mut b = [0u8; 6];
        d.read(r, 10, &mut b).unwrap();
        (d.rights(), b)
    });
    assert_eq!(
        inside,
        (
            Rights {
                read: true,
                write: false
            },
            *b"vesper"
        )
    );
    assert_eq!(d.rights(), full);
    let nested = d.with_revoked(Revoke::Write, || {
        let deeper = d.with_revoked(Revoke::Access, || d.rights());
        (deeper, d.rights())
    });
    assert_eq!(
        nested,
        (
            Rights {
                read: false,
                write: false
            },
            Rights {
                read: true,
                write: false
            }
        )
    );
    // A panic unwinding out of the closure still restores the register.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        d.with_revoked(Revoke::Access, || panic!("legacy code panicked"))
    }));
    assert!(caught.is_err());
    assert_eq!(d.rights(), full);
    d.write(r, 0, &[1]).unwrap();
}

#[test]
fn a_write_into_a_write_protected_region_faults_in_a_contained_child() {
    let _serial = serial();
    let Some(_) = available() else { return };
    let mut d = IsolationDomain::new().unwrap();
    let r = d.add_region(4096).unwrap();
    let base = d.region_address(r).unwrap() as u64;
    let out = contain(T, |tx| {
        tx.send(1);
        d.with_revoked(Revoke::Write, || {
            let mut b = [0u8; 1];
            d.read(r, 0, &mut b).unwrap();
            tx.send(2);
            // A stray write, as foreign code would make: faults in hardware.
            d.write(r, 64, &[0xAA]).unwrap();
            tx.send(3);
        });
    })
    .unwrap();
    assert_eq!(out.messages, vec![1, 2], "no progress after the fault");
    match out.exit {
        ChildExit::Fault {
            signal,
            address,
            protection_key,
            ..
        } => {
            assert_eq!(signal, 11, "SIGSEGV");
            assert!(protection_key, "kernel reported SEGV_PKUERR");
            assert_eq!(address, base + 64);
        }
        other => panic!("expected a protection-key fault, got {other:?}"),
    }
    // The parent is untouched and keeps full rights.
    d.write(r, 64, &[0xBB]).unwrap();
    let mut b = [0u8; 1];
    d.read(r, 64, &mut b).unwrap();
    assert_eq!(b, [0xBB]);
}

#[test]
fn a_read_with_access_revoked_faults_and_unprotected_work_returns() {
    let _serial = serial();
    let Some(_) = available() else { return };
    let mut d = IsolationDomain::new().unwrap();
    let r = d.add_region(8).unwrap();
    let out = contain(T, |_| {
        d.with_revoked(Revoke::Access, || {
            let mut b = [0u8; 1];
            let _ = d.read(r, 0, &mut b);
        })
    })
    .unwrap();
    assert!(
        matches!(
            out.exit,
            ChildExit::Fault {
                protection_key: true,
                ..
            }
        ),
        "{out:?}"
    );
    let ok = contain(T, |tx| {
        d.write(r, 0, &[5]).unwrap();
        tx.send(7);
    })
    .unwrap();
    assert_eq!(ok.exit, ChildExit::Returned);
    assert_eq!(ok.messages, vec![7]);
    // The child's write was to its own copy-on-write pages.
    let mut b = [9u8; 1];
    d.read(r, 0, &mut b).unwrap();
    assert_eq!(b, [0]);
    let panicked = contain(T, |_| panic!("boom")).unwrap();
    assert_eq!(panicked.exit, ChildExit::Panicked);
    let slow = contain(Duration::from_millis(200), |tx| {
        tx.send(1);
        loop {
            std::hint::spin_loop();
        }
    })
    .unwrap();
    assert_eq!(slow.exit, ChildExit::TimedOut);
    assert_eq!(slow.messages, vec![1]);
}

#[test]
fn other_domains_and_bounds_are_checked() {
    let _serial = serial();
    let Some(_) = available() else { return };
    let mut a = IsolationDomain::new().unwrap();
    let mut b = IsolationDomain::new().unwrap();
    assert_ne!(a.key(), b.key());
    let ra = a.add_region(16).unwrap();
    let rb = b.add_region(16).unwrap();
    // Revoking `a` leaves `b` writable.
    a.with_revoked(Revoke::Access, || b.write(rb, 0, &[1, 2, 3]).unwrap());
    assert_eq!(
        a.write(ra, 10, &[0; 7]),
        Err(DomainError::OutOfBounds {
            offset: 10,
            len: 7,
            region: 16
        })
    );
    assert!(matches!(
        a.read(ra, usize::MAX, &mut [0; 2]),
        Err(DomainError::OutOfBounds { .. })
    ));
    assert_eq!(
        a.add_region(0),
        Err(DomainError::RegionLength { requested: 0 })
    );
    assert_eq!(
        a.add_region(MAX_REGION_BYTES + 1),
        Err(DomainError::RegionLength {
            requested: MAX_REGION_BYTES + 1
        })
    );
    // `rb` has index 0 like `ra`, but belongs to `b`.
    assert_eq!(a.region_len(rb), Err(DomainError::UnknownRegion));
}

/// Written straight to the stderr handle, which the test harness does not
/// capture, so CI logs record whether hardware isolation was exercised.
fn note(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}
