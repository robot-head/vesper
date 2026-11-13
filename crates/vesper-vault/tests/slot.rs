//! SecretSlot: page alignment, projections, shred, leak detection and the
//! honest protection report.

use vesper_vault::{LeakAuditor, LockPolicy, ObligationKind, Protection, SecretSlot, SlotError};

#[test]
fn slot_is_page_aligned_projected_and_shredded() {
    let mut src = [0xA5u8; 32];
    let mut slot = SecretSlot::<32>::from_bytes(LockPolicy::BestEffort, &mut src).unwrap();
    assert_eq!(src, [0u8; 32], "source zeroized after the move");
    let prot = slot.protection();
    assert!(prot.page_aligned);
    assert!(prot.mapped_bytes >= 32);
    eprintln!("slot protection on this host: {prot:?}");
    assert!(slot.with(|b| b.iter().all(|&x| x == 0xA5)));
    slot.with_mut(|b| b[0] = 1);
    assert_eq!(slot.with(|b| b[0]), 1);
    let before = LeakAuditor::this_thread();
    let receipt = slot.shred();
    assert!(receipt.verified_zero);
    assert_eq!(receipt.wiped_bytes, prot.mapped_bytes);
    assert_eq!(LeakAuditor::this_thread(), before, "shred is not a leak");
}

#[test]
fn dropping_without_shred_is_counted() {
    let before = LeakAuditor::this_thread().count(ObligationKind::SecretSlot);
    {
        let _slot = SecretSlot::<16>::new(LockPolicy::BestEffort).unwrap();
    }
    assert_eq!(
        LeakAuditor::this_thread().count(ObligationKind::SecretSlot),
        before + 1
    );
    assert!(LeakAuditor::global().count(ObligationKind::SecretSlot) >= 1);
}

#[test]
fn zero_sized_slot_is_rejected() {
    assert_eq!(
        SecretSlot::<0>::new(LockPolicy::BestEffort).unwrap_err(),
        SlotError::ZeroSize
    );
}

#[test]
fn require_policy_reports_mlock_honestly() {
    match SecretSlot::<64>::new(LockPolicy::Require) {
        Ok(slot) => {
            assert_eq!(slot.protection().mlock, Protection::Applied);
            let _ = slot.shred();
        }
        Err(SlotError::MlockFailed { os_error }) => {
            eprintln!("mlock unavailable here (os error {os_error:?}); reported as a typed error");
        }
        Err(e) => panic!("unexpected {e}"),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn linux_applies_dontdump() {
    let slot = SecretSlot::<8>::new(LockPolicy::BestEffort).unwrap();
    assert_eq!(slot.protection().dontdump, Protection::Applied);
    let _ = slot.shred();
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
#[test]
fn dontdump_is_reported_unavailable_off_linux() {
    let slot = SecretSlot::<8>::new(LockPolicy::BestEffort).unwrap();
    assert_eq!(slot.protection().dontdump, Protection::Unavailable);
    let _ = slot.shred();
}

/// Exceed RLIMIT_MEMLOCK on purpose: lower the soft limit to one page in a
/// child process and require locking of a larger slot.
#[cfg(target_os = "linux")]
#[test]
fn rlimit_memlock_failure_is_a_typed_error() {
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("ulimit -l 0 2>/dev/null && exec \"$0\" --exact rlimit_child --nocapture")
        .arg(&exe)
        .env("VAULT_MEMLOCK_CHILD_RUN", "1")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&status.stdout);
    let err = String::from_utf8_lossy(&status.stderr);
    assert!(status.status.success(), "{out}\n{err}");
    assert!(out.contains("1 passed"), "{out}\n{err}");
    eprintln!("{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn rlimit_child() {
    if std::env::var_os("VAULT_MEMLOCK_CHILD_RUN").is_none() {
        return;
    }
    // CAP_IPC_LOCK (root) ignores RLIMIT_MEMLOCK, so locking may still work.
    match SecretSlot::<{ 64 * 1024 }>::new(LockPolicy::Require) {
        Err(SlotError::MlockFailed { os_error }) => {
            eprintln!("RLIMIT_MEMLOCK=0 refused mlock: os error {os_error:?}");
        }
        Ok(slot) => {
            // SAFETY: geteuid has no preconditions and cannot fail.
            #[allow(unsafe_code)]
            let euid = unsafe { libc::geteuid() };
            assert_eq!(
                euid, 0,
                "an unprivileged process locked memory past RLIMIT_MEMLOCK=0"
            );
            eprintln!("mlock succeeded despite RLIMIT_MEMLOCK=0: running as root (CAP_IPC_LOCK)");
            let _ = slot.shred();
        }
        Err(e) => panic!("unexpected {e}"),
    }
    let best = SecretSlot::<{ 64 * 1024 }>::new(LockPolicy::BestEffort).unwrap();
    eprintln!("best-effort protection: {:?}", best.protection());
    let _ = best.shred();
}
