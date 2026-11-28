//! The pinned CLIs compose and run the fixtures that publish objects
//! (`WAC_BIN` and `WASMTIME_BIN`, as `aspect ci install-wac` and
//! `install-wasmtime` lay them out; without them the test says so and
//! passes): Stage 2 builds each one, `wac plug` links the Cranelift
//! component into it, and the wasmtime CLI runs the result with no other
//! host (its `wasi:cli/run` succeeds).
mod support;

use std::process::Command;
use support::*;

/// (fixture, entry) of each fixture that publishes an object.
const PUBLISHING: &[(&str, &str)] = &[
    ("checked.ves", "checked_x64_linux"),
    ("checked.ves", "checked_aarch64_linux"),
    ("checked.ves", "checked_aarch64_macos"),
    ("checked.ves", "checked_x64_windows"),
    ("checked.ves", "checked_none"),
    ("checked.ves", "checked_speed"),
    ("pilot.ves", "pilot"),
    ("sh5_extras.ves", "extras"),
];

#[test]
fn composed_fixtures_run_under_the_wasmtime_cli() {
    let Some(stage2) = stage2::stage2() else {
        return;
    };
    let (Some(wac), Some(wasmtime), Some(cranelift), Some((triple, _))) = (
        std::env::var_os("WAC_BIN"),
        std::env::var_os("WASMTIME_BIN"),
        stage2::component_path(),
        host_target(),
    ) else {
        eprintln!("WAC_BIN or WASMTIME_BIN missing; composition unavailable");
        return;
    };
    let temp = Temp::new("compose");
    let mut failures = Vec::new();
    for (file, entry) in PUBLISHING {
        let guest = temp.0.join(format!("{entry}.wasm"));
        let composed = temp.0.join(format!("{entry}.composed.wasm"));
        let source = format!("{}\n{}", fixture(file), stage2::entry_main(entry));
        let built = stage2
            .build(&source)
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        std::fs::write(&guest, built).unwrap();
        let plug = Command::new(&wac)
            .arg("plug")
            .arg(&guest)
            .arg("--plug")
            .arg(&cranelift)
            .arg("-o")
            .arg(&composed)
            .output()
            .unwrap();
        if !plug.status.success() {
            failures.push(format!(
                "{entry}: wac plug failed: {}",
                String::from_utf8_lossy(&plug.stderr)
            ));
            continue;
        }
        let run = Command::new(&wasmtime)
            .arg("run")
            .args(["--env", &format!("VESPER_CRANELIFT_HOST={triple}")])
            .arg(&composed)
            .output()
            .unwrap();
        if !run.status.success() {
            failures.push(format!(
                "{entry}: the wasmtime CLI failed ({}): {}",
                run.status,
                String::from_utf8_lossy(&run.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
