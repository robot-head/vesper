//! Gated KVM execution (§37.5). The cloud host and CI runners have no
//! `/dev/kvm`, so this records `Unavailable` there; the guest run is asserted
//! only where KVM actually opened. `VESPER_REQUIRE_KVM=1` makes absence fail.
use vesper_aether::host::*;
use vesper_aether::slot_plan::KVM_MEM_READONLY;

#[test]
fn kvm_guest_writes_port_or_reports_unavailable() {
    match probe_kvm() {
        Err(reason) => {
            assert!(
                std::env::var_os("VESPER_REQUIRE_KVM").is_none(),
                "VESPER_REQUIRE_KVM set but KVM unavailable: {reason:?}"
            );
            if cfg!(all(target_os = "linux", target_arch = "x86_64"))
                && !std::path::Path::new("/dev/kvm").exists()
            {
                assert_eq!(reason, Unavailable::DeviceAbsent { path: "/dev/kvm" });
            }
            if cfg!(not(target_os = "linux")) {
                assert_eq!(reason, Unavailable::UnsupportedPlatform);
            }
            assert_eq!(
                run_port_write_guest(0x2a).unwrap_err(),
                KvmError::Unavailable(reason.clone())
            );
            eprintln!("KVM UNAVAILABLE, no guest was run: {reason:?}");
        }
        Ok(limits) => {
            eprintln!("KVM available: {limits:?}");
            let run = run_port_write_guest(0x2a).expect("guest run");
            assert_eq!(run.port, 0x10);
            assert_eq!(run.value, 0x2a);
            assert_eq!(run.plan.regions().len(), 2);
            assert_eq!(run.plan.regions()[1].flags, KVM_MEM_READONLY);
            eprintln!("KVM guest ran: {run:?}");
        }
    }
}

#[test]
fn kvm_pvh_boot_or_reports_unavailable() {
    match probe_kvm() {
        Err(reason) => {
            assert!(
                std::env::var_os("VESPER_REQUIRE_KVM").is_none(),
                "VESPER_REQUIRE_KVM set but KVM unavailable: {reason:?}"
            );
            assert_eq!(
                run_pvh_guest().unwrap_err(),
                KvmError::Unavailable(reason.clone())
            );
            eprintln!("KVM UNAVAILABLE, no PVH guest was booted: {reason:?}");
        }
        Ok(_) => {
            let run = run_pvh_guest().expect("PVH guest run");
            // hvm_start_info.magic (0x336ec578) low byte, then version 1.
            assert_eq!(run.port_bytes, vec![0x78, 0x01]);
            eprintln!("KVM PVH guest ran: {run:?}");
        }
    }
}
