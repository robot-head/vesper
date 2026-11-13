//! Integration B (#136): `vesper replay` over Chrono `.vchrono` bundles.
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_chrono::{Bundle, BundleKey, FlightRecorder, HopKind, RecorderConfig, RedactionPolicy};

const KEY: [u8; 32] = [7; 32];

fn key_hex() -> String {
    KEY.iter().map(|b| format!("{b:02x}")).collect()
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-replay-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Seven records: an effect, two writes to place 0xAB (seq 2 and 5), an
/// obligation minted at 3 and discharged at 6 (or leaked), a snapshot, a
/// digest record.
fn bundle(leak: bool) -> Vec<u8> {
    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    rec.record_effect(1, 1, 3, 4, b"hello").unwrap();
    rec.record_inout_write(1, 2, 0xAB, 9).unwrap();
    let id = rec.mint_obligation(1, 3, 1, None).unwrap();
    rec.capture_snapshot(4, b"img").unwrap();
    rec.record_inout_write(2, 5, 0xAB, 10).unwrap();
    let kind = if leak {
        HopKind::Leak
    } else {
        HopKind::UnpackDischarge
    };
    rec.record_obligation_hop(1, 6, id, kind, 2).unwrap();
    rec.record_effect_digest(1, 7, 3, 4, b"digest me").unwrap();
    Bundle::capture(&rec, &RedactionPolicy::new(), b"meta")
        .seal(&BundleKey::new(KEY))
        .unwrap()
}

#[test]
fn replay_verifies_replays_and_answers_queries() {
    let temp = Temp::new();
    let file = temp.write("run.vchrono", &bundle(false));
    let key = key_hex();
    let o = vesper(&[
        "replay",
        &file,
        "--key",
        &key,
        "--timeline",
        "--watch",
        "0xab",
        "--lineage",
        "1",
    ]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("bundle verified: records 1..8 (7 retained, 0 evicted), 1 snapshot(s)"),
        "{t}"
    );
    assert!(
        t.contains("replayed 7 record(s) from the initial state; transcript "),
        "{t}"
    );
    assert!(t.contains("inout-write"), "{t}");
    assert!(t.contains(" digest "), "{t}");
    assert!(
        t.contains("watch 0xab: last write before seq 8 at seq 5"),
        "{t}"
    );
    assert!(t.contains("fiber 2"), "{t}");
    assert!(
        t.contains("obligations: 0 open, 0 leaked, lineage complete"),
        "{t}"
    );
    assert!(t.contains("lineage 1: obligation 1"), "{t}");
    assert!(t.contains("UnpackDischarge"), "{t}");
    // Reverse watch from an earlier cursor finds the earlier write.
    let o = vesper(&[
        "replay", &file, "--key", &key, "--watch", "0xab", "--before", "5",
    ]);
    assert!(
        text(&o).contains("last write before seq 5 at seq 2"),
        "{}",
        text(&o)
    );
    // Replay is deterministic.
    let a = vesper(&["replay", &file, "--key", &key]);
    let b = vesper(&["replay", &file, "--key", &key]);
    assert_eq!(a.stdout, b.stdout);
    // The key may come from a file, raw or hex.
    let raw = temp.write("key.bin", &KEY);
    assert!(
        vesper(&["replay", &file, "--key-file", &raw])
            .status
            .success()
    );
    let hex = temp.write("key.txt", format!("{key}\n").as_bytes());
    assert!(
        vesper(&["replay", &file, "--key-file", &hex])
            .status
            .success()
    );
}

#[test]
fn replay_rejects_wrong_keys_tampering_and_leaks() {
    let temp = Temp::new();
    let mut bytes = bundle(false);
    let file = temp.write("run.vchrono", &bytes);
    let wrong: String = "00".repeat(32);
    let o = vesper(&["replay", &file, "--key", &wrong]);
    assert!(!o.status.success());
    assert!(text(&o).contains("Bundle(Authentication)"), "{}", text(&o));
    let middle = bytes.len() / 2;
    bytes[middle] ^= 1;
    let tampered = temp.write("tampered.vchrono", &bytes);
    let o = vesper(&["replay", &tampered, "--key", &key_hex()]);
    assert!(!o.status.success());
    assert!(text(&o).contains("Bundle(Authentication)"), "{}", text(&o));
    let leaked = temp.write("leak.vchrono", &bundle(true));
    let o = vesper(&["replay", &leaked, "--key", &key_hex()]);
    let t = text(&o);
    assert!(!o.status.success(), "{t}");
    assert!(t.contains("obligations: 0 open, 1 leaked"), "{t}");
    assert!(t.contains("replay: 1 obligation(s) leaked: [1]"), "{t}");
}

#[test]
fn replay_reports_typed_usage_errors() {
    let temp = Temp::new();
    let file = temp.write("run.vchrono", &bundle(false));
    for (args, message) in [
        (vec!["replay"], "missing bundle"),
        (vec!["replay", file.as_str()], "replay requires --key"),
        (
            vec!["replay", file.as_str(), "--key", "abc"],
            "64 hex digits",
        ),
        (
            vec![
                "replay",
                file.as_str(),
                "--key",
                &key_hex(),
                "--before",
                "3",
            ],
            "--before requires --watch",
        ),
        (
            vec![
                "replay",
                file.as_str(),
                "--key",
                &key_hex(),
                "--watch",
                "0x99",
            ],
            "watch 0x99",
        ),
        (
            vec![
                "replay",
                file.as_str(),
                "--key",
                &key_hex(),
                "--lineage",
                "42",
            ],
            "lineage 42",
        ),
        (
            vec!["replay", file.as_str(), "--nope"],
            "unknown replay option",
        ),
    ] {
        let o = vesper(&args);
        assert!(!o.status.success(), "{args:?}: {}", text(&o));
        assert!(text(&o).contains(message), "{args:?}: {}", text(&o));
    }
}
