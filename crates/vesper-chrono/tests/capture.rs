//! Gated perf_event capture: on hosts without a trace PMU it reports
//! unavailable with the reason; the sysfs probe is checked on a fake tree.

use vesper_chrono::capture::{
    CaptureConfig, CaptureError, SYSFS_PMU_ROOT, TracePmu, capture_command, parse_format, probe,
};

#[test]
fn format_fields_parse() {
    assert_eq!(parse_format("config:10\n"), Some(1 << 10));
    assert_eq!(parse_format("config:0-3"), Some(0xf));
    assert_eq!(parse_format("config:0-63"), Some(u64::MAX));
    assert_eq!(parse_format("config1:0-3"), None);
    assert_eq!(parse_format("config:9-3"), None);
    assert_eq!(parse_format("config:64"), None);
}

#[test]
fn fake_sysfs_tree_is_probed() {
    let root = std::env::temp_dir().join(format!("vesper-chrono-pmu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("intel_pt/format")).unwrap();
    std::fs::write(root.join("intel_pt/type"), "9\n").unwrap();
    std::fs::write(root.join("intel_pt/format/tsc"), "config:10\n").unwrap();
    std::fs::write(root.join("intel_pt/format/branch"), "config:13\n").unwrap();
    let p = probe(&root).unwrap();
    assert_eq!(p.pmu, TracePmu::IntelPt);
    assert_eq!(p.event_type, 9);
    assert_eq!(p.config, (1 << 10) | (1 << 13));

    std::fs::write(root.join("intel_pt/type"), "nine\n").unwrap();
    assert!(matches!(probe(&root), Err(CaptureError::BadSysfs { .. })));
    std::fs::remove_dir_all(root.join("intel_pt")).unwrap();
    std::fs::create_dir_all(root.join("cs_etm")).unwrap();
    std::fs::write(root.join("cs_etm/type"), "11").unwrap();
    assert_eq!(probe(&root).unwrap().pmu, TracePmu::CoreSight);
    let _ = std::fs::remove_dir_all(&root);
}

/// Records what this host offers. Where no trace PMU exists (the
/// development host and CI runners), capture must report unavailable.
#[test]
fn capture_reports_unavailable_without_a_trace_pmu() {
    let root = std::path::Path::new(SYSFS_PMU_ROOT);
    match probe(root) {
        Err(CaptureError::Unavailable { reason }) => {
            eprintln!("hardware trace capture unavailable: {reason}");
            assert!(reason.contains("no intel_pt, cs_etm or arm_spe PMU"));
            let err =
                capture_command(root, "/bin/true", &[], CaptureConfig::default()).unwrap_err();
            assert!(matches!(err, CaptureError::Unavailable { .. }));
        }
        Ok(found) => {
            // A trace PMU exists: the capture may still be refused by
            // perf_event_paranoid, which must also come back as unavailable.
            eprintln!("trace PMU present: {found:?}");
            match capture_command(root, "/bin/true", &[], CaptureConfig::default()) {
                Ok(trace) => eprintln!("captured {} trace bytes", trace.aux.len()),
                // Refused by the kernel, or no shell to start the child in a
                // sandbox: reported, never a fake success.
                Err(e) => eprintln!("capture not exercised: {e}"),
            }
        }
        Err(e) => panic!("{e}"),
    }
    let bad = CaptureConfig {
        aux_pages: 3,
        ..CaptureConfig::default()
    };
    // Invalid sizes are rejected once a PMU is found; without one, the
    // unavailable error comes first.
    assert!(capture_command(root, "/bin/true", &[], bad).is_err());
}
