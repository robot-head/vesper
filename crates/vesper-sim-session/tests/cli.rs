//! The runnable command records and replays the fixture, and rejects replays
//! whose scenario changed.
use std::process::Command;

fn fixture(name: &str) -> String {
    format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn sim(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_vesper-sim"))
        .args(args)
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn record_then_replay_through_the_command() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("vesper-sim-cli");
    std::fs::create_dir_all(&dir).unwrap();
    let recording = dir.join("pipeline.recording.json");
    let recording = recording.to_str().unwrap();
    let (source, scenario) = (fixture("pipeline.ves"), fixture("pipeline.json"));

    let (ok, stdout, stderr) = sim(&["record", &source, &scenario, recording]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("virtual time: 14 ns"), "{stdout}");
    assert!(stdout.contains(r#""received":1234"#), "{stdout}");

    let (ok, stdout, stderr) = sim(&["replay", &source, &scenario, recording]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("replay matched the recording"), "{stdout}");

    let changed = dir.join("changed.json");
    let text = std::fs::read_to_string(&scenario)
        .unwrap()
        .replace(r#""seed": 7"#, r#""seed": 8"#);
    std::fs::write(&changed, text).unwrap();
    let (ok, _, stderr) = sim(&["replay", &source, changed.to_str().unwrap(), recording]);
    assert!(!ok);
    assert!(stderr.contains("scenario_sha256"), "{stderr}");

    let (ok, _, stderr) = sim(&["replay", &source, &scenario]);
    assert!(!ok);
    assert!(stderr.contains("usage"), "{stderr}");
}
