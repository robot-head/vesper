//! ConstTime::scope: DIT is set inside and restored after on AArch64 with
//! FEAT_DIT; everywhere else the scope reports the mode as unavailable.

use vesper_vault::{ConstTime, HardwareCtMode};

#[test]
fn scope_reports_and_restores_hardware_mode() {
    let before = ConstTime::dit_flag();
    let probe = ConstTime::probe();
    eprintln!("hardware constant-time mode on this host: {probe:?}");
    let (inside, mode) = ConstTime::scope(|ct| (ConstTime::dit_flag(), ct.mode()));
    assert_eq!(mode, inside.1);
    match mode {
        HardwareCtMode::ArmDit { was_set } => {
            assert_eq!(Some(was_set), before);
            assert_eq!(inside.0, Some(true), "DIT set inside the scope");
            assert_eq!(
                ConstTime::dit_flag(),
                before,
                "DIT restored after the scope"
            );
            assert!(mode.is_active());
        }
        HardwareCtMode::Unavailable { reason } => {
            assert!(!reason.is_empty());
            assert_eq!(inside.0, None);
            assert!(!mode.is_active());
        }
    }
}

#[test]
fn nested_scopes_restore_the_outer_state() {
    let before = ConstTime::dit_flag();
    let (_, outer) = ConstTime::scope(|_| {
        let (_, inner) = ConstTime::scope(|_| ConstTime::dit_flag());
        if let HardwareCtMode::ArmDit { was_set } = inner {
            assert!(was_set, "outer scope already set DIT");
            assert_eq!(
                ConstTime::dit_flag(),
                Some(true),
                "inner exit keeps outer DIT"
            );
        }
    });
    let _ = outer;
    assert_eq!(ConstTime::dit_flag(), before);
}

#[test]
fn scope_restores_on_panic() {
    let before = ConstTime::dit_flag();
    let r = std::panic::catch_unwind(|| ConstTime::scope(|_| panic!("boom")));
    assert!(r.is_err());
    assert_eq!(ConstTime::dit_flag(), before);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn x86_doitm_is_reported_unavailable() {
    assert!(
        matches!(ConstTime::probe(), HardwareCtMode::Unavailable { reason } if reason.contains("DOITM"))
    );
}

/// Every Apple-silicon core (M1 onward, Armv8.4+) implements FEAT_DIT, so on
/// the macOS AArch64 runner the scope must really toggle PSTATE.DIT.
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[test]
fn apple_silicon_sets_dit_inside_the_scope() {
    assert!(ConstTime::probe().is_active(), "{:?}", ConstTime::probe());
    let before = ConstTime::dit_flag();
    let (inside, _) = ConstTime::scope(|_| ConstTime::dit_flag());
    assert_eq!(inside, Some(true));
    assert_eq!(ConstTime::dit_flag(), before);
}
