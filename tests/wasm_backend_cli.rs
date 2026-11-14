//! `vesper build --target wasm32-wasip2`: the CLI writes a component that
//! Wasmtime 50 instantiates and runs.
use std::process::Command;
use wasmtime::{
    Config, Engine, Store,
    component::{Component, Linker, Val},
};

fn vesper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vesper-wasm-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn build_writes_a_runnable_component() {
    let out = temp("shapes.wasm");
    let status = vesper()
        .args([
            "build",
            "--target",
            "wasm32-wasip2",
            "tests/fixtures/wasm_backend/shapes.ves",
            "-o",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(String::from_utf8_lossy(&status.stdout).contains("wasm32-wasip2 component"));
    let bytes = std::fs::read(&out).unwrap();
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config).unwrap();
    let component = Component::new(&engine, &bytes).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Linker::<()>::new(&engine)
        .instantiate(&mut store, &component)
        .unwrap();
    let iface = instance
        .get_export_index(&mut store, None, vesper::wasm_backend::ENTRY_INTERFACE)
        .unwrap();
    let main = instance
        .get_export_index(&mut store, Some(&iface), "main")
        .unwrap();
    let main = instance.get_func(&mut store, main).unwrap();
    let mut results = [Val::Bool(false)];
    main.call(&mut store, &[], &mut results).unwrap();
    // The evaluator prints (12, 110) for this fixture.
    assert_eq!(results[0], Val::Tuple(vec![Val::S64(12), Val::S64(110)]));
    let run = vesper()
        .args(["run", "tests/fixtures/wasm_backend/shapes.ves"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "(12, 110)");
}

#[test]
fn unsupported_source_fails_with_a_typed_error() {
    let out = temp("remainder.wasm");
    let status = vesper()
        .args([
            "build",
            "--target",
            "wasm32-wasip2",
            "tests/fixtures/wasm_backend/remainder.ves",
            "-o",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(!status.status.success());
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(stderr.contains("unsupported FloatRemainder"), "{stderr}");
    assert!(!out.exists());
}

#[test]
fn native_only_flags_are_rejected_for_wasm() {
    let out = temp("tee.wasm");
    let status = vesper()
        .args([
            "build",
            "--target",
            "wasm32-wasip2",
            "--tee",
            "tests/fixtures/wasm_backend/shapes.ves",
            "-o",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("native targets only"));
}
