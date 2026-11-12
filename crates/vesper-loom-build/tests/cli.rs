//! Runs the `vesper-loom-build` executable on the committed example request.

use std::path::Path;
use std::process::{Command, Output};

const EXE: &str = env!("CARGO_BIN_EXE_vesper-loom-build");

fn example() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/codegen/request.json"
    ))
}

fn run(args: &[&str]) -> Output {
    Command::new(EXE).args(args).output().unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn example_request_misses_then_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let cache = cache.to_str().unwrap();
    let req = example().to_str().unwrap();
    let out1 = tmp.path().join("out1");
    let out2 = tmp.path().join("out2");

    let first = json(&run(&[
        "run",
        "--cache",
        cache,
        "--request",
        req,
        "--out",
        out1.to_str().unwrap(),
    ]));
    assert_eq!(first["version"], 1);
    assert_eq!(first["cache"], "miss");
    assert_eq!(first["executed"], true);
    let second = json(&run(&[
        "run",
        "--cache",
        cache,
        "--request",
        req,
        "--out",
        out2.to_str().unwrap(),
    ]));
    assert_eq!(second["cache"], "hit");
    assert_eq!(second["executed"], false);
    assert_eq!(first["key"], second["key"]);
    assert_eq!(first["manifest"], second["manifest"]);
    let generated = std::fs::read_to_string(out2.join("generated.vsp")).unwrap();
    assert!(generated.contains("struct Generated"));

    let key = run(&["key", "--cache", cache, "--request", req]);
    assert!(key.status.success());
    assert_eq!(
        String::from_utf8(key.stdout).unwrap().trim(),
        first["key"].as_str().unwrap()
    );

    // An existing destination is refused without --replace (exit 1)...
    let again = run(&[
        "run",
        "--cache",
        cache,
        "--request",
        req,
        "--out",
        out1.to_str().unwrap(),
    ]);
    assert_eq!(again.status.code(), Some(1));
    // ...and replaced with it.
    let replaced = json(&run(&[
        "run",
        "--cache",
        cache,
        "--request",
        req,
        "--out",
        out1.to_str().unwrap(),
        "--replace",
    ]));
    assert_eq!(replaced["cache"], "hit");

    let verify = json(&run(&[
        "run",
        "--cache",
        cache,
        "--request",
        req,
        "--out",
        tmp.path().join("out3").to_str().unwrap(),
        "--policy",
        "verify",
    ]));
    assert_eq!(verify["cache"], "verified");

    let sweep = json(&run(&["sweep", "--cache", cache, "--min-age-secs", "0"]));
    assert!(sweep["removed"].is_u64());
}

#[test]
fn usage_and_request_errors_have_distinct_exit_codes() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(run(&["frobnicate"]).status.code(), Some(2));
    assert_eq!(run(&["run", "--bogus"]).status.code(), Some(2));
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, r#"{"version": 7}"#).unwrap();
    let cache = tmp.path().join("cache");
    let out = run(&[
        "run",
        "--cache",
        cache.to_str().unwrap(),
        "--request",
        bad.to_str().unwrap(),
        "--out",
        tmp.path().join("o").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("UnsupportedVersion"));

    let identity = json(&run(&["identity"]));
    assert_eq!(identity["wit_package"], "vesper:sandbox@0.1.0");
    assert!(identity["engine_dependencies"].is_string());
}
